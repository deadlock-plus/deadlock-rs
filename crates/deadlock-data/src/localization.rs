//! Reading Deadlock's localisation files.
//!
//! The game ships these as loose UTF-8 files under
//! `game/citadel/resource/localization/<bundle>/<stem>_<language>.txt`, in Valve's `KeyValues`
//! format. They are *not* inside the VPK archives, so no unpacking is needed - which
//! makes them the cheapest offline source of display names, and they are always exactly
//! matched to the installed build.
//!
//! Hero names live in the `citadel_gc_hero_names` bundle, keyed by the hero's internal
//! class name:
//!
//! ```text
//! "Tokens"
//! {
//!     "hero_inferno:n"    "Infernus"
//!     "hero_gigawatt:n"   "Seven"
//! }
//! ```
//!
//! The `:n` suffix is a grammatical variant marker; there are `:n`, `_search:n` and
//! `_sort:n` forms of most keys, and only the plain one is a display name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Bundle holding hero display names.
pub const HERO_NAMES_BUNDLE: &str = "citadel_gc_hero_names";

/// Accolade flavour names and descriptions.
///
/// The one bundle a retail install ships that is **not** flat: it sits under
/// `citadel_vdata/`, so the file is `citadel_vdata/accolades/accolades_<language>.txt`.
/// Its tokens are what `scripts/accolades.vdata_c` points at with `m_sFlavorName` and
/// `m_sDescription` - e.g. `Citadel_VData_accolades_ability_damage_FlavorName` is
/// "The Zapper".
pub const ACCOLADES_BUNDLE: &str = "citadel_vdata/accolades";

/// Every localisation bundle a retail install ships, as [`relative_path`] wants them.
///
/// [`relative_path`]: LocalizationFile::relative_path
///
/// Bundles are not interchangeable - a token lives in exactly one - so a consumer
/// resolving a token it did not choose has to sweep. This is that list, and
/// `every_shipped_bundle_offers_every_language` fails if a build adds or drops one.
///
/// `citadel_vdata` is deliberately absent: it is a container holding
/// [`ACCOLADES_BUNDLE`], not a bundle, and carries no files of its own.
pub const BUNDLES: &[&str] = &[
    "citadel_attributes",
    "citadel_gc",
    HERO_NAMES_BUNDLE,
    "citadel_gc_mod_names",
    "citadel_generated_vo",
    "citadel_heroes",
    "citadel_main",
    "citadel_mods",
    "citadel_patch_notes",
    ACCOLADES_BUNDLE,
];

/// A parsed localisation bundle: token -> localised string.
#[derive(Clone, Debug, Default)]
pub struct LocalizationFile {
    /// Language this was read for.
    pub language: String,
    /// Token to localised text.
    pub tokens: HashMap<String, String>,
}

impl LocalizationFile {
    /// Path of a bundle for a language, relative to the `citadel` directory.
    pub fn relative_path(bundle: &str, language: &str) -> PathBuf {
        // A bundle may sit under a directory rather than at the top: nine of the ten a
        // retail install ships are flat, and `citadel_vdata/accolades` is not. The file is
        // named for the bundle's last segment, so joining the whole string as the stem too
        // yields `citadel_vdata/accolades/citadel_vdata/accolades_english.txt`.
        let stem = bundle.rsplit('/').next().unwrap_or(bundle);
        PathBuf::from("resource")
            .join("localization")
            .join(bundle)
            .join(format!("{stem}_{language}.txt"))
    }

    /// Read and parse a bundle from an installed game.
    ///
    /// `citadel_dir` is the directory containing `resource/`, i.e.
    /// `.../Deadlock/game/citadel`.
    pub fn load(citadel_dir: impl AsRef<Path>, bundle: &str, language: &str) -> Result<Self> {
        let path = citadel_dir
            .as_ref()
            .join(Self::relative_path(bundle, language));
        let raw = std::fs::read(&path).map_err(|e| Error::Io {
            kind: e.kind(),
            path: path.clone(),
            source: e.to_string(),
        })?;
        let text = decode(&raw);
        Ok(Self::parse(&text, language))
    }

    /// Parse localisation text that has already been read.
    pub fn parse(text: &str, language: &str) -> Self {
        LocalizationFile {
            language: language.to_string(),
            tokens: parse_tokens(text),
        }
    }

    /// Look up a token, ignoring grammatical suffixes.
    pub fn get(&self, token: &str) -> Option<&str> {
        self.tokens.get(token).map(String::as_str)
    }

    /// Number of tokens parsed.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether nothing was parsed.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Decode a localisation file, honouring the byte-order marks Valve uses.
///
/// These files are UTF-8 with a BOM in the current build, but Valve has shipped
/// UTF-16LE in other titles, so both are handled rather than assumed.
fn decode(raw: &[u8]) -> String {
    match raw {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xFE, 0xFF, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// Pull `"key" "value"` pairs out of a `KeyValues` body.
///
/// Deliberately not a full KV parser: these files are one flat `Tokens` block, and a
/// targeted scan avoids a dependency and copes with the malformed lines that turn up in
/// community-translated bundles.
///
/// Keys are normalised by stripping the `:n`-style grammatical suffix; `_search` and
/// `_sort` variants are dropped, since they are collation helpers rather than names.
fn parse_tokens(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('{')
            || line.starts_with('}')
        {
            continue;
        }
        // "key" <ws> "value"
        let mut parts = line.split('"').skip(1);
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.nth(1) else { continue };
        // Some shipped strings carry padding inside the quotes - `"Weakening Headshot "` in the
        // retail mod-name bundle - which would make an install disagree with the vendored
        // snapshot. Keys get the same treatment so a padded one stays reachable by its
        // clean token.
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            continue;
        }
        let key = key.split(':').next().unwrap_or(key);
        if key.ends_with("_search") || key.ends_with("_sort") {
            continue;
        }
        // A later duplicate wins, matching the game's own last-write behaviour.
        out.insert(key.to_string(), value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
\"lang\"
{
\t\"Language\"\t\t\"english\"
\t\"Tokens\"
\t{
\t\t// Hero names (tokens match the hero name directly)
\t\t\"hero_inferno:n\"\t\t\"Infernus\"
\t\t\"hero_inferno_search:n\"\t\t\"Infernus\"
\t\t\"hero_inferno_sort:n\"\t\t\"Infernus\"
\t\t\"hero_gigawatt:n\"\t\t\"Seven\"
\t\t\"hero_krill:n\"\t\t\"Mo & Krill\"
\t}
}
";

    #[test]
    fn parses_hero_tokens() {
        let f = LocalizationFile::parse(SAMPLE, "english");
        assert_eq!(f.get("hero_inferno"), Some("Infernus"));
        assert_eq!(f.get("hero_gigawatt"), Some("Seven"));
        assert_eq!(f.get("hero_krill"), Some("Mo & Krill"));
    }

    #[test]
    fn drops_collation_variants() {
        let f = LocalizationFile::parse(SAMPLE, "english");
        assert!(f.get("hero_inferno_search").is_none());
        assert!(f.get("hero_inferno_sort").is_none());
        assert_eq!(f.len(), 4);
    }

    #[test]
    fn ignores_structure_and_comments() {
        let f = LocalizationFile::parse(SAMPLE, "english");
        assert!(!f.tokens.contains_key("lang"));
        assert!(!f.tokens.contains_key("Tokens"));
        assert_eq!(f.get("Language"), Some("english"));
    }

    #[test]
    fn survives_malformed_lines() {
        let f = LocalizationFile::parse("garbage\n\"unterminated\n\"a\"\t\"b\"\n", "english");
        assert_eq!(f.get("a"), Some("b"));
    }

    /// `citadel_gc_mod_names_english.txt` in the retail build ships
    /// `"upgrade_headshot_booster2"` with the value `"Weakening Headshot "` - a trailing
    /// space inside the quotes. Left in, an install and the vendored snapshot disagree
    /// about the same item's name, so whether a user sees the padding depends on whether
    /// an install was found.
    #[test]
    fn whitespace_padding_around_shipped_values_is_trimmed() {
        const PADDED: &str = r#"
	"upgrade_headshot_booster2"		"Weakening Headshot "
	" hero_krill "		" Mo & Krill "
"#;

        let f = LocalizationFile::parse(PADDED, "english");
        assert_eq!(
            f.get("upgrade_headshot_booster2"),
            Some("Weakening Headshot")
        );
        assert_eq!(f.get("hero_krill"), Some("Mo & Krill"));
    }

    #[test]
    fn decodes_each_bom() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi"), "hi");
        let utf16le: Vec<u8> = [0xFF, 0xFE]
            .iter()
            .copied()
            .chain("hi".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode(&utf16le), "hi");
        assert_eq!(decode(b"plain"), "plain");
    }

    /// A bundle nested under a directory keeps its own name as the file stem.
    ///
    /// Nine of the ten bundles a retail install ships sit flat at
    /// `localization/<bundle>/<bundle>_<lang>.txt`, which is the shape this module's docs
    /// describe. The accolade bundle does not: it is at
    /// `localization/citadel_vdata/accolades/accolades_english.txt`. Joining the whole
    /// bundle string as one directory *and* as the file stem produces
    /// `citadel_vdata/accolades/citadel_vdata/accolades_english.txt`, which does not exist.
    #[test]
    fn a_nested_bundle_takes_its_file_name_from_its_last_segment() {
        let p = LocalizationFile::relative_path("citadel_vdata/accolades", "english");
        assert_eq!(
            p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"),
            "resource/localization/citadel_vdata/accolades/accolades_english.txt"
        );
        let flat = LocalizationFile::relative_path("citadel_main", "english");
        assert_eq!(
            flat.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
            "resource/localization/citadel_main/citadel_main_english.txt"
        );
    }

    /// Every bundle a retail install ships offers exactly [`crate::LANGUAGES`].
    ///
    /// [`crate::LANGUAGES`]'s own docs used to rest on one bundle,
    /// `citadel_gc_hero_names`. That is the weaker claim: [`LocalizationFile::load`] takes
    /// an arbitrary bundle, so a caller passing a language the list vouches for and a
    /// bundle that happens not to ship it gets an IO error out of a lookup that had no way
    /// to know better. This asserts the set is the same for all of them, which is what the
    /// public list actually promises.
    ///
    /// It also pins [`BUNDLES`]: a build that adds one is a build where any consumer
    /// sweeping every bundle silently stops covering the new one.
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn every_shipped_bundle_offers_every_language() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let root = Path::new(&dir).join("resource").join("localization");

        let mut found = Vec::new();
        let mut walk = vec![(root.clone(), String::new())];
        while let Some((path, prefix)) = walk.pop() {
            let entries = std::fs::read_dir(&path).expect("read localization dir");
            let mut has_txt = false;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    let child = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    };
                    walk.push((entry.path(), child));
                } else if name.ends_with(".txt") {
                    has_txt = true;
                }
            }
            if has_txt {
                found.push(prefix);
            }
        }
        found.sort();

        let mut expected: Vec<String> = BUNDLES.iter().map(|b| (*b).to_owned()).collect();
        expected.sort();
        assert_eq!(found, expected, "the shipped bundle list moved");

        let mut wrong = Vec::new();
        for bundle in &found {
            let stem = bundle.rsplit('/').next().unwrap_or(bundle);
            let mut langs: Vec<String> = std::fs::read_dir(root.join(bundle))
                .expect("read bundle dir")
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_prefix(&format!("{stem}_"))?
                        .strip_suffix(".txt")
                        .map(str::to_owned)
                })
                .collect();
            langs.sort();
            if langs != crate::LANGUAGES {
                wrong.push(format!("{bundle}: {langs:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "bundles disagreeing with LANGUAGES: {wrong:#?}"
        );
    }

    #[test]
    fn builds_the_expected_relative_path() {
        let p = LocalizationFile::relative_path(HERO_NAMES_BUNDLE, "english");
        assert_eq!(
            p.to_string_lossy().replace('\\', "/"),
            "resource/localization/citadel_gc_hero_names/citadel_gc_hero_names_english.txt"
        );
    }
}

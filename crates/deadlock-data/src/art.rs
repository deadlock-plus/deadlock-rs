//! Hero portraits and rank badges read from the installed game.
//!
//! The textures are `vtex_c` resources in `pak01_dir.vpk`. [`ArtArchive`] opens the archive
//! once, finds an asset by [`Art`] request, decodes the first mip (undoing the game's
//! scaled `YCoCg` encoding where a texture uses it) and hands back RGBA pixels or PNG bytes.
//!
//! Where the result is stored is the caller's decision. Art changes with game patches, so a
//! caller caching the PNGs should key the cache by game build; [`ArtArchive::png`] returns
//! bytes and [`ArtArchive::write_png`] a finished file so either fits.
//!
//! Only BGRA8888, RGBA8888, BC1 and BC3 textures decode. Anything else fails *that asset*
//! with [`Error::Parse`] and leaves the archive usable, so a caller can fall back to
//! another source for it.
//!
//! The art is Valve's. Decode it for the user's own machine; do not ship or commit it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use source2::ext::{ResourceTexture, VpkResourceKv3};
use source2::kv3;
use source2::resource::{BlockKind, Resource};
use source2::vpk::Vpk;

use crate::error::{Error, Result};
use crate::install::ARCHIVE;
use crate::vdata::{HEROES_PATH, vpk_error};

const HERO_DIR: &str = "panorama/images/heroes";
const BADGE_DIR: &str = "panorama/images/ranked/badges";
const CLASS_PREFIX: &str = "hero_";
const IMAGES_PREFIX: &str = "file://{images}/";

/// The `heroes.vdata_c` field that names each image's source file.
const HERO_FIELDS: [(HeroArtKind, &str); 6] = [
    (HeroArtKind::Sm, "m_strIconImageSmall"),
    (HeroArtKind::Card, "m_strIconHeroCard"),
    (HeroArtKind::CardCritical, "m_strIconHeroCardCritical"),
    (HeroArtKind::CardGloat, "m_strIconHeroCardGloat"),
    (HeroArtKind::Mm, "m_strMinimapImage"),
    (HeroArtKind::Vertical, "m_strTopBarVertical"),
];

/// The file-name stem a hero's art usually has: the class name without its `hero_` prefix,
/// `hero_inferno` -> `inferno`. A name without the prefix is returned unchanged.
///
/// This is a guess, and a wrong one for heroes that kept a development codename in their
/// files (`hero_atlas` has `bull_*`). [`ArtArchive::path`] consults the game's own table
/// and uses this only for heroes the table does not cover.
pub fn hero_art_key(class_name: &str) -> &str {
    class_name.strip_prefix(CLASS_PREFIX).unwrap_or(class_name)
}

/// Which of a hero's images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HeroArtKind {
    /// Small square icon.
    Sm,
    /// Portrait card.
    Card,
    /// Portrait card, low-health variant.
    CardCritical,
    /// Portrait card, gloating variant.
    CardGloat,
    /// Minimap icon.
    Mm,
    /// Tall portrait.
    Vertical,
    /// Wide background.
    Background,
}

impl HeroArtKind {
    /// Every kind.
    pub const ALL: [HeroArtKind; 7] = [
        HeroArtKind::Sm,
        HeroArtKind::Card,
        HeroArtKind::CardCritical,
        HeroArtKind::CardGloat,
        HeroArtKind::Mm,
        HeroArtKind::Vertical,
        HeroArtKind::Background,
    ];

    fn file_suffix(self) -> &'static str {
        match self {
            HeroArtKind::Sm => "sm",
            HeroArtKind::Card => "card",
            HeroArtKind::CardCritical => "card_critical",
            HeroArtKind::CardGloat => "card_gloat",
            HeroArtKind::Mm => "mm",
            HeroArtKind::Vertical => "vertical",
            HeroArtKind::Background => "bg",
        }
    }
}

/// Which of a rank tier's badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RankArtKind {
    /// Full-colour badge.
    Large,
    /// Chalk-style badge.
    Chalk,
}

impl RankArtKind {
    /// Every kind.
    pub const ALL: [RankArtKind; 2] = [RankArtKind::Large, RankArtKind::Chalk];

    fn file_suffix(self) -> &'static str {
        match self {
            RankArtKind::Large => "lg",
            RankArtKind::Chalk => "chalk",
        }
    }
}

/// One image in the game's files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Art<'a> {
    /// A hero image, by class name (`hero_inferno`).
    Hero {
        /// The hero's class name; the `hero_` prefix is removed to form the file name.
        class_name: &'a str,
        /// Which image.
        kind: HeroArtKind,
    },
    /// A rank badge.
    Rank {
        /// The tier number in the file name (`rank05_lg` is 5).
        tier: u8,
        /// Which badge.
        kind: RankArtKind,
    },
}

impl<'a> Art<'a> {
    /// A hero image.
    pub fn hero(class_name: &'a str, kind: HeroArtKind) -> Self {
        Art::Hero { class_name, kind }
    }

    /// A rank badge.
    pub fn rank(tier: u8, kind: RankArtKind) -> Self {
        Art::Rank { tier, kind }
    }

    /// The texture's path by the naming rule alone: `hero_art_key` for heroes, the tier
    /// number for ranks.
    ///
    /// Right for ranks. For heroes it is the fallback; [`ArtArchive::path`] gives the
    /// game's actual answer.
    pub fn rule_path(&self) -> String {
        match *self {
            Art::Hero {
                class_name,
                kind: HeroArtKind::Background,
            } => {
                format!(
                    "{HERO_DIR}/backgrounds/{}_bg_psd.vtex_c",
                    hero_art_key(class_name)
                )
            }
            Art::Hero { class_name, kind } => {
                format!(
                    "{HERO_DIR}/{}_{}_psd.vtex_c",
                    hero_art_key(class_name),
                    kind.file_suffix()
                )
            }
            Art::Rank { tier, kind } => {
                format!(
                    "{BADGE_DIR}/rank{tier:02}_{}_psd.vtex_c",
                    kind.file_suffix()
                )
            }
        }
    }
}

/// A decoded image: 8-bit RGBA, row-major, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Image {
    /// Encode as a PNG.
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] if the encoder rejects the image.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let png_error = |e: png::EncodingError| Error::Parse(format!("png: {e}"));
        let mut writer = encoder.write_header().map_err(png_error)?;
        writer.write_image_data(&self.rgba).map_err(png_error)?;
        writer.finish().map_err(png_error)?;
        Ok(out)
    }
}

/// An opened `pak01_dir.vpk`, ready to hand out art.
///
/// Opening parses the archive's whole directory, which is the expensive part; keep one
/// around for a batch of requests rather than opening per image.
#[derive(Debug)]
pub struct ArtArchive {
    vpk: Vpk,
    archive: PathBuf,
    hero_paths: HashMap<(String, HeroArtKind), String>,
}

impl ArtArchive {
    /// Open the archive in `citadel_dir`, i.e. `.../Deadlock/game/citadel`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] with kind `NotFound` when there is no archive there, and
    /// [`Error::Parse`] when it is not a readable VPK.
    pub fn open(citadel_dir: impl AsRef<Path>) -> Result<Self> {
        let archive = citadel_dir.as_ref().join(ARCHIVE);
        let vpk = Vpk::open(&archive).map_err(|e| vpk_error(e, &archive))?;
        // An archive whose hero table cannot be read still serves ranks and every hero
        // whose files follow the naming rule, so this is not an open failure.
        let hero_paths = vpk
            .read_resource_kv3(HEROES_PATH, BlockKind::DATA)
            .map(|doc| hero_paths(&doc))
            .unwrap_or_default();
        Ok(ArtArchive {
            vpk,
            archive,
            hero_paths,
        })
    }

    /// Where the texture for this request lives inside the archive.
    ///
    /// Heroes are looked up in the game's own hero table, which names each image; only a
    /// hero or image the table does not mention falls back to [`Art::rule_path`]. The
    /// answer is a path whether or not the file exists; see [`ArtArchive::contains`].
    pub fn path(&self, art: &Art<'_>) -> String {
        if let Art::Hero { class_name, kind } = *art
            && let Some(path) = self.hero_paths.get(&(class_name.to_owned(), kind))
        {
            return path.clone();
        }
        art.rule_path()
    }

    /// Whether the archive holds a texture for this request.
    ///
    /// A `true` answer does not promise it decodes: the format may be unsupported.
    pub fn contains(&self, art: &Art<'_>) -> bool {
        self.vpk.find(&self.path(art)).is_some()
    }

    /// Decode the full-size image.
    ///
    /// # Errors
    ///
    /// [`Error::AssetNotFound`] when the archive has no such texture, [`Error::Parse`] when
    /// it is malformed or in a format that is not decoded, [`Error::Io`] on a read failure.
    pub fn image(&self, art: &Art<'_>) -> Result<Image> {
        let path = self.path(art);
        let entry = self
            .vpk
            .find(&path)
            .ok_or_else(|| Error::AssetNotFound(path.clone()))?;
        let bytes = self
            .vpk
            .read(entry)
            .map_err(|e| vpk_error(e, &self.archive))?;
        let resource = Resource::parse(&bytes).map_err(|e| Error::Parse(format!("{path}: {e}")))?;
        let image = resource
            .decode_texture_rgba8(0)
            .map_err(|e| Error::Parse(format!("{path}: {e}")))?;
        Ok(Image {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
        })
    }

    /// Decode and encode as PNG bytes.
    ///
    /// # Errors
    ///
    /// As [`ArtArchive::image`], plus encoder failures from [`Image::to_png`].
    pub fn png(&self, art: &Art<'_>) -> Result<Vec<u8>> {
        self.image(art)?.to_png()
    }

    /// Decode and write a PNG to `dest`, creating missing parent directories.
    ///
    /// The file appears whole or not at all: it is written beside `dest` and renamed, so a
    /// caller treating existence as "cached" never sees a partial image. Nothing is written
    /// when decoding fails.
    ///
    /// # Errors
    ///
    /// As [`ArtArchive::png`], plus [`Error::Io`] for the write.
    pub fn write_png(&self, art: &Art<'_>, dest: impl AsRef<Path>) -> Result<()> {
        let dest = dest.as_ref();
        let bytes = self.png(art)?;
        let io = |e: std::io::Error| Error::Io {
            path: dest.to_path_buf(),
            kind: e.kind(),
            source: e.to_string(),
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut partial = dest.as_os_str().to_owned();
        partial.push(".part");
        let partial = PathBuf::from(partial);
        std::fs::write(&partial, bytes)
            .and_then(|()| std::fs::rename(&partial, dest))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&partial);
            })
            .map_err(io)
    }
}

/// Every image the hero table names, keyed by class name.
///
/// A reference like `file://{images}/heroes/bull_sm.psd` compiles to
/// `panorama/images/heroes/bull_sm_psd.vtex_c`. The backgrounds are not in the table; their
/// file name reuses the stem of the card, which is the one image every hero lists.
fn hero_paths(doc: &kv3::Document) -> HashMap<(String, HeroArtKind), String> {
    let mut out = HashMap::new();
    let Some(root) = doc.root.as_object() else {
        return out;
    };
    for (class_name, value) in root.iter() {
        let Some(hero) = value.as_object() else {
            continue;
        };
        for (kind, field) in HERO_FIELDS {
            let Some(path) = hero
                .get(field)
                .and_then(kv3::Value::as_str)
                .and_then(compiled_path)
            else {
                continue;
            };
            if kind == HeroArtKind::Card
                && let Some(stem) = path.strip_suffix("_card_psd.vtex_c")
            {
                let stem = stem.rsplit('/').next().unwrap_or(stem);
                out.insert(
                    (class_name.to_owned(), HeroArtKind::Background),
                    format!("{HERO_DIR}/backgrounds/{stem}_bg_psd.vtex_c"),
                );
            }
            out.insert((class_name.to_owned(), kind), path);
        }
    }
    out
}

/// `file://{images}/heroes/bull_sm.psd` -> `panorama/images/heroes/bull_sm_psd.vtex_c`.
fn compiled_path(reference: &str) -> Option<String> {
    let relative = reference.strip_prefix(IMAGES_PREFIX)?;
    let (stem, extension) = relative.rsplit_once('.')?;
    Some(format!("panorama/images/{stem}_{extension}.vtex_c"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use source2::resource::{Block, BlockKind, Resource};
    use source2::vpk::Vpk;

    use super::*;
    use crate::error::Error;
    use crate::install::ARCHIVE;

    const FORMAT_BGRA8888: u8 = 28;
    const FORMAT_BC7: u8 = 20;

    fn vtex(width: u16, height: u16, format: u8, pixels: Vec<u8>) -> Vec<u8> {
        let mut head = Vec::new();
        head.extend_from_slice(&1u16.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes());
        for r in [0.0f32; 4] {
            head.extend_from_slice(&r.to_le_bytes());
        }
        head.extend_from_slice(&width.to_le_bytes());
        head.extend_from_slice(&height.to_le_bytes());
        head.extend_from_slice(&1u16.to_le_bytes());
        head.push(format);
        head.push(1);
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&8u32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        Resource::new()
            .with_block(Block::new(BlockKind::DATA, head))
            .with_trailing(pixels)
            .to_bytes()
            .unwrap()
    }

    /// Two-by-two BGRA image whose RGBA reading differs from its stored order in every
    /// channel position, so a swapped or dropped channel shows.
    fn bgra_2x2() -> (Vec<u8>, Vec<u8>) {
        let bgra = vec![
            10, 20, 30, 255, //
            40, 50, 60, 128, //
            70, 80, 90, 0, //
            100, 110, 120, 7,
        ];
        let rgba = vec![
            30, 20, 10, 255, //
            60, 50, 40, 128, //
            90, 80, 70, 0, //
            120, 110, 100, 7,
        ];
        (bgra, rgba)
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str, files: &[(String, Vec<u8>)]) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("deadlock-data-art-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let mut pack = Vpk::new(1);
            for (path, bytes) in files {
                pack.add(path.clone(), bytes.clone()).unwrap();
            }
            pack.write(dir.join(ARCHIVE)).unwrap();
            Fixture(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn the_art_key_is_the_class_name_without_the_hero_prefix() {
        assert_eq!(hero_art_key("hero_inferno"), "inferno");
        assert_eq!(hero_art_key("hero_ratking"), "ratking");
        assert_eq!(hero_art_key("inferno"), "inferno");
    }

    #[cfg(feature = "bundled")]
    #[test]
    fn every_catalogued_hero_has_a_distinct_nonempty_art_key() {
        let heroes = crate::HeroCatalog::bundled();
        assert!(heroes.len() > 50);
        let mut keys = std::collections::HashSet::new();
        for hero in heroes.all() {
            assert!(
                hero.class_name.starts_with("hero_"),
                "{} breaks the hero_ rule",
                hero.class_name
            );
            let key = hero_art_key(&hero.class_name);
            assert!(!key.is_empty());
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{key}"
            );
            assert!(keys.insert(key.to_owned()), "duplicate key {key}");
        }
    }

    #[test]
    fn paths_follow_the_games_layout() {
        let hero = |kind| Art::hero("hero_inferno", kind).rule_path();
        assert_eq!(
            hero(HeroArtKind::Sm),
            "panorama/images/heroes/inferno_sm_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::Card),
            "panorama/images/heroes/inferno_card_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::CardCritical),
            "panorama/images/heroes/inferno_card_critical_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::CardGloat),
            "panorama/images/heroes/inferno_card_gloat_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::Mm),
            "panorama/images/heroes/inferno_mm_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::Vertical),
            "panorama/images/heroes/inferno_vertical_psd.vtex_c"
        );
        assert_eq!(
            hero(HeroArtKind::Background),
            "panorama/images/heroes/backgrounds/inferno_bg_psd.vtex_c"
        );
        assert_eq!(
            Art::rank(5, RankArtKind::Large).rule_path(),
            "panorama/images/ranked/badges/rank05_lg_psd.vtex_c"
        );
        assert_eq!(
            Art::rank(11, RankArtKind::Chalk).rule_path(),
            "panorama/images/ranked/badges/rank11_chalk_psd.vtex_c"
        );
    }

    #[test]
    fn a_stored_texture_decodes_to_rgba_and_encodes_to_png() {
        let (bgra, rgba) = bgra_2x2();
        let art = Art::hero("hero_inferno", HeroArtKind::Sm);
        let fx = Fixture::new(
            "decode",
            &[(art.rule_path(), vtex(2, 2, FORMAT_BGRA8888, bgra))],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();

        let image = archive.image(&art).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.rgba, rgba);

        let (w, h, pixels) = decode_png(&archive.png(&art).unwrap());
        assert_eq!((w, h), (2, 2));
        assert_eq!(pixels, rgba);
    }

    #[test]
    fn written_png_matches_the_direct_decode() {
        let (bgra, _) = bgra_2x2();
        let art = Art::rank(3, RankArtKind::Large);
        let fx = Fixture::new(
            "write",
            &[(art.rule_path(), vtex(2, 2, FORMAT_BGRA8888, bgra))],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();
        let dest = fx.path().join("out").join("nested").join("rank.png");

        archive.write_png(&art, &dest).unwrap();

        let direct = archive.image(&art).unwrap();
        let (w, h, pixels) = decode_png(&std::fs::read(&dest).unwrap());
        assert_eq!((w, h, pixels), (direct.width, direct.height, direct.rgba));
        let leftovers = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .flatten()
            .count();
        assert_eq!(leftovers, 1, "temp file left behind");
    }

    #[test]
    fn a_failed_write_leaves_no_destination() {
        let art = Art::hero("hero_inferno", HeroArtKind::Sm);
        let fx = Fixture::new(
            "nodest",
            &[(art.rule_path(), vtex(4, 4, FORMAT_BC7, vec![0; 16]))],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();
        let dest = fx.path().join("sm.png");
        assert!(archive.write_png(&art, &dest).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn missing_art_is_reported_and_not_available() {
        let present = Art::hero("hero_inferno", HeroArtKind::Sm);
        let absent = Art::hero("hero_inferno", HeroArtKind::Card);
        let (bgra, _) = bgra_2x2();
        let fx = Fixture::new(
            "missing",
            &[(present.rule_path(), vtex(2, 2, FORMAT_BGRA8888, bgra))],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();

        assert!(archive.contains(&present));
        assert!(!archive.contains(&absent));
        assert_eq!(
            archive.png(&absent),
            Err(Error::AssetNotFound(absent.rule_path()))
        );
    }

    #[test]
    fn an_unsupported_format_fails_that_asset_only() {
        let bad = Art::hero("hero_bad", HeroArtKind::Sm);
        let good = Art::hero("hero_good", HeroArtKind::Sm);
        let (bgra, rgba) = bgra_2x2();
        let fx = Fixture::new(
            "unsupported",
            &[
                (bad.rule_path(), vtex(4, 4, FORMAT_BC7, vec![0; 16])),
                (good.rule_path(), vtex(2, 2, FORMAT_BGRA8888, bgra)),
            ],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();

        assert!(archive.contains(&bad));
        assert!(matches!(archive.png(&bad), Err(Error::Parse(m)) if m.contains("bad_sm_psd")));
        assert_eq!(archive.image(&good).unwrap().rgba, rgba);
    }

    #[test]
    fn a_directory_without_the_archive_is_an_io_not_found() {
        let dir = std::env::temp_dir().join("deadlock-data-art-empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            ArtArchive::open(&dir),
            Err(Error::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    const VDATA: &str = r#"<!-- kv3 encoding:text:version{e21c7f3c-8a33-41c5-9977-a76d3a32aa0d} format:generic:version{7412167c-06e9-4698-aff2-e63eb59037e7} -->
{
    generic_data_type = "CitadelHeroData"
    hero_atlas =
    {
        m_strIconImageSmall = "file://{images}/heroes/bull_sm.psd"
        m_strMinimapImage = "file://{images}/heroes/bull_mm.psd"
        m_strIconHeroCard = "file://{images}/heroes/bull_card.psd"
        m_strTopBarVertical = "file://{images}/heroes/bull_vertical.psd"
        m_strIconHeroCardCritical = "file://{images}/heroes/bull_card_critical.psd"
        m_strIconHeroCardGloat = "file://{images}/heroes/bull_card_gloat.psd"
    }
    hero_hornet =
    {
        m_strIconImageSmall = "file://{images}/heroes/hornet_sm.png"
        m_strIconHeroCard = "file://{images}/heroes/hornet_card.psd"
    }
    hero_bare =
    {
        m_HeroID = 9
    }
}
"#;

    fn vdata_resource() -> Vec<u8> {
        use source2::ext::ResourceKv3;
        let doc = source2::kv3::Document::from_text(VDATA).unwrap();
        let mut resource = Resource::new();
        resource
            .put_kv3(BlockKind::DATA, &source2::kv3::Document::new(doc.root))
            .unwrap();
        resource.to_bytes().unwrap()
    }

    #[test]
    fn the_heroes_table_names_each_heros_files() {
        let doc = source2::kv3::Document::from_text(VDATA).unwrap();
        let paths = hero_paths(&doc);

        let get = |class: &str, kind| paths.get(&(class.to_owned(), kind)).map(String::as_str);
        assert_eq!(
            get("hero_atlas", HeroArtKind::Sm),
            Some("panorama/images/heroes/bull_sm_psd.vtex_c")
        );
        assert_eq!(
            get("hero_atlas", HeroArtKind::CardGloat),
            Some("panorama/images/heroes/bull_card_gloat_psd.vtex_c")
        );
        assert_eq!(
            get("hero_atlas", HeroArtKind::Background),
            Some("panorama/images/heroes/backgrounds/bull_bg_psd.vtex_c")
        );
        assert_eq!(
            get("hero_hornet", HeroArtKind::Sm),
            Some("panorama/images/heroes/hornet_sm_png.vtex_c")
        );
        assert_eq!(get("hero_hornet", HeroArtKind::Mm), None);
        assert_eq!(get("hero_bare", HeroArtKind::Card), None);
    }

    #[test]
    fn the_games_table_beats_the_class_name_rule() {
        let (bgra, rgba) = bgra_2x2();
        let by_table = "panorama/images/heroes/bull_card_psd.vtex_c".to_owned();
        let by_rule = Art::hero("hero_atlas", HeroArtKind::Card).rule_path();
        assert_ne!(by_table, by_rule);
        let fx = Fixture::new(
            "table",
            &[
                ("scripts/heroes.vdata_c".to_owned(), vdata_resource()),
                (by_table.clone(), vtex(2, 2, FORMAT_BGRA8888, bgra)),
            ],
        );
        let archive = ArtArchive::open(fx.path()).unwrap();
        let art = Art::hero("hero_atlas", HeroArtKind::Card);

        assert_eq!(archive.path(&art), by_table);
        assert!(archive.contains(&art));
        assert_eq!(archive.image(&art).unwrap().rgba, rgba);
        // Not in the table: the rule still answers, and reports the file missing.
        let unlisted = Art::hero("hero_other", HeroArtKind::Card);
        assert_eq!(archive.path(&unlisted), unlisted.rule_path());
        assert!(!archive.contains(&unlisted));
    }

    /// `DEADLOCK_CITADEL_DIR=".../Deadlock/game/citadel" cargo test -p deadlock-data --features art -- --ignored`
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn every_playable_hero_has_card_and_sm_art_in_the_archive() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let archive = ArtArchive::open(&dir).expect("archive");
        let roster = crate::vdata::hero_roster(&dir).expect("roster");

        let mut failures = Vec::new();
        let mut checked = 0;
        for hero in roster.iter().filter(|h| h.is_playable()) {
            for kind in [HeroArtKind::Sm, HeroArtKind::Card] {
                let art = Art::hero(&hero.class_name, kind);
                checked += 1;
                match archive.image(&art) {
                    Ok(_) => {}
                    // Some textures are stored as embedded PNG, which the decoder leaves
                    // out; that is the documented per-asset failure, not a broken lookup.
                    Err(Error::Parse(m)) if m.contains("unsupported pixel format") => {
                        println!("undecodable: {m}");
                    }
                    Err(e) => failures.push(format!("{}: {e}", archive.path(&art))),
                }
            }
        }
        assert!(checked > 40, "only {checked} checked");
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn a_real_png_round_trips_through_the_png_crate() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let archive = ArtArchive::open(&dir).expect("archive");
        let out = std::env::temp_dir().join("deadlock-data-art-real");
        let _ = std::fs::remove_dir_all(&out);

        for art in [
            Art::hero("hero_inferno", HeroArtKind::Card),
            Art::hero("hero_inferno", HeroArtKind::Sm),
            Art::rank(5, RankArtKind::Large),
        ] {
            let dest = out.join(format!("{}.png", archive.path(&art).replace('/', "_")));
            archive.write_png(&art, &dest).expect("write");
            let direct = archive.image(&art).expect("decode");
            let (w, h, pixels) = decode_png(&std::fs::read(&dest).unwrap());
            assert_eq!(
                (w, h),
                (direct.width, direct.height),
                "{}",
                archive.path(&art)
            );
            assert_eq!(pixels, direct.rgba, "{}", archive.path(&art));
            assert!(direct.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0));
        }
        let _ = std::fs::remove_dir_all(&out);
    }
}

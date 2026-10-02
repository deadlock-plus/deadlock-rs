//! Ranked tier names, and the two disagreeing tables the game ships them in.
//!
//! [`deadlock_core::RankBadge`] packs a tier and a subrank into one number and
//! deliberately does not name the tier. This is where the names live, because they are
//! asset data: the ladder has gained tiers across patches, and - more awkwardly - the
//! game currently ships **two** tables for it.
//!
//! # Two families, and why that matters
//!
//! `citadel_main` carries both, one after the other, labelled in the file's own comments
//! as "Old Rank Names" and "New Rank Names":
//!
//! ```text
//! // Old Rank Names
//! "Citadel_ranks_rank5"   "Ritualist"
//! "Citadel_ranks_rank6"   "Emissary"
//!
//! // New Rank Names
//! "Citadel_ranks_5"       "Mystic"
//! "Citadel_ranks_6"       "Ritualist"
//! "Citadel_ranks_7"       "Emissary"
//! ```
//!
//! They agree on tiers 0-2 and 8-11 and disagree on 3-7. The dangerous part is that they
//! do not merely disagree, they **collide**: `Ritualist` and `Emissary` each name a real
//! tier in both families, one tier apart. A lookup against the wrong family therefore
//! returns a plausible name for the wrong rank rather than failing, and a rank *name* on
//! its own does not identify a tier at all.
//!
//! [`RankFamily::Current`] is the default because it is what the running client renders,
//! which is what a user comparing a companion tool against their own screen expects.
//! [`RankFamily::Legacy`] stays reachable because historical screenshots, VOD overlays
//! and third-party datasets were captured under it. Neither is inferable from the other,
//! so the family is part of every call rather than a mode set once and forgotten.
//!
//! # Source
//!
//! The names come from the `citadel_main` localisation bundle - the same loose
//! `KeyValues` files [`crate::localization`] already reads, in any of [`crate::LANGUAGES`].
//!
//! This is a plain lookup rather than a fourth [`crate::Catalogs`] facet: a rank table
//! has no roster to key against and no art, deadlock-api.com publishes no rank endpoint
//! to degrade to, and there is nothing to merge - a table is read whole or not at all.
//! The per-facet resolver exists to stop a names-only source blanking a roster, and here
//! there is no second facet for it to protect.

use deadlock_core::RankBadge;

use crate::source::Source;

/// Localisation bundle carrying both rank tables.
///
/// A general-purpose bundle rather than a rank-specific one; the tokens sit among the
/// rest of the ranked-matchmaking UI strings.
pub const RANKS_BUNDLE: &str = "citadel_main";

/// English names for the table the current client shows, as of this crate's build.
///
/// Not gated behind the `bundled` feature. That feature exists to keep about 205 KB of
/// vendored JSON out of a binary that does not want it; twelve short strings are not
/// worth a build configuration in which rank names cannot be looked up at all.
const CURRENT: &[&str] = &[
    "Obscurus",
    "Initiate",
    "Seeker",
    "Acolyte",
    "Sentinel",
    "Mystic",
    "Ritualist",
    "Emissary",
    "Oracle",
    "Phantom",
    "Ascendant",
    "Eternus",
];

/// English names for the superseded table, as of this crate's build.
const LEGACY: &[&str] = &[
    "Obscurus",
    "Initiate",
    "Seeker",
    "Alchemist",
    "Arcanist",
    "Ritualist",
    "Emissary",
    "Archon",
    "Oracle",
    "Phantom",
    "Ascendant",
    "Eternus",
];

/// Which of the two shipped rank-name tables to read.
///
/// See the [module docs](self): the two collide, so this is never a cosmetic choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RankFamily {
    /// What the running client renders today: `Citadel_ranks_<tier>`.
    #[default]
    Current,
    /// The superseded table: `Citadel_ranks_rank<tier>`. Still shipped, and still what
    /// older captures and third-party datasets used.
    Legacy,
}

impl RankFamily {
    /// Both families, so a caller can report a name under each rather than pick one.
    pub const ALL: &'static [RankFamily] = &[RankFamily::Current, RankFamily::Legacy];

    /// Token prefix a tier number is appended to.
    pub const fn token_prefix(self) -> &'static str {
        match self {
            RankFamily::Current => "Citadel_ranks_",
            RankFamily::Legacy => "Citadel_ranks_rank",
        }
    }

    /// The localisation token naming a tier in this family.
    pub fn token(self, tier: u32) -> String {
        format!("{}{tier}", self.token_prefix())
    }

    /// A short lowercase label, for logs and status lines.
    pub const fn as_str(self) -> &'static str {
        match self {
            RankFamily::Current => "current",
            RankFamily::Legacy => "legacy",
        }
    }
}

impl std::fmt::Display for RankFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rank tier names, holding both shipped families.
///
/// Both are kept rather than one being chosen at load time, because which one a caller
/// wants depends on what it is comparing against - see the [module docs](self).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankNames {
    language: String,
    source: Source,
    current: Vec<String>,
    legacy: Vec<String>,
}

impl RankNames {
    /// The English tables vendored into this crate.
    ///
    /// Always answers, and needs neither an install nor the network. Like every vendored
    /// table it is a snapshot; [`RankNames::load`] is what tracks the installed build.
    pub fn bundled() -> Self {
        RankNames {
            language: crate::DEFAULT_LANGUAGE.to_string(),
            source: Source::Bundled,
            current: CURRENT.iter().map(|s| (*s).to_string()).collect(),
            legacy: LEGACY.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// Read both tables out of an installed game.
    ///
    /// `citadel_dir` is the directory containing `resource/`, i.e.
    /// `.../Deadlock/game/citadel`. Names come back in `language`, which must be one of
    /// [`crate::LANGUAGES`].
    #[cfg(feature = "client")]
    pub fn load(citadel_dir: impl AsRef<std::path::Path>, language: &str) -> crate::Result<Self> {
        let loc = crate::localization::LocalizationFile::load(citadel_dir, RANKS_BUNDLE, language)?;
        Ok(Self::from_localization(&loc, Source::Client))
    }

    /// Read both tables out of an installed game, falling back to the vendored English
    /// tables when no install can be found or read.
    ///
    /// [`RankNames::source`] reports which of the two happened, so a caller that cares
    /// about staleness can still tell.
    #[cfg(feature = "client")]
    pub fn for_game(language: &str) -> Self {
        crate::install::find_citadel_dir()
            .and_then(|dir| Self::load(dir, language).ok())
            .unwrap_or_else(Self::bundled)
    }

    /// Pull both tables out of a bundle that has already been read.
    ///
    /// `source` is recorded as given; it is what [`RankNames::source`] later reports.
    #[cfg(feature = "client")]
    pub fn from_localization(loc: &crate::localization::LocalizationFile, source: Source) -> Self {
        RankNames {
            language: loc.language.clone(),
            source,
            current: read_family(loc, RankFamily::Current),
            legacy: read_family(loc, RankFamily::Legacy),
        }
    }

    /// Which source these names came from.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Language the names are in.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Every name in a family, indexed by tier.
    pub fn names(&self, family: RankFamily) -> &[String] {
        match family {
            RankFamily::Current => &self.current,
            RankFamily::Legacy => &self.legacy,
        }
    }

    /// How many tiers a family names.
    ///
    /// Read from the table rather than fixed at twelve: the ladder has grown before, and
    /// an install that ships a thirteenth tier should report thirteen without a rebuild.
    pub fn tier_count(&self, family: RankFamily) -> usize {
        self.names(family).len()
    }

    /// The name of a tier, in [`RankFamily::Current`].
    pub fn tier_name(&self, tier: u32) -> Option<&str> {
        self.tier_name_in(RankFamily::default(), tier)
    }

    /// The name of a tier, in a named family.
    ///
    /// `None` for a tier past the end of the table this was built from.
    pub fn tier_name_in(&self, family: RankFamily, tier: u32) -> Option<&str> {
        let tier = usize::try_from(tier).ok()?;
        self.names(family).get(tier).map(String::as_str)
    }

    /// The tier name for a badge, in [`RankFamily::Current`].
    pub fn badge_name(&self, badge: RankBadge) -> Option<&str> {
        self.badge_name_in(RankFamily::default(), badge)
    }

    /// The tier name for a badge, in a named family.
    ///
    /// An unranked badge has no name: zero means "no rank assigned" rather than the
    /// lowest tier, so naming it after tier zero would report a rank the player has not
    /// been given.
    pub fn badge_name_in(&self, family: RankFamily, badge: RankBadge) -> Option<&str> {
        if !badge.is_ranked() {
            return None;
        }
        self.tier_name_in(family, badge.tier())
    }
}

impl Default for RankNames {
    fn default() -> Self {
        Self::bundled()
    }
}

/// Collect one family's tiers from a bundle, upwards from zero.
///
/// Stops at the first absent tier rather than scanning a fixed range, so a build that
/// adds a tier is picked up without a code change and a partial translation cannot leave
/// a hole that shifts every later name onto the wrong tier.
#[cfg(feature = "client")]
fn read_family(loc: &crate::localization::LocalizationFile, family: RankFamily) -> Vec<String> {
    let mut out = Vec::new();
    for tier in 0.. {
        let Some(name) = loc.get(&family.token(tier)) else {
            break;
        };
        out.push(name.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table the current client shows, transcribed from a retail install's
    /// `citadel_main_english.txt`.
    #[test]
    fn the_current_family_names_every_tier_the_ladder_publishes() {
        let n = RankNames::bundled();
        let expected = [
            "Obscurus",
            "Initiate",
            "Seeker",
            "Acolyte",
            "Sentinel",
            "Mystic",
            "Ritualist",
            "Emissary",
            "Oracle",
            "Phantom",
            "Ascendant",
            "Eternus",
        ];
        for (tier, want) in expected.iter().enumerate() {
            assert_eq!(n.tier_name(tier as u32), Some(*want), "tier {tier}");
        }
        assert_eq!(n.tier_count(RankFamily::Current), expected.len());
    }

    /// The superseded table, kept because it is what older screenshots and third-party
    /// data show.
    #[test]
    fn the_legacy_family_names_the_same_ladder_differently() {
        let n = RankNames::bundled();
        let expected = [
            "Obscurus",
            "Initiate",
            "Seeker",
            "Alchemist",
            "Arcanist",
            "Ritualist",
            "Emissary",
            "Archon",
            "Oracle",
            "Phantom",
            "Ascendant",
            "Eternus",
        ];
        for (tier, want) in expected.iter().enumerate() {
            assert_eq!(
                n.tier_name_in(RankFamily::Legacy, tier as u32),
                Some(*want),
                "tier {tier}"
            );
        }
    }

    /// The reason a name may never be treated as an identifier for a tier: the two
    /// families do not merely differ, they reuse each other's names one tier apart.
    /// Reading `Ritualist` off a screenshot and mapping it back to a tier is ambiguous
    /// without knowing which family produced it.
    #[test]
    fn the_two_families_give_the_same_name_to_different_tiers() {
        let n = RankNames::bundled();
        assert_eq!(n.tier_name_in(RankFamily::Legacy, 5), Some("Ritualist"));
        assert_eq!(n.tier_name_in(RankFamily::Current, 6), Some("Ritualist"));
        assert_eq!(n.tier_name_in(RankFamily::Legacy, 6), Some("Emissary"));
        assert_eq!(n.tier_name_in(RankFamily::Current, 7), Some("Emissary"));

        for tier in [0, 1, 2, 8, 9, 10, 11] {
            assert_eq!(
                n.tier_name_in(RankFamily::Current, tier),
                n.tier_name_in(RankFamily::Legacy, tier),
                "tier {tier}"
            );
        }
        for tier in 3..=7 {
            assert_ne!(
                n.tier_name_in(RankFamily::Current, tier),
                n.tier_name_in(RankFamily::Legacy, tier),
                "tier {tier}"
            );
        }
    }

    /// Badge zero is unranked rather than tier zero, so it must not borrow the lowest
    /// tier's name - which a bare `badge / 10` would hand it.
    #[test]
    fn an_unranked_badge_has_no_tier_name() {
        let n = RankNames::bundled();
        assert!(!RankBadge(0).is_ranked());
        assert_eq!(n.badge_name(RankBadge(0)), None);
        assert_eq!(n.badge_name_in(RankFamily::Legacy, RankBadge(0)), None);
        assert_eq!(n.tier_name(0), Some("Obscurus"));
        assert_eq!(n.badge_name(RankBadge(1)), Some("Obscurus"));
        assert_eq!(n.badge_name(RankBadge(6)), Some("Obscurus"));
    }

    /// Ground truth from a live party: badges 93 and 96 are both tier nine.
    #[test]
    fn a_badge_names_the_tier_it_unpacks_to() {
        let n = RankNames::bundled();
        assert_eq!(n.badge_name(RankBadge(93)), Some("Phantom"));
        assert_eq!(n.badge_name(RankBadge(96)), Some("Phantom"));
        assert_eq!(n.badge_name(RankBadge(115)), Some("Eternus"));
        assert_eq!(
            n.badge_name_in(RankFamily::Legacy, RankBadge(73)),
            Some("Archon")
        );
        assert_eq!(n.badge_name(RankBadge(73)), Some("Emissary"));
    }

    /// The ladder has gained tiers before and may again, so a tier past the end of the
    /// installed table is a miss rather than a panic.
    #[test]
    fn a_tier_past_the_end_of_the_table_has_no_name() {
        let n = RankNames::bundled();
        assert_eq!(n.tier_name(12), None);
        assert_eq!(n.tier_name(u32::MAX), None);
        assert_eq!(n.badge_name(RankBadge(129)), None);
        assert_eq!(n.badge_name(RankBadge(u32::MAX)), None);
    }

    #[test]
    fn tokens_are_built_the_way_the_bundle_spells_them() {
        assert_eq!(RankFamily::Current.token(7), "Citadel_ranks_7");
        assert_eq!(RankFamily::Legacy.token(7), "Citadel_ranks_rank7");
        assert_eq!(RankFamily::Current.token(11), "Citadel_ranks_11");
        assert_eq!(RankFamily::default(), RankFamily::Current);
    }

    #[cfg(feature = "client")]
    mod client {
        use super::*;
        use crate::localization::LocalizationFile;

        const SAMPLE: &str = "\
\"lang\"
{
\t\"Tokens\"
\t{
\t\t// Old Rank Names
\t\t\"Citadel_ranks_rank0\"\t\t\"Obscurus\"
\t\t\"Citadel_ranks_rank1\"\t\t\"Initiate\"
\t\t\"Citadel_ranks_rank2\"\t\t\"Seeker\"

\t\t// New Rank Names
\t\t\"Citadel_ranks_0\"\t\t\"Obskurus\"
\t\t\"Citadel_ranks_1\"\t\t\"Eingeweihter\"
\t\t\"Citadel_ranks_2\"\t\t\"Sucher\"
\t\t\"Citadel_ranks_3\"\t\t\"Akolyth\"
\t\t\"Citadel_rank_material_0\"\t\t\"\"
\t}
}
";

        /// A localised bundle answers in its own language, and each family is read from
        /// its own token prefix rather than one being derived from the other.
        #[test]
        fn a_parsed_bundle_fills_both_families_independently() {
            let n = RankNames::from_localization(
                &LocalizationFile::parse(SAMPLE, "german"),
                Source::Client,
            );
            assert_eq!(n.language(), "german");
            assert_eq!(n.source(), Source::Client);
            assert_eq!(n.tier_name(3), Some("Akolyth"));
            assert_eq!(n.tier_name_in(RankFamily::Legacy, 2), Some("Seeker"));
        }

        /// Tiers are read upwards until one is missing, so a bundle that gains a tier is
        /// picked up without a code change and a truncated one does not invent entries.
        #[test]
        fn reading_a_family_stops_at_the_first_absent_tier() {
            let n = RankNames::from_localization(
                &LocalizationFile::parse(SAMPLE, "german"),
                Source::Client,
            );
            assert_eq!(n.tier_count(RankFamily::Current), 4);
            assert_eq!(n.tier_count(RankFamily::Legacy), 3);
            assert_eq!(n.tier_name(4), None);
            assert_eq!(n.tier_name_in(RankFamily::Legacy, 3), None);
        }

        /// A bundle with no rank tokens at all leaves both families empty rather than
        /// silently keeping the vendored ones, so `source` cannot claim an install
        /// supplied names it did not have.
        #[test]
        fn a_bundle_without_rank_tokens_yields_no_names() {
            let n = RankNames::from_localization(
                &LocalizationFile::parse("\"a\"\t\"b\"\n", "english"),
                Source::Client,
            );
            assert_eq!(n.tier_count(RankFamily::Current), 0);
            assert_eq!(n.tier_count(RankFamily::Legacy), 0);
            assert_eq!(n.badge_name(RankBadge(93)), None);
        }

        /// Against the retail install. Ignored by default; run it with
        /// `DEADLOCK_CITADEL_DIR=".../Deadlock/game/citadel" cargo test -- --ignored`.
        #[test]
        #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
        fn a_real_install_still_ships_both_families() {
            let dir = std::env::var(crate::install::ENV_OVERRIDE)
                .expect("set DEADLOCK_CITADEL_DIR to the citadel directory");
            let n = RankNames::load(&dir, crate::DEFAULT_LANGUAGE).expect("read citadel_main");
            assert_eq!(n.source(), Source::Client);

            let bundled = RankNames::bundled();
            assert_eq!(
                n.names(RankFamily::Current),
                bundled.names(RankFamily::Current),
                "the vendored current table has gone stale"
            );
            assert_eq!(
                n.names(RankFamily::Legacy),
                bundled.names(RankFamily::Legacy),
                "the vendored legacy table has gone stale"
            );
        }

        /// The install must still carry both prefixes; if Valve ever drops one, the
        /// family this crate defaults to is worth revisiting.
        #[test]
        #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
        fn a_real_install_disagrees_with_itself_exactly_where_expected() {
            let dir = std::env::var(crate::install::ENV_OVERRIDE)
                .expect("set DEADLOCK_CITADEL_DIR to the citadel directory");
            let n = RankNames::load(&dir, crate::DEFAULT_LANGUAGE).expect("read citadel_main");
            let disagree: Vec<u32> = (0..n.tier_count(RankFamily::Current) as u32)
                .filter(|&t| n.tier_name(t) != n.tier_name_in(RankFamily::Legacy, t))
                .collect();
            assert_eq!(disagree, vec![3, 4, 5, 6, 7]);
        }
    }
}

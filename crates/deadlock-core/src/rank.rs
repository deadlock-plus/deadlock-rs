//! Ranked badges.
//!
//! Split from the state enums because a badge is a packed number, not a state, and the
//! module it lived in was named after a Rust construct rather than anything in the game.

/// A packed ranked badge.
///
/// Reported by `CSOCitadelParty.RankedScores::rank_display_badge` for party members and by
/// `PlayerDataGlobal_t::m_unPackedRank` for players in a match.
///
/// The badge packs a tier and a subrank into one number as `tier * 10 + subrank`, which is
/// the convention deadlock-api.com decodes it with. Two live party members read 93 and 96,
/// which fits that shape; it has not been verified against a rank whose subrank is known
/// independently, so treat the split as the documented convention rather than as measured.
///
/// Tier *names* are deliberately not baked in, and the game vindicates that: the
/// `citadel_main` localisation bundle ships **two** tables, `Citadel_ranks_rank<tier>`
/// and `Citadel_ranks_<tier>`, which disagree at tiers 3 through 7 and reuse each other's
/// names one tier apart - `Ritualist` and `Emissary` each name a real tier under both.
/// Freezing either here would have named the wrong rank within a patch. The tables belong
/// with the rest of the asset data: `deadlock_data::RankNames`, keyed by [`RankBadge::tier`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct RankBadge(pub u32);

impl RankBadge {
    /// The raw packed value.
    pub fn get(&self) -> u32 {
        self.0
    }

    /// The tier, e.g. `9` for badge 93.
    pub fn tier(&self) -> u32 {
        self.0 / 10
    }

    /// The subrank within the tier, `0..=6`, e.g. `3` for badge 93.
    pub fn subrank(&self) -> u32 {
        self.0 % 10
    }

    /// Whether a rank has been assigned at all.
    ///
    /// Zero means unranked rather than the lowest tier, so it is worth distinguishing.
    pub fn is_ranked(&self) -> bool {
        self.0 > 0
    }
}

impl std::fmt::Display for RankBadge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.is_ranked() {
            return write!(f, "unranked");
        }
        write!(f, "{}-{}", self.tier(), self.subrank())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth from a live party: two members read 93 and 96.
    #[test]
    fn a_badge_splits_into_tier_and_subrank() {
        let a = RankBadge(93);
        assert_eq!((a.tier(), a.subrank()), (9, 3));
        assert_eq!(a.to_string(), "9-3");
        let b = RankBadge(96);
        assert_eq!((b.tier(), b.subrank()), (9, 6));
        assert!(a < b, "badges order by tier then subrank");
    }

    /// Zero is unranked, not tier zero, and must not render as a rank.
    #[test]
    fn an_unranked_badge_is_distinguishable() {
        let none = RankBadge(0);
        assert!(!none.is_ranked());
        assert_eq!(none.to_string(), "unranked");
        assert!(RankBadge(1).is_ranked());
    }
}

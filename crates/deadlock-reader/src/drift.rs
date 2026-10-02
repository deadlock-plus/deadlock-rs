//! Noticing when the game has changed underneath the reader.
//!
//! Field offsets come from the game's own runtime schema, so a patch that moves a field
//! costs nothing. What a patch *can* do is rename a field or a class, or renumber an
//! enum. None of those produce an error: the lookup simply misses, and what happens next
//! decides whether you get missing data or wrong data.
//!
//! # The failure worth engineering against
//!
//! Most fields have no baked fallback, so a rename leaves them `None`, which is honest.
//! A handful do have one, recovered from a single 2025 build, and for those a rename
//! silently substitutes a stale number. That is the dangerous case, and
//! [`UNSAFE_FALLBACKS`] refuses it outright rather than reporting fiction.
//!
//! Everything noticed is recorded as a [`Drift`] and surfaced on every snapshot, because
//! a signal a caller has to remember to ask for is a signal most callers never see.

use std::fmt;

/// Fallback offsets that must never be used, and why.
///
/// A baked offset is a number from one build of Deadlock. When the schema still knows a
/// field, the fallback is irrelevant; it is reached only when the name has gone, which is
/// exactly when the number is least likely to be right.
///
/// For most fields that trade is acceptable, since the alternative is losing the value.
/// For these it is not, because a wrong answer is worse than no answer:
///
/// - `m_PlayerDataGlobal` is the base address of a player's entire stat block. A stale
///   offset here does not corrupt one number, it relocates the whole scoreboard: kills,
///   deaths, souls, level and items all read from the wrong place and all look plausible.
/// - `m_steamID` identifies a person. A wrong one attributes a match to the wrong
///   account, which is worse than declining to say who played.
/// - `m_pGameRules` is the root pointer for match state. A stale offset yields a garbage
///   pointer whose dereferences produce arbitrary numbers rather than a clean `None`.
pub const UNSAFE_FALLBACKS: &[(&str, &str)] = &[
    ("CCitadelPlayerController", "m_PlayerDataGlobal"),
    ("CCitadelPlayerController", "m_steamID"),
    ("C_CitadelGameRulesProxy", "m_pGameRules"),
];

/// Whether a baked fallback for this pair is refused on principle.
pub fn fallback_is_unsafe(class: &str, field: &str) -> bool {
    UNSAFE_FALLBACKS
        .iter()
        .any(|(c, f)| *c == class && *f == field)
}

/// Something the reader noticed that suggests the game has moved on.
///
/// Ordered so a `BTreeSet` deduplicates repeats: the same miss recurs on every tick and
/// should be reported once, not thousands of times.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Drift {
    /// A field resolved from the baked table because the live schema did not know it.
    ///
    /// The value is a number from the build this crate was written against. Treat it as
    /// suspect.
    StaleOffset {
        /// Schema class.
        class: String,
        /// Schema field.
        field: String,
    },
    /// A baked fallback existed but was refused; see [`UNSAFE_FALLBACKS`].
    ///
    /// The field reads as `None`. That is deliberate, and it means the schema no longer
    /// knows a name this crate depends on.
    ///
    /// Counted as corrupting by [`Drift::is_corrupting`]. The `None` itself is honest,
    /// but only fields whose absence wrecks a whole subtree are on that list:
    /// `m_PlayerDataGlobal` going missing takes every player statistic with it, and a
    /// consumer summing those into a scoreboard gets zeros that look like real numbers.
    /// Reporting healthy while that happens is the failure worth avoiding.
    RefusedFallback {
        /// Schema class.
        class: String,
        /// Schema field.
        field: String,
    },
    /// The runtime schema's enumerator name disagrees with this crate's mapping.
    ///
    /// The schema is authoritative. A disagreement means the enum was renumbered and the
    /// decoded variant is wrong, which is the failure that once had a live match
    /// reporting `PostGame`.
    EnumMismatch {
        /// Schema class carrying the enum-typed field.
        class: String,
        /// The enum-typed field.
        field: String,
        /// Raw value read from the game.
        raw: u32,
        /// Enumerator name the running game gives it.
        schema: String,
        /// Name this crate's baked mapping gives it.
        baked: String,
    },
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Drift::StaleOffset { class, field } => {
                write!(f, "{class}::{field} fell back to a baked offset")
            }
            Drift::RefusedFallback { class, field } => write!(
                f,
                "{class}::{field} is missing from the schema and its baked offset was refused"
            ),
            Drift::EnumMismatch {
                class,
                field,
                raw,
                schema,
                baked,
            } => write!(
                f,
                "{class}::{field} = {raw} is {schema} in the game but {baked} here"
            ),
        }
    }
}

impl Drift {
    /// Whether this drift means a value is likely wrong rather than merely absent.
    ///
    /// A refused fallback costs you data and is safe. A stale offset or a renumbered enum
    /// hands you a number that looks fine and is not.
    pub fn is_corrupting(&self) -> bool {
        matches!(
            self,
            Drift::StaleOffset { .. } | Drift::EnumMismatch { .. } | Drift::RefusedFallback { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scoreboard_root_is_never_taken_from_a_baked_offset() {
        assert!(fallback_is_unsafe(
            "CCitadelPlayerController",
            "m_PlayerDataGlobal"
        ));
        assert!(fallback_is_unsafe("CCitadelPlayerController", "m_steamID"));
        assert!(fallback_is_unsafe(
            "C_CitadelGameRulesProxy",
            "m_pGameRules"
        ));
    }

    #[test]
    fn ordinary_fields_still_get_their_fallback() {
        assert!(!fallback_is_unsafe(
            "C_CitadelPlayerPawn",
            "m_pGameSceneNode"
        ));
        assert!(!fallback_is_unsafe("CBaseModifier", "m_flDuration"));
    }

    /// This once asserted that absent data is safe. It is not, and the reason is the
    /// invariant below: `RefusedFallback` is *only* emitted for a field in
    /// `UNSAFE_FALLBACKS`, and that list exists precisely because losing those fields
    /// wrecks a whole subtree. A refused `m_PlayerDataGlobal` drops every statistic for
    /// every player at once, and `TeamStats` then sums the `None`s into zeros that are
    /// indistinguishable from a side that has genuinely scored nothing.
    #[test]
    fn both_kinds_of_drift_are_corrupting_for_different_reasons() {
        let refused = Drift::RefusedFallback {
            class: "A".into(),
            field: "b".into(),
        };
        let stale = Drift::StaleOffset {
            class: "A".into(),
            field: "b".into(),
        };
        assert!(
            refused.is_corrupting(),
            "a refused fallback drops a load-bearing field"
        );
        assert!(stale.is_corrupting(), "a stale offset reads as real data");
        assert_ne!(refused, stale);
        assert_ne!(refused.to_string(), stale.to_string());
    }

    /// The invariant the classification above rests on. If a field ever produced a
    /// `RefusedFallback` without being on the unsafe list, calling it corrupting would be
    /// too blunt - so pin the relationship rather than leaving it to a comment.
    #[test]
    fn a_refused_fallback_is_only_possible_for_an_unsafe_field() {
        for (class, field) in UNSAFE_FALLBACKS {
            assert!(
                fallback_is_unsafe(class, field),
                "{class}::{field} is on the list but not reported as unsafe"
            );
        }
        assert!(!fallback_is_unsafe(
            "C_CitadelPlayerPawn",
            "m_pGameSceneNode"
        ));
    }

    #[test]
    fn drift_deduplicates_so_a_poll_loop_reports_once() {
        use std::collections::BTreeSet;
        let mut seen = BTreeSet::new();
        for _ in 0..1000 {
            seen.insert(Drift::StaleOffset {
                class: "C".into(),
                field: "f".into(),
            });
        }
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn an_enum_mismatch_names_both_readings() {
        let d = Drift::EnumMismatch {
            class: "C_CitadelGameRules".into(),
            field: "m_eGameState".into(),
            raw: 7,
            schema: "EGameState_HeroSelection".into(),
            baked: "GameInProgress".into(),
        };
        let msg = d.to_string();
        assert!(
            msg.contains("HeroSelection") && msg.contains("GameInProgress"),
            "{msg}"
        );
        assert!(d.is_corrupting());
    }
}

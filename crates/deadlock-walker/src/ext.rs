//! Readings of the Game Coordinator party types that the raw protobuf fields do not give
//! directly: the displayed join code, queue state, rank badge, rights bits.

use valveprotos::deadlock::c_msg_hero_selection_match_info::Hero;
use valveprotos::deadlock::cso_citadel_party::Member;
use valveprotos::deadlock::{CMsgHeroSelectionMatchInfo, CsoCitadelParty};

/// Alphabet the client renders a party's `join_code` with.
///
/// Recovered as a literal string constant in `client.dll`. Thirty characters: the twenty
/// consonants plus the ten digits. `A`, `E`, `I`, `O` and `U` are excluded so a generated
/// code cannot spell a word, and `L` because it is confusable with `1`.
pub const PARTY_CODE_ALPHABET: &[u8; 30] = b"BCDFGHJKMNPQRSTVWXYZ0123456789";

/// Characters in a rendered party code.
pub const PARTY_CODE_LEN: usize = 5;

/// Derived views of a [`CsoCitadelParty`].
pub trait PartyExt {
    /// The party code as the game displays it, e.g. `J1ZKN`: `join_code` in base 30 over
    /// [`PARTY_CODE_ALPHABET`], most significant digit first, padded to five characters.
    /// `None` when no code was issued (zero), or the number is wider than five digits.
    fn display_code(&self) -> Option<String>;

    /// Whether the party is in a matchmaking queue.
    ///
    /// Keyed on the start time rather than on the mode: the mode is also set for a private
    /// lobby that is merely configured and has not been queued.
    fn is_queueing(&self) -> bool;

    /// Seconds since matchmaking began, given the current Unix time.
    ///
    /// The start time looks to be set once when matchmaking begins and left alone across
    /// requeues, so this may span several queue attempts.
    fn queued_for(&self, now_unix: u64) -> Option<u64>;

    /// Whether `account_id` is currently a member.
    fn contains(&self, account_id: u32) -> bool;

    /// Members other than `me`.
    fn others(&self, me: u32) -> Box<dyn Iterator<Item = &Member> + '_>;

    /// Whether this party can be shown, from the two messages alone, to postdate `older`.
    ///
    /// Only meaningful between two views of the same `party_id`; different parties are not
    /// ordered. Two proofs exist: an invite in `older` that is a member here (an accepted
    /// invite does not become pending again), and a member of `older` listed here in
    /// `left_members`.
    fn supersedes(&self, older: &CsoCitadelParty) -> bool;
}

impl PartyExt for CsoCitadelParty {
    fn display_code(&self) -> Option<String> {
        let mut n = self.join_code.filter(|c| *c != 0)?;
        let base = PARTY_CODE_ALPHABET.len() as u64;
        let mut out = [0u8; PARTY_CODE_LEN];
        for slot in out.iter_mut().rev() {
            *slot = PARTY_CODE_ALPHABET[(n % base) as usize];
            n /= base;
        }
        (n == 0).then(|| String::from_utf8_lossy(&out).into_owned())
    }

    fn is_queueing(&self) -> bool {
        self.match_making_start_time.unwrap_or(0) > 0
    }

    fn queued_for(&self, now_unix: u64) -> Option<u64> {
        let started = u64::from(self.match_making_start_time.filter(|t| *t > 0)?);
        Some(now_unix.saturating_sub(started))
    }

    fn contains(&self, account_id: u32) -> bool {
        self.members
            .iter()
            .any(|m| m.account_id == Some(account_id))
    }

    fn others(&self, me: u32) -> Box<dyn Iterator<Item = &Member> + '_> {
        Box::new(
            self.members
                .iter()
                .filter(move |m| m.account_id != Some(me)),
        )
    }

    fn supersedes(&self, older: &CsoCitadelParty) -> bool {
        if self.party_id.is_none() || self.party_id != older.party_id {
            return false;
        }
        let joined = older
            .invites
            .iter()
            .filter_map(|i| i.account_id)
            .any(|a| self.contains(a));
        let left = older
            .members
            .iter()
            .filter_map(|m| m.account_id)
            .any(|a| self.left_members.iter().any(|l| l.account_id == Some(a)));
        joined || left
    }
}

/// Derived views of a party [`Member`].
pub trait MemberExt {
    /// Whether the `Admin` bit (1) is set in `rights_flags`. Independent of
    /// [`is_creator`](Self::is_creator): a solo party's creator reads as `Creator` only.
    fn is_admin(&self) -> bool;

    /// Whether the `Creator` bit (2) is set in `rights_flags`.
    fn is_creator(&self) -> bool;

    /// The displayed rank badge, from the first ladder that has a non-zero one.
    fn rank_badge(&self) -> Option<u32>;

    /// Whether the member is still calibrating on any ladder.
    fn in_calibration(&self) -> bool;
}

impl MemberExt for Member {
    fn is_admin(&self) -> bool {
        self.rights_flags.unwrap_or(0) & 1 != 0
    }

    fn is_creator(&self) -> bool {
        self.rights_flags.unwrap_or(0) & 2 != 0
    }

    fn rank_badge(&self) -> Option<u32> {
        self.ranked_scores
            .iter()
            .find_map(|r| r.rank_display_badge.filter(|b| *b != 0))
    }

    fn in_calibration(&self) -> bool {
        self.ranked_scores
            .iter()
            .any(|r| r.in_calibration == Some(true))
    }
}

/// Derived views of a member's hero queue roster.
pub trait RosterExt {
    /// Selections ordered most wanted first; equal priorities keep their order.
    fn by_priority(&self) -> Vec<&Hero>;
}

impl RosterExt for CMsgHeroSelectionMatchInfo {
    fn by_priority(&self) -> Vec<&Hero> {
        let mut v: Vec<&Hero> = self.hero_selections.iter().collect();
        v.sort_by_key(|h| std::cmp::Reverse(h.priority.unwrap_or(0)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valveprotos::deadlock::cso_citadel_party::{Invite, LeftMember, RankedScores};

    fn party(id: u64) -> CsoCitadelParty {
        CsoCitadelParty {
            party_id: Some(id),
            ..Default::default()
        }
    }

    fn member(id: u32) -> Member {
        Member {
            account_id: Some(id),
            ..Default::default()
        }
    }

    fn invite(id: u32) -> Invite {
        Invite {
            account_id: Some(id),
            ..Default::default()
        }
    }

    fn departed(id: u32) -> LeftMember {
        LeftMember {
            account_id: Some(id),
            ..Default::default()
        }
    }

    #[test]
    fn join_codes_render_as_the_game_shows_them() {
        let code = |n| {
            CsoCitadelParty {
                join_code: Some(n),
                ..Default::default()
            }
            .display_code()
        };
        assert_eq!(code(5_444_319).as_deref(), Some("J1ZKN"));
        assert_eq!(code(423_386).as_deref(), Some("BV0R6"));
        assert_eq!(code(0), None);
        assert_eq!(code(30u64.pow(5)), None, "six digits would be truncated");
        assert_eq!(party(1).display_code(), None);
    }

    #[test]
    fn queueing_follows_the_start_time_not_the_mode() {
        let mut p = CsoCitadelParty {
            match_mode: Some(1),
            ..Default::default()
        };
        assert!(!p.is_queueing());
        assert_eq!(p.queued_for(1_000), None);
        p.match_making_start_time = Some(0);
        assert!(!p.is_queueing());
        p.match_making_start_time = Some(940);
        assert!(p.is_queueing());
        assert_eq!(p.queued_for(1_000), Some(60));
        assert_eq!(
            p.queued_for(900),
            Some(0),
            "a clock behind the start clamps"
        );
    }

    #[test]
    fn membership_and_others() {
        let p = CsoCitadelParty {
            members: vec![member(1), member(2), member(3)],
            ..Default::default()
        };
        assert!(p.contains(2));
        assert!(!p.contains(9));
        let others: Vec<_> = p.others(2).filter_map(|m| m.account_id).collect();
        assert_eq!(others, [1, 3]);
    }

    #[test]
    fn an_accepted_invite_orders_two_generations() {
        let mut old = party(7);
        old.members = vec![member(1)];
        old.invites = vec![invite(2)];
        let mut new = party(7);
        new.members = vec![member(1), member(2)];
        assert!(new.supersedes(&old));
        assert!(!old.supersedes(&new));
    }

    #[test]
    fn a_departure_orders_them_the_other_way() {
        let mut old = party(7);
        old.members = vec![member(1), member(2)];
        let mut new = party(7);
        new.members = vec![member(1)];
        new.left_members = vec![departed(2)];
        assert!(new.supersedes(&old));
        assert!(!old.supersedes(&new));
    }

    #[test]
    fn different_parties_are_never_ordered() {
        let mut old = party(7);
        old.invites = vec![invite(2)];
        let mut new = party(8);
        new.members = vec![member(2)];
        assert!(!new.supersedes(&old));
        let mut idless = new.clone();
        idless.party_id = None;
        assert!(!idless.supersedes(&idless.clone()));
    }

    #[test]
    fn rights_bits_are_independent() {
        let m = |flags| Member {
            rights_flags: Some(flags),
            ..Default::default()
        };
        assert!(!m(2).is_admin() && m(2).is_creator());
        assert!(m(1).is_admin() && !m(1).is_creator());
        assert!(m(3).is_admin() && m(3).is_creator());
        assert!(!Member::default().is_admin() && !Member::default().is_creator());
    }

    #[test]
    fn the_rank_badge_is_the_first_nonzero_one_and_calibration_is_any_ladder() {
        let scores = |badge, calibrating| RankedScores {
            rank_display_badge: badge,
            in_calibration: calibrating,
            ..Default::default()
        };
        let m = Member {
            ranked_scores: vec![scores(Some(0), None), scores(Some(54), Some(true))],
            ..Default::default()
        };
        assert_eq!(m.rank_badge(), Some(54));
        assert!(m.in_calibration());
        assert_eq!(Member::default().rank_badge(), None);
        assert!(!Member::default().in_calibration());
    }

    #[test]
    fn heroes_are_listed_most_wanted_first() {
        let hero = |id, priority| Hero {
            hero_id: id,
            priority,
        };
        let roster = CMsgHeroSelectionMatchInfo {
            hero_selections: vec![
                hero(Some(2), Some(1)),
                hero(Some(1), Some(9)),
                hero(None, None),
            ],
            ..Default::default()
        };
        let ids: Vec<_> = roster.by_priority().iter().map(|h| h.hero_id).collect();
        assert_eq!(ids, [Some(1), Some(2), None]);
    }
}

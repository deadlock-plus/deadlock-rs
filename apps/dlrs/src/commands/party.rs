//! `dlrs party`: read the Game Coordinator's party object.

use std::time::Instant;

use deadlock_core::{
    BotDifficulty, ChatMode, GameMode, HeroId, HeroNames, MatchMode, MmPreference, RankBadge,
    RegionMode,
};
use deadlock_data::HeroCatalog;
use deadlock_reader::steam;
use deadlock_walker::GcSession;
use deadlock_walker::ext::{MemberExt, PartyExt, RosterExt};
use valveprotos::deadlock::CsoCitadelParty as Party;
use valveprotos::deadlock::cso_citadel_party::Member;

use super::attach;
use crate::catalog::hero_catalog;
use crate::output::{opt, trunc};

/// The heroes a member queued as, most wanted first.
fn roster_of(m: &Member, heroes: &HeroCatalog) -> String {
    let Some(roster) = m.hero_roster.as_ref() else {
        return "-".into();
    };
    let picked: Vec<String> = roster
        .by_priority()
        .into_iter()
        .filter_map(|h| h.hero_id)
        .map(|id| heroes.display_name(HeroId(id)))
        .collect();
    if picked.is_empty() {
        return "-".into();
    }
    picked.join(", ")
}

/// What the party is queueing for, and what it has configured.
fn print_matchmaking(p: &Party) {
    if p.is_queueing() {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|now| p.queued_for(now.as_secs()))
            .map(|s| format!(", matchmaking for {}:{:02}", s / 60, s % 60))
            .unwrap_or_default();
        outln!(
            "queue      {} / {}{}  (start_time {})",
            MatchMode::from_raw(p.match_mode.unwrap_or(0) as u32).name(),
            GameMode::from_raw(p.game_mode.unwrap_or(0) as u32).name(),
            elapsed,
            p.match_making_start_time.unwrap_or(0)
        );
    } else {
        outln!("queue      not queueing");
    }
    outln!(
        "region     {}   preference {}   chat {}",
        RegionMode::from_raw(p.region_mode.unwrap_or(0) as u32).name(),
        MmPreference::from_raw(p.mm_preference.unwrap_or(0) as u32).name(),
        ChatMode::from_raw(p.chat_mode.unwrap_or(0) as u32).name()
    );
    let bots = BotDifficulty::from_raw(p.bot_difficulty.unwrap_or(0) as u32);
    if bots != BotDifficulty::None {
        outln!("bots       {}", bots.name());
    }
    if p.is_high_skill_range_party == Some(true) {
        outln!("flagged    high skill range party");
    }
    if p.desires_laning_together == Some(true) {
        outln!("laning     members asked to be laned together");
    }
    // Members must agree on the build or the Game Coordinator will not start a match, so
    // a split is worth surfacing rather than leaving as a puzzling failure to queue.
    let mut builds: Vec<u32> = p
        .members
        .iter()
        .filter_map(|m| m.compatibility_version)
        .collect();
    builds.sort_unstable();
    builds.dedup();
    match builds.as_slice() {
        [] => {}
        [one] => outln!("build      {one}"),
        many => outln!("build      MISMATCH across members: {many:?}"),
    }
}

/// Locate and print the current party.
///
/// The first read pays for a full heap sweep, which takes about a second. There is no
/// pointer to follow, so the object has to be found by its RTTI vtable and checked against
/// the local account id.
pub fn party() -> i32 {
    let Some(r) = attach() else { return 1 };
    let account = match steam::active_account_id() {
        Ok(Some(id)) => id,
        Ok(None) => {
            eprintln!("no Steam user is signed in; the scan anchors on your account id");
            return 1;
        }
        Err(e) => {
            eprintln!("could not read the local account id: {e}");
            return 1;
        }
    };

    let mut gc = match GcSession::new(r.memory(), account) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("could not read the client's protobuf tables: {e}");
            return 1;
        }
    };
    // The sweep reads gigabytes and iterates every byte, so an unoptimised build is
    // several times slower. Saying so beats leaving someone to conclude it is broken.
    if cfg!(debug_assertions) {
        outln!("NOTE: debug build. The sweep takes roughly 6x longer than --release.");
    }
    outln!("account {account}, sweeping for the party object...");
    let t = Instant::now();
    let report = match gc.sweep(r.memory()) {
        Ok(rep) => rep,
        Err(e) => {
            eprintln!("sweep failed: {e}");
            return 1;
        }
    };
    outln!(
        "swept {} regions, {} anchor hits, {} objects pinned in {:.1}s",
        report.regions,
        report.hits,
        report.pinned,
        t.elapsed().as_secs_f64()
    );

    match gc.party(r.memory()) {
        Ok(Some(p)) => {
            outln!("");
            outln!("party_id   {}", opt(p.party_id));
            outln!(
                "join_code  {}   (numeric {})",
                p.display_code().unwrap_or_else(|| "-".into()),
                opt(p.join_code)
            );
            outln!("private    {}", opt(p.is_private_lobby));
            print_matchmaking(&p);
            outln!("");

            let heroes = hero_catalog(Some(&r));
            outln!(
                "{:<12} {:<20} {:<7} {:<6} {:<8} {}",
                "account",
                "name",
                "rank",
                "ready",
                "rights",
                "queued as"
            );
            for m in &p.members {
                let rank = match m.rank_badge() {
                    Some(b) if m.in_calibration() => format!("{} cal", RankBadge(b)),
                    Some(b) => RankBadge(b).to_string(),
                    None if m.in_calibration() => "calibrating".to_string(),
                    None => "-".to_string(),
                };
                outln!(
                    "{:<12} {:<20} {:<7} {:<6} {:<8} {}",
                    opt(m.account_id),
                    trunc(&m.persona_name.clone().unwrap_or_else(|| "-".into()), 20),
                    rank,
                    opt(m.is_ready),
                    if m.is_creator() {
                        "creator"
                    } else if m.is_admin() {
                        "admin"
                    } else {
                        "-"
                    },
                    roster_of(m, &heroes)
                );
            }
            for l in &p.left_members {
                outln!("left         {}", opt(l.account_id));
            }
            for i in &p.invites {
                outln!(
                    "invited      {} {}",
                    opt(i.account_id),
                    i.persona_name.clone().unwrap_or_default()
                );
            }

            // The number that decides whether this is pollable.
            let t = Instant::now();
            for _ in 0..1000 {
                let _ = gc.party(r.memory());
            }
            outln!("");
            outln!(
                "pinned re-read {:.1} us each over 1000 reads",
                t.elapsed().as_secs_f64() * 1000.0
            );
            0
        }
        Ok(None) => {
            outln!("");
            outln!("no party object resident.");
            outln!("that is the normal answer when playing solo without having made a party.");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valveprotos::deadlock::CMsgHeroSelectionMatchInfo as HeroRoster;
    use valveprotos::deadlock::c_msg_hero_selection_match_info::Hero as HeroSelection;
    use valveprotos::deadlock::cso_citadel_party::Member;

    fn selection(hero_id: Option<u32>, priority: u32) -> HeroSelection {
        HeroSelection {
            hero_id,
            priority: Some(priority),
        }
    }

    fn member_with(roster: Option<HeroRoster>) -> Member {
        Member {
            hero_roster: roster,
            ..Member::default()
        }
    }

    /// `roster_of` answers `"-"` for both an absent roster and a present-but-empty one,
    /// which is deliberate: this is a table cell, and an empty cell would leave the
    /// columns after it misread. The overlay's equivalent returns an empty `Vec` instead,
    /// because a UI list renders emptiness on its own. The two are not shared for that
    /// reason, so this pins the difference rather than leaving it to look like drift.
    #[test]
    fn an_absent_or_empty_roster_is_a_dash() {
        let heroes = HeroCatalog::bundled();
        assert_eq!(roster_of(&member_with(None), &heroes), "-");
        assert_eq!(
            roster_of(&member_with(Some(HeroRoster::default())), &heroes),
            "-"
        );
        let idless = HeroRoster {
            hero_selections: vec![selection(None, 3), selection(None, 1)],
            ..HeroRoster::default()
        };
        assert_eq!(roster_of(&member_with(Some(idless)), &heroes), "-");
    }

    #[test]
    fn heroes_are_named_and_ordered_most_wanted_first() {
        let heroes = HeroCatalog::bundled();
        let roster = HeroRoster {
            hero_selections: vec![selection(Some(2), 1), selection(Some(1), 9)],
            ..HeroRoster::default()
        };
        let out = roster_of(&member_with(Some(roster)), &heroes);
        assert_eq!(
            out,
            format!(
                "{}, {}",
                heroes.display_name(HeroId(1)),
                heroes.display_name(HeroId(2))
            )
        );
    }

    /// A selection with no hero id is dropped, not rendered as a gap, so a partial
    /// roster still reads as a list of the heroes that are known.
    #[test]
    fn selections_without_a_hero_id_are_skipped() {
        let heroes = HeroCatalog::bundled();
        let roster = HeroRoster {
            hero_selections: vec![
                selection(Some(1), 9),
                selection(None, 5),
                selection(Some(2), 1),
            ],
            ..HeroRoster::default()
        };
        let out = roster_of(&member_with(Some(roster)), &heroes);
        assert_eq!(out.split(", ").count(), 2);
    }
}

//! `dlrs account`: the Game Coordinator's account objects and hero builds.

use std::time::Instant;

use deadlock_core::{HeroId, HeroNames};
use deadlock_data::HeroCatalog;
use deadlock_reader::steam;
use deadlock_walker::GcSession;

use super::attach;
use crate::catalog::hero_catalog;
use crate::output::{opt, trunc};

/// A unix timestamp as a date, or a dash when it is zero.
///
/// Zero is the game's "never", not 1970, and rendering it as a date would make an account
/// that has never been banned look like one banned at the epoch.
fn stamp(secs: Option<u32>) -> String {
    match secs {
        Some(s) if s > 0 => format!("{s}"),
        _ => "-".into(),
    }
}

/// A hero id as a name.
///
/// Hero zero is not a hero. `CMsgAccountStats` opens its breakdown with an aggregate row
/// carrying `hero_id = 0`, and rendering that through the catalogue gives `hero 0`, which
/// reads as a hero the roster is missing rather than as a total.
fn hero_name(id: Option<u32>, heroes: &HeroCatalog) -> String {
    match id {
        None => "-".into(),
        Some(0) => "(all heroes)".into(),
        Some(h) => heroes.display_name(HeroId(h)),
    }
}

pub fn account() -> i32 {
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

    let heroes = hero_catalog(Some(&r));
    let mut gc = match GcSession::new(r.memory(), account) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("could not read the client's protobuf tables: {e}");
            return 1;
        }
    };
    if cfg!(debug_assertions) {
        outln!("NOTE: debug build. The sweep takes roughly 6x longer than --release.");
    }
    outln!("account {account}, sweeping...");
    let t = Instant::now();
    let report = match gc.sweep(r.memory()) {
        Ok(rep) => rep,
        Err(e) => {
            eprintln!("sweep failed: {e}");
            return 1;
        }
    };
    outln!(
        "swept {} regions in {:.2}s: {} hits, {} pinned",
        report.regions,
        t.elapsed().as_secs_f32(),
        report.hits,
        report.pinned
    );
    outln!("");

    match gc.game_account(r.memory()) {
        Ok(Some(a)) => {
            outln!("account object");
            outln!(
                "  most played      {}",
                hero_name(a.most_played_hero_id, &heroes)
            );
            outln!("  wins/losses      {}/{}", opt(a.wins), opt(a.losses));
            outln!(
                "  brawl w/l/k      {}/{}/{}",
                opt(a.brawl_wins),
                opt(a.brawl_losses),
                opt(a.brawl_kills)
            );
            outln!(
                "  bans mm/comms/rp {} {} {}",
                stamp(a.mm_ban_until),
                stamp(a.comms_ban_until),
                stamp(a.report_ban_until)
            );
            outln!(
                "  priority tokens  {} (progress {})",
                opt(a.priority_tokens),
                opt(a.priority_token_progress)
            );
            outln!("  last matchmade   {}", stamp(a.last_mm_match_time));
        }
        // Not resident is a normal answer, not a failure.
        Ok(None) => outln!("account object: not resident in the heap this sweep"),
        Err(e) => outln!("account object: {e}"),
    }
    outln!("");

    match gc.account_stats(r.memory()) {
        Ok(Some(s)) => {
            outln!("account stats: {} hero breakdowns", s.stats.len());
            for h in s.stats.iter().take(5) {
                outln!(
                    "  {:<16} {} stat ids",
                    hero_name(h.hero_id, &heroes),
                    h.stat_id.len()
                );
            }
        }
        Ok(None) => outln!("account stats: not resident"),
        Err(e) => outln!("account stats: {e}"),
    }
    outln!("");

    match gc.hero_builds(r.memory()) {
        Ok(builds) if builds.is_empty() => outln!("hero builds: none resident"),
        Ok(builds) => {
            outln!("hero builds: {} resident", builds.len());
            for b in builds.iter().take(10) {
                outln!(
                    "  {:<16} {:<28} by {} {}",
                    hero_name(b.hero_id, &heroes),
                    trunc(b.name.as_deref().unwrap_or("-"), 28),
                    opt(b.author_account_id),
                    if b.author_account_id == Some(account) {
                        "(yours)"
                    } else {
                        ""
                    }
                );
            }
        }
        Err(e) => outln!("hero builds: {e}"),
    }
    0
}

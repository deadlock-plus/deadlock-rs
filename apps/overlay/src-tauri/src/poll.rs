//! Two pollers on two threads, because the two sources have nothing in common but being
//! polled.
//!
//! The entity reader answers in microseconds and wants a tight interval. The Game
//! Coordinator scan answers from a pin in microseconds too, but when a pin dies it can
//! spend a second sweeping the heap. Sharing a thread would let one of those sweeps eat a
//! dozen scoreboard frames, so they get one each and the window merges whatever arrives.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deadlock_core::{GameMode, MatchMode, MmPreference, RankBadge, RegionMode};
use deadlock_data::{Catalogs, HeroCatalog, Source};
use deadlock_reader::supervise::{Attached, ReaderSupervisor};
use deadlock_reader::{Reader, cache::SnapshotCache};
use deadlock_walker::GcSession;
use deadlock_walker::ext::{MemberExt, PartyExt};
use tauri::{AppHandle, Emitter};
use valveprotos::deadlock::CsoCitadelParty;

use crate::model::{MatchView, PartyMemberView, PartyView, SwapLog, roster_names};
use crate::statlocker::Statlocker;

/// How often the scoreboard is refreshed. A snapshot costs well under a millisecond.
const MATCH_INTERVAL: Duration = Duration::from_millis(250);
/// How often the party is refreshed. Served from a pin unless the object moved.
const PARTY_INTERVAL: Duration = Duration::from_secs(2);
/// How long to wait before trying to attach again after a failure.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// The supervised reader, shared by both pollers so the game is only opened once.
///
/// Attach-retry, reattach and failure tolerance all live in `ReaderSupervisor`; this used
/// to be a third hand-rolled copy of that logic.
type Shared = Arc<Mutex<ReaderSupervisor>>;

/// Hero names, localised from the install when there is one to read.
///
/// Install first so names match the running build, snapshot behind it so ids always
/// resolve. The API is deliberately not in the order: this runs on a poll loop and must
/// not reach the network.
fn catalog(reader: Option<&Reader>) -> HeroCatalog {
    Catalogs::new()
        .sources([Source::Client, Source::Bundled])
        .maybe_game_dir(reader.and_then(Reader::game_dir))
        // Names stay correct between matches, when no process is attached.
        .discover_game_dir()
        .heroes()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Start both pollers. They run for the life of the process.
pub fn spawn(app: AppHandle) {
    let shared: Shared = Arc::new(Mutex::new(ReaderSupervisor::new(RETRY_INTERVAL)));
    spawn_match(app.clone(), Arc::clone(&shared));
    spawn_party(app, shared);
}

/// Take the current reader, attaching if there is not one yet.
///
/// `Err(Some(_))` carries something worth showing the user; `Err(None)` means "nothing to
/// report yet". An overlay that says nothing when the game is closed looks broken.
fn reader_of(shared: &Shared) -> Result<Arc<Reader>, Option<String>> {
    let Ok(mut sup) = shared.lock() else {
        return Err(Some("reader lock poisoned".into()));
    };
    match sup.acquire() {
        Attached::Fresh(r) | Attached::Held(r) => Ok(r),
        Attached::Absent | Attached::Waiting => Err(None),
        Attached::Failed(why) => Err(Some(why)),
    }
}

/// Tell the supervisor a read failed, so a run of them costs the reader.
fn note_failure(shared: &Shared) {
    if let Ok(mut sup) = shared.lock() {
        sup.failed();
    }
}

/// Tell the supervisor a read succeeded, clearing the failure run.
fn note_success(shared: &Shared) {
    if let Ok(mut sup) = shared.lock() {
        sup.succeeded();
    }
}

fn spawn_match(app: AppHandle, shared: Shared) {
    std::thread::spawn(move || {
        let mut cache = SnapshotCache::new();
        let mut heroes = catalog(None);
        let mut have_catalog = false;
        // Hero swaps only exist as a difference between two snapshots, so the state that
        // notices them has to outlive one turn of the loop.
        let mut swaps = SwapLog::new();
        // Off-thread by construction: `pp` reads a cache and queues, and the worker
        // behind it does the waiting. Without an API key it queues nothing at all - see
        // `crate::statlocker` for what is and is not known about that service.
        let statlocker = Statlocker::from_env();

        loop {
            let view = match reader_of(&shared) {
                Err(e) => {
                    // No client. Nothing to diff against, and last match's swaps must not
                    // reappear over whatever the next one turns out to be.
                    swaps.clear();
                    MatchView::detached(e)
                }
                Ok(reader) => {
                    // The bundled snapshot works offline; the installed game's own
                    // localisation is better, and only available once attached.
                    if !have_catalog {
                        heroes = catalog(Some(&reader));
                        have_catalog = true;
                    }
                    match reader.live_snapshot_cached(&mut cache) {
                        Ok(Some(s)) => {
                            note_success(&shared);
                            // Ranks are cached for one match and no longer, so a rank
                            // cannot follow a player into the next lobby.
                            statlocker.observe_match(s.match_id);
                            MatchView::from_snapshot(&s, &heroes)
                                .with_swaps(swaps.observe(&s, &heroes))
                                .with_statlocker_pp(|account| statlocker.pp(account).into())
                        }
                        Ok(None) => {
                            note_success(&shared);
                            swaps.clear();
                            statlocker.observe_match(None);
                            MatchView::idle("not in a match".into())
                        }
                        Err(e) => {
                            // One failed read is a hiccup, not a closed game. The
                            // supervisor drops the reader only after a run of them.
                            //
                            // The swap log is deliberately left alone: clearing it here
                            // would drop the baseline, and a swap that happened across the
                            // hiccup would then be lost rather than reported late.
                            note_failure(&shared);
                            have_catalog = false;
                            MatchView::detached(Some(e.to_string()))
                        }
                    }
                }
            };
            let _ = app.emit("match", &view);
            std::thread::sleep(MATCH_INTERVAL);
        }
    });
}

fn spawn_party(app: AppHandle, shared: Shared) {
    std::thread::spawn(move || {
        let mut heroes = catalog(None);
        let mut have_catalog = false;
        let mut gc: Option<GcSession> = None;

        loop {
            if let Ok(reader) = reader_of(&shared) {
                if !have_catalog {
                    heroes = catalog(Some(&reader));
                    have_catalog = true;
                }
                // The party is the object that lists the local account, whose id comes from
                // the registry rather than from the game.
                if gc.is_none()
                    && let Ok(Some(id)) = deadlock_reader::steam::active_account_id()
                {
                    gc = GcSession::new(reader.memory(), id).ok();
                }
                if let Some(session) = gc.as_mut() {
                    let view = match session.parties(reader.memory()) {
                        Ok(list) => list.first().map(|p| party_view(p, &heroes)),
                        Err(deadlock_walker::Error::WrongProcess) => {
                            gc = None;
                            None
                        }
                        Err(_) => None,
                    };
                    let _ = app.emit("party", &view);
                }
            } else {
                let _ = app.emit("party", &Option::<PartyView>::None);
            }
            std::thread::sleep(PARTY_INTERVAL);
        }
    });
}

fn party_view(p: &CsoCitadelParty, heroes: &HeroCatalog) -> PartyView {
    let queueing = p.is_queueing();
    let queue = if queueing {
        format!(
            "{} / {}",
            MatchMode::from_raw(p.match_mode.unwrap_or(0) as u32).name(),
            GameMode::from_raw(p.game_mode.unwrap_or(0) as u32).name()
        )
    } else {
        String::new()
    };

    let mut builds: Vec<u32> = p
        .members
        .iter()
        .filter_map(|m| m.compatibility_version)
        .collect();
    builds.sort_unstable();
    builds.dedup();

    PartyView {
        party_id: p.party_id,
        join_code: p.display_code(),
        queueing,
        queue,
        matchmaking_seconds: p.queued_for(unix_now()),
        region: RegionMode::from_raw(p.region_mode.unwrap_or(0) as u32)
            .name()
            .to_string(),
        preference: MmPreference::from_raw(p.mm_preference.unwrap_or(0) as u32)
            .name()
            .to_string(),
        members: p
            .members
            .iter()
            .map(|m| PartyMemberView {
                account_id: m.account_id.unwrap_or(0),
                name: m.persona_name.clone().unwrap_or_else(|| "-".into()),
                rank: m
                    .rank_badge()
                    .map(RankBadge)
                    .filter(deadlock_core::RankBadge::is_ranked)
                    .map(|b| b.to_string())
                    .unwrap_or_default(),
                ready: m.is_ready,
                is_creator: m.is_creator(),
                queued_as: roster_names(m, heroes),
            })
            .collect(),
        build_mismatch: builds.len() > 1,
    }
}

//! `dlrs events`: print match and party events as they happen.

use std::sync::Arc;
use std::time::{Duration, Instant};

use deadlock_core::{HeroId, HeroNames, ItemNames};
use deadlock_events::{
    Engine, Event, Health, MatchEvent, Notification, PartyEvent, PartySource, PostGameEvent,
    PostGameSource, ReaderSource,
};
use deadlock_reader::steam;

use super::attach;
use crate::catalog::{hero_catalog, item_catalog};

/// Drive both sources on their own schedules and format what arrives.
///
/// The match source polls ten times a second, comfortably inside the 12 to 19 second
/// window in which a finished match is still readable. The party source polls every two
/// seconds, since a pinned re-read is microseconds and membership rarely changes.
pub fn events() -> i32 {
    let Some(r) = attach() else { return 1 };
    let heroes = hero_catalog(Some(&r));
    let items = item_catalog(Some(&r));
    let reader = Arc::new(r);

    let mut engine = Engine::new()
        .with(ReaderSource::new(Arc::clone(&reader)).every(Duration::from_millis(100)));

    // The party source needs the local account id to anchor its scan. Without it the
    // match half still works, so this degrades rather than failing.
    match steam::active_account_id() {
        Ok(Some(account)) => {
            engine = engine
                .with(PartySource::new(Arc::clone(&reader), account).every(Duration::from_secs(2)))
                .with(PostGameSource::new(Arc::clone(&reader), account));
            outln!("watching match, party and post-game events (ctrl-c to stop)");
        }
        _ => outln!("watching match events only; no Steam account id, so no party source"),
    }

    // Held for the loop's lifetime: dropping it stops every source thread.
    // `start` reports a failed thread spawn rather than panicking; there is nothing
    // useful to do about it here but say so.
    let (_handle, rx) = match engine.start() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("could not start the event engine: {e}");
            return 1;
        }
    };

    let mut labels = Labels::default();
    for note in rx {
        let line = match note {
            Notification::Health { source, health } => match health {
                Health::Ok => format!("[{source}] ok"),
                Health::Idle(why) => format!("[{source}] idle: {why}"),
                Health::Degraded(why) => format!("[{source}] DEGRADED, values may be wrong: {why}"),
                Health::Failed(why) => format!("[{source}] failed: {why}"),
                _ => continue,
            },
            Notification::Event {
                source,
                event: Event::Party(p),
            } => format!("[{source}] {}", party_line(&p)),
            Notification::Event {
                source,
                event: Event::PostGame(p),
            } => format!("[{source}] {}", postgame_line(&p)),
            Notification::Event {
                source,
                event: Event::Match(m),
            } => {
                let Some(text) = match_line(&m, &reader, &heroes, &items, &mut labels) else {
                    continue;
                };
                format!("[{source}] {text}")
            }
            // Notification is #[non_exhaustive]: a new variant must not break this.
            _ => continue,
        };
        outln!("{line}");
    }
    0
}

fn postgame_line(p: &PostGameEvent) -> String {
    match p {
        PostGameEvent::Captured { match_id, metadata } => format!(
            "captured metadata for match {match_id}: {} player(s)",
            metadata.match_info.as_ref().map_or(0, |i| i.players.len())
        ),
        PostGameEvent::Updated { match_id, metadata } => format!(
            "updated metadata for match {match_id}: {} player(s)",
            metadata.match_info.as_ref().map_or(0, |i| i.players.len())
        ),
        PostGameEvent::Missed { match_id, attempts } => {
            format!("missed match {match_id}: not resident after {attempts} attempt(s)")
        }
        other => format!("{other:?}"),
    }
}

fn party_line(p: &PartyEvent) -> String {
    let who = |id: &u32, name: &Option<String>| match name {
        Some(n) => format!("{n} ({id})"),
        None => id.to_string(),
    };
    match p {
        PartyEvent::Formed { party_id, members } => {
            format!("party {party_id} with {} member(s)", members.len())
        }
        PartyEvent::Disbanded { party_id } => format!("party {party_id} gone"),
        PartyEvent::MemberJoined {
            account_id,
            persona_name,
        } => format!("{} joined the party", who(account_id, persona_name)),
        PartyEvent::MemberLeft {
            account_id,
            persona_name,
        } => format!("{} left the party", who(account_id, persona_name)),
        PartyEvent::ReadyChanged {
            account_id,
            is_ready,
        } => format!(
            "{account_id} is {}",
            if *is_ready { "ready" } else { "not ready" }
        ),
        other => format!("{other:?}"),
    }
}

/// The scoreboard, refreshed on a timer rather than once per event.
///
/// Events carry slots, not names, so labelling one needs a snapshot - and taking a fresh
/// one per event meant a full entity walk inside a loop that delivers bursts of them at
/// 10 Hz. Names and team labels do not change during a match, so a periodic refresh is
/// plenty.
#[derive(Default)]
struct Labels {
    snap: Option<deadlock_reader::snapshot::LiveSnapshot>,
    refreshed: Option<Instant>,
}

impl Labels {
    const TTL: Duration = Duration::from_secs(2);

    fn get(
        &mut self,
        reader: &deadlock_reader::Reader,
    ) -> Option<&deadlock_reader::snapshot::LiveSnapshot> {
        let stale = self
            .refreshed
            .map(|t| t.elapsed() >= Self::TTL)
            .unwrap_or(true);
        if stale {
            // Stamped even when the read fails, so a closed game does not send every
            // event back down the same expensive path.
            self.refreshed = Some(Instant::now());
            if let Ok(Some(s)) = reader.live_snapshot() {
                self.snap = Some(s);
            }
        }
        self.snap.as_ref()
    }
}

fn match_line(
    ev: &MatchEvent,
    reader: &deadlock_reader::Reader,
    heroes: &deadlock_data::HeroCatalog,
    items: &deadlock_data::ItemCatalog,
    labels: &mut Labels,
) -> Option<String> {
    let snap = labels.get(reader);
    let who = |slot: Option<u32>| -> String {
        snap.and_then(|s| s.players.iter().find(|p| p.slot == slot))
            .map(deadlock_reader::snapshot::PlayerRow::display_name)
            .unwrap_or_else(|| "?".into())
    };
    let hero = |h: Option<HeroId>| -> String {
        h.map(|h| heroes.display_name(h))
            .unwrap_or_else(|| "?".into())
    };

    Some(match ev {
        MatchEvent::MatchStarted { match_id } => format!("match started: {match_id:?}"),
        MatchEvent::MatchEnded {
            match_id,
            final_state,
        } => {
            let lead = final_state
                .soul_lead()
                .filter(|(_, n)| *n > 0)
                .map(|(t, n)| format!(", {} ahead by {n}", final_state.team_name(t)))
                .unwrap_or_default();
            format!(
                "MATCH ENDED {match_id:?}: captured {} players{lead}",
                final_state.scoreboard().count()
            )
        }
        MatchEvent::MatchLeft { match_id } => format!("left match {match_id:?}"),
        MatchEvent::GameStateChanged { from, to } => {
            format!("state {} -> {}", from.name(), to.name())
        }
        MatchEvent::PauseChanged { paused } => {
            (if *paused { "PAUSED" } else { "unpaused" }).to_string()
        }
        MatchEvent::Kill {
            slot,
            hero: h,
            total,
            ..
        } => format!("{} ({}) killed -> {total}", who(*slot), hero(*h)),
        MatchEvent::Death {
            slot,
            hero: h,
            total,
            ..
        } => format!("{} ({}) died -> {total}", who(*slot), hero(*h)),
        MatchEvent::LevelUp { slot, level, .. } => {
            format!("{} is level {level}", who(*slot))
        }
        MatchEvent::ItemPurchased { slot, item } => {
            format!("{} bought {}", who(*slot), items.item_display_name(*item))
        }
        MatchEvent::ObjectiveDestroyed { kind, team } => format!(
            "{} {kind} destroyed",
            team.and_then(|t| snap.map(|s| s.team_name(t).to_string()))
                .unwrap_or_else(|| "?".into())
        ),
        MatchEvent::MidbossKilled { total } => format!("midboss killed ({total}x)"),
        MatchEvent::PlayerLeft { slot, name } => {
            format!(
                "{} left the match",
                name.clone().unwrap_or_else(|| who(*slot))
            )
        }
        // Quieter events are skipped rather than flooding a live feed.
        _ => return None,
    })
}

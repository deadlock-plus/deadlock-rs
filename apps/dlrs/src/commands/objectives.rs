//! `dlrs objectives`: map structures, and the objective timers around them.

use super::with_snapshot;
use crate::output::opt;

use deadlock_core::Team;
use deadlock_reader::snapshot::{LiveSnapshot, MidbossSchedule};

/// Seconds as `m:ss`, or a dash when there is no answer.
///
/// A dash rather than a zero on purpose: an absent timer and an expired one mean different
/// things, and most of these are absent because the client is never told them.
fn countdown(secs: Option<f32>) -> String {
    match secs {
        Some(s) if s.is_finite() && s >= 0.0 => {
            let s = s as u32;
            format!("{}:{:02}", s / 60, s % 60)
        }
        _ => "-".into(),
    }
}

/// The Midboss spawn schedule as a person should read it.
///
/// An overdue schedule is said out loud rather than shown as a countdown at zero. Both
/// states used to render `0:00`, which claims an imminent spawn - and live, that claim
/// held for a full minute while the Midboss spawned and died behind it.
fn spawn_note(s: &LiveSnapshot) -> String {
    match s.midboss_schedule() {
        MidbossSchedule::Alive => "-".into(),
        MidbossSchedule::Unscheduled => "not scheduled".into(),
        MidbossSchedule::In(secs) => countdown(Some(secs)),
        MidbossSchedule::Overdue(by) => {
            format!(
                "overdue by {} (schedule stale, or alive and unseen)",
                countdown(Some(by))
            )
        }
    }
}

/// The objective timers, above the structure table.
fn timers(s: &LiveSnapshot) {
    let t = &s.timers;
    let now = s.clock.now;

    outln!("match  {}", t.display().unwrap_or_else(|| "-".into()));

    let m = &t.midboss_state;
    let hp = match (m.health, m.max_health) {
        (Some(h), Some(max)) => format!(
            "{h}/{max}{}",
            m.health_fraction()
                .map_or_else(String::new, |f| format!(" ({:.0}%)", f * 100.0))
        ),
        _ => "not on the map".into(),
    };
    outln!("midboss  {}  next spawn {}", hp, spawn_note(s));

    // The rift's own cadence is server-side, so there is deliberately no "next rift".
    let r = &t.rift_state;
    outln!(
        "rift     contested {}  gold amber {} sapphire {}  give up in {}",
        r.contested(now),
        opt(r.gold(Team::AMBER)),
        opt(r.gold(Team::SAPPHIRE)),
        countdown(r.seconds_until_give_up(now)),
    );
    if let Some(best) = r.best_progress() {
        outln!(
            "         capture {:.0}%  being captured {}",
            best * 100.0,
            r.being_captured()
        );
    }
    if r.spawner_count > 0 || r.cash_in_count > 0 {
        outln!(
            "         spawners {}  cash-ins {}  enable state {}",
            r.spawner_count,
            r.cash_in_count,
            opt(r.cash_in_enable_state)
        );
    }

    outln!(
        "urn      {}  bridge buffs next {}",
        t.urn.as_ref().map_or_else(
            || "-".into(),
            |o| format!("{} on map", o.count.unwrap_or(0))
        ),
        t.bridge_buffs
            .as_ref()
            .map_or_else(|| "-".into(), |o| countdown(o.next_spawn_in))
    );
    outln!("");
}

pub fn objectives() -> i32 {
    with_snapshot(|_, s| {
        timers(s);
        outln!(
            "{:<16} {:<10} {:>8} {:>8} {:>5}",
            "label",
            "team",
            "hp",
            "maxhp",
            "lane"
        );
        for o in &s.objectives {
            outln!(
                "{:<16} {:<10} {:>8} {:>8} {:>5}",
                o.kind,
                o.team_name.clone().unwrap_or_else(|| "?".into()),
                opt(o.health),
                opt(o.max_health),
                opt(o.lane)
            );
        }
        0
    })
}

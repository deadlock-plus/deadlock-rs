//! `dlrs modifiers`: the buffs, debuffs and crowd control on each player.

use super::with_snapshot;
use crate::output::opt;

use deadlock_reader::snapshot::Modifier;

/// Strip the prefix every Citadel modifier class shares, so the interesting half fits.
///
/// Falls back to the whole name rather than truncating: a class that does not follow the
/// convention is exactly the one worth seeing in full.
fn short(class: &str) -> &str {
    class
        .strip_prefix("CCitadel_Modifier_")
        .or_else(|| class.strip_prefix("CCitadel_"))
        .unwrap_or(class)
}

/// How long is left, or why there is no answer.
fn remaining(m: &Modifier, now: Option<f32>) -> String {
    if m.is_permanent() {
        return "permanent".into();
    }
    match (now, m.expires_at()) {
        (Some(now), Some(_)) => m
            .remaining(now)
            .map_or_else(|| "?".into(), |s| format!("{s:.1}s")),
        _ => "?".into(),
    }
}

pub fn modifiers(filter: Option<&str>) -> i32 {
    with_snapshot(|_, s| {
        let now = s.clock.now;
        let mut any = false;
        for p in s.scoreboard() {
            let Some(mods) = p.modifiers.as_ref() else {
                continue;
            };
            let shown: Vec<&Modifier> = mods
                .iter()
                .filter(|m| {
                    filter.is_none_or(|f| {
                        m.class
                            .as_deref()
                            .is_some_and(|c| c.to_lowercase().contains(&f.to_lowercase()))
                    })
                })
                .collect();
            if shown.is_empty() {
                continue;
            }
            any = true;
            outln!(
                "{} (slot {}, {} modifiers)",
                p.display_name(),
                opt(p.slot),
                mods.len()
            );
            for m in shown {
                outln!(
                    "  {:<40} {:>10} {:>10} {:>6} cc={}",
                    m.class.as_deref().map_or("?", short),
                    remaining(m, now),
                    m.creation_time
                        .map_or_else(|| "?".into(), |t| format!("{t:.2}")),
                    m.stacks.map_or_else(|| "-".into(), |c| format!("{c}")),
                    m.crowd_control()
                        .map_or_else(|| "-".into(), |c| format!("{c:?}").to_lowercase()),
                );
            }
        }
        if !any {
            outln!("no modifiers read; is the client in a match?");
        }
        0
    })
}

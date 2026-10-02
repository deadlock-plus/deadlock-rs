//! `dlrs player`: full detail for one player or all of them.

use deadlock_core::{HeroNames, ItemId, ItemNames};
use deadlock_reader::Reader;
use deadlock_reader::snapshot::LiveSnapshot;

use super::with_snapshot;
use crate::catalog::{hero_catalog, item_catalog};
use crate::output::opt;

/// Selected by slot number or hero-name substring; no selector prints everyone.
pub fn player(which: Option<&str>) -> i32 {
    with_snapshot(|r, s| player_inner(which, r, s))
}

fn player_inner(which: Option<&str>, r: &Reader, s: &LiveSnapshot) -> i32 {
    let heroes = hero_catalog(Some(r));
    let items = item_catalog(Some(r));

    let sel = which.unwrap_or("").to_lowercase();
    let rows: Vec<_> = s
        .scoreboard()
        .filter(|p| {
            if sel.is_empty() {
                return true;
            }
            if p.slot.map(|x| x.to_string()) == Some(sel.clone()) {
                return true;
            }
            p.hero_id
                .map(|h| heroes.display_name(h).to_lowercase().contains(&sel))
                .unwrap_or(false)
        })
        .collect();

    if rows.is_empty() {
        outln!("no player matched {sel:?}");
        return 1;
    }

    for p in rows {
        outln!(
            "\n=== {} slot {} - {} (level {}) ===",
            p.team_name.clone().unwrap_or_else(|| "?".into()),
            opt(p.slot),
            p.hero_id
                .map(|h| heroes.display_name(h))
                .unwrap_or_else(|| "?".into()),
            opt(p.level)
        );
        outln!(
            "  {}/{}/{}  lh {}  denies {}  streak {}",
            opt(p.kills),
            opt(p.deaths),
            opt(p.assists),
            opt(p.last_hits),
            opt(p.denies),
            opt(p.kill_streak)
        );
        outln!(
            "  souls {}  (creep {}, secured {}, denied {}, neutral {})  baseline {}",
            opt(p.net_worth),
            opt(p.creep_souls),
            opt(p.secured_souls),
            opt(p.denied_souls),
            opt(p.neutral_souls),
            opt(p.farm_baseline)
        );
        outln!(
            "  ability points {}  hero dmg {}  obj dmg {}  healing {} (self {})",
            opt(p.ability_points),
            opt(p.hero_damage),
            opt(p.objective_damage),
            opt(p.healing),
            opt(p.self_healing)
        );
        outln!(
            "  alive {}  hp {}  respawn_at {}  rank {}",
            opt(p.is_alive),
            opt(p.health),
            opt(p.respawn_time),
            opt(p.packed_rank)
        );
        outln!(
            "  ult trained {}  cooldown {} -> {}",
            opt(p.ultimate_trained),
            opt(p.ultimate_cooldown_start),
            opt(p.ultimate_cooldown_end)
        );
        outln!(
            "  rejuvenator {}  rebirth {}  abandoned {}  flagged {}",
            opt(p.has_rejuvenator),
            opt(p.has_rebirth),
            opt(p.abandoned),
            opt(p.flagged_as_cheater)
        );
        match &p.items {
            Some(ids) => {
                let named: Vec<String> = ids
                    .iter()
                    .map(|&i| items.item_display_name(ItemId(i)))
                    .collect();
                outln!("  items ({}): {}", named.len(), named.join(", "));
            }
            // Distinct from "no items": the read did not complete this tick.
            None => outln!("  items: unreadable"),
        }

        let ups: Vec<String> = p
            .ability_upgrades
            .iter()
            .flatten()
            .map(|a| {
                let name = items.item_display_name(ItemId(a.item_id));
                if a.unlocked {
                    format!("{name} {}pt", a.points)
                } else {
                    format!("{name} (not learned)")
                }
            })
            .collect();
        outln!(
            "  abilities ({} pts spent): {}",
            p.spent_ability_points(),
            ups.join(", ")
        );
    }
    0
}

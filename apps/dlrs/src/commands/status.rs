//! `dlrs status`: attach and report what resolved.

use super::attach;

pub fn status() -> i32 {
    let Some(r) = attach() else { return 1 };

    outln!("pid                {}", r.pid());
    outln!("target abi         {}", r.abi());
    outln!(
        "client module      {:#018x} ({} bytes, {} readable)",
        r.client_base(),
        r.client_size(),
        r.image_bytes_read()
    );
    let g = r.globals();
    outln!("entity_system      {:#018x}", g.entity_system);
    outln!("entity_identity    {:#018x}", g.entity_identity_list);
    outln!("schema_system      {:#018x}", g.schema_system);

    match r.schema() {
        Some(s) => {
            outln!(
                "schema             resolved: {} classes, scopes: {}",
                s.class_count(),
                s.scopes.join(", ")
            );
            outln!(
                "schema layout      type_scopes @ {:#x}",
                s.layout.system_type_scopes
            );
            outln!(
                "schema enums       {} enums over {} enum-typed fields",
                s.enums.len(),
                s.field_enums.len()
            );
        }
        None => {
            outln!(
                "schema             UNRESOLVED ({}), using baked fallback offsets",
                r.schema_error()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| "unknown".into())
            );
        }
    }

    match r.entities() {
        Ok(e) => {
            outln!("entities           {}", e.len());
            for (name, n) in e.class_breakdown().into_iter().take(10) {
                outln!("  {n:>5}  {name}");
            }
        }
        Err(e) => outln!("entities           walk failed: {e}"),
    }

    match r.live_snapshot() {
        Ok(Some(s)) => {
            outln!("context            {}", s.describe());
            outln!(
                "state              {} (raw {:?}){}",
                s.game_state.map(|g| g.name()).unwrap_or("?"),
                s.game_state_raw,
                s.game_state_schema_name
                    .as_deref()
                    .map(|n| format!("  schema: {n}"))
                    .unwrap_or_default()
            );
            outln!(
                "match              id={:?} match_mode={} game_mode={} players={}",
                s.match_id,
                s.match_mode.map(|m| m.name()).unwrap_or("?"),
                s.game_mode.map(|m| m.name()).unwrap_or("?"),
                s.players.len()
            );
        }
        Ok(None) => outln!("match              no game rules - not in a lobby/match"),
        Err(e) => outln!("match              {e}"),
    }

    // Last, because drift is recorded lazily as fields are read; the entity walk and
    // the snapshot above are what exercise it.
    let drift = r.drift();
    if drift.is_empty() {
        outln!("integrity          clean: every field and enum agreed with the game");
    } else {
        let corrupt = drift.iter().filter(|d| d.is_corrupting()).count();
        outln!(
            "integrity          {} drift item(s), {corrupt} of which mean values are WRONG:",
            drift.len()
        );
        for d in &drift {
            outln!(
                "                     {} {d}",
                if d.is_corrupting() { "!!" } else { "  " }
            );
        }
    }
    0
}

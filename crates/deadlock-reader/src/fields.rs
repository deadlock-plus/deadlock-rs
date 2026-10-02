//! Every `(class, field)` pair this crate looks up, with a fallback offset for each.
//!
//! This table is the source of truth. `schema-fields.json` beside the crate root is a
//! machine-readable mirror of it for tools that will not parse Rust, and
//! `tests/schema_fields_json.rs` fails when the two stop agreeing.
//!
//! `fallback` is used *only* when the runtime schema lookup misses. These constants drift
//! with every Deadlock build. Always prefer a resolved schema offset; see
//! [`crate::Reader::offset_of`].
//!
//! **Baselined against build 6723** (`VersionDate=Sep 30 2026`, "City Never Sleeps"),
//! re-probed from a live client. Twelve rows drifted from build 6683 and were rebaselined:
//! the player controller's `m_PlayerDataGlobal` and `m_steamID`, the ability component and
//! damage-taken rows on the pawn, `CModifierProperty::m_vecModifiers`, the four timing
//! and slot rows on `C_CitadelBaseAbility`, and the three power rows on
//! `CCitadel_Modifier_HeroUpgradeBonuses`.
//!
//! The patch also replaced `STrooperFOWEntity`'s `m_nTeam` and `m_nPositionXY` with
//! `m_nPosX`, `m_nPosY` and `m_nFlags`, at `0x30`, `0x31` and `0x32`. Every Citadel
//! networked `S*`/`*_t` value type carries a `0x30` prologue before its first member, so
//! anything reading such a struct as a packed array must use the schema's offsets, not the
//! header's. The corroborating measurement is
//! [`crate::tunables::DEFAULT_ABILITY_UPGRADE_LAYOUT`], probed independently of the schema
//! and landing on exactly the same shape - stride `0x38`, members at `0x30` and `0x34`.
//!
//! Two `#[ignore]`d tests at the bottom of this file keep both properties honest against
//! a running client: every row names a field the schema has, and every baked fallback
//! equals the live offset.

/// One `(class, field)` pair this crate looks up, with its fallback offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    /// Schema class name, e.g. `"C_CitadelPlayerPawn"`.
    pub class: &'static str,
    /// Schema field name, e.g. `"m_iHealth"`.
    pub field: &'static str,
    /// Offset used only when the schema lookup misses.
    pub fallback: Option<u32>,
    /// True when the class name is resolved at runtime rather than read from a literal,
    /// so the pairing here is inferred from context rather than proven.
    pub inferred: bool,
}

/// Every `(class, field)` pair this crate is known to request.
///
/// MSVC/PE layout only. Use [`crate::abi::Abi::fallback_offset`] to pick by target ABI.
pub static WIN64_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        class: "CBaseModifier",
        field: "m_bDisabled",
        fallback: Some(0x71),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_flCreationTime",
        fallback: Some(0x30),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_flDuration",
        fallback: Some(0x34),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_hAbility",
        fallback: Some(0x3c),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_hCaster",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_iMaxStackCount",
        fallback: Some(0x64),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_iStackCount",
        fallback: Some(0x62),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_nAbilitySubclassID",
        fallback: Some(0x5c),
        inferred: false,
    },
    FieldSpec {
        class: "CBaseModifier",
        field: "m_nSerialNumber",
        fallback: Some(0x28),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelAbilityComponent",
        field: "m_vecAbilities",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelPlayerController",
        field: "m_PlayerDataGlobal",
        fallback: Some(0x908),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelPlayerController",
        field: "m_bIsLocalPlayerController",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelPlayerController",
        field: "m_iTeamNum",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelPlayerController",
        field: "m_steamID",
        fallback: Some(0x788),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelPlayerController",
        field: "m_unLobbyPlayerSlot",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelRecentDamage",
        field: "m_flLastDamageTime",
        fallback: Some(0x8),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelRecentDamage",
        field: "m_flStartTime",
        fallback: Some(0xc),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelRecentDamage",
        field: "m_hPlayerEntToStore",
        fallback: Some(0x14),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadelTrooperMinimap",
        field: "m_vecFOWEntities",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "CCitadel_Modifier_HeroUpgradeBonuses",
        field: "m_flArmorPower",
        fallback: Some(0x13c),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadel_Modifier_HeroUpgradeBonuses",
        field: "m_flTechPower",
        fallback: Some(0x140),
        inferred: false,
    },
    FieldSpec {
        class: "CCitadel_Modifier_HeroUpgradeBonuses",
        field: "m_flWeaponPower",
        fallback: Some(0x138),
        inferred: false,
    },
    FieldSpec {
        class: "CGameSceneNode",
        field: "m_vecAbsOrigin",
        fallback: Some(0xc8),
        inferred: false,
    },
    FieldSpec {
        class: "CModifierProperty",
        field: "m_vecModifiers",
        fallback: Some(0x40),
        inferred: false,
    },
    //
    // `inferred` because this class name is resolved at runtime, and it has to be:
    // three classes carry `m_vecTrackedStats` at the same 0x5f0, and only the per-player
    // one carries a slot. `m_nTeam` sits at a *different* offset on each, which is why
    // neither it nor the slot could carry a fallback while the class was a placeholder.
    // Offsets measured against build 6683; the schema is authoritative on later builds.
    FieldSpec {
        class: "CPlayerTrackedStatsEntity",
        field: "m_nPlayerSlot",
        fallback: Some(0x660),
        inferred: true,
    },
    FieldSpec {
        class: "CPlayerTrackedStatsEntity",
        field: "m_nTeam",
        fallback: Some(0x664),
        inferred: true,
    },
    FieldSpec {
        class: "CPlayerTrackedStatsEntity",
        field: "m_vecTrackedStats",
        fallback: Some(0x5f0),
        inferred: true,
    },
    FieldSpec {
        class: "CTeamTrackedStatsEntity",
        field: "m_nTeam",
        fallback: Some(0x660),
        inferred: true,
    },
    FieldSpec {
        class: "CTeamTrackedStatsEntity",
        field: "m_vecTrackedStats",
        fallback: Some(0x5f0),
        inferred: true,
    },
    FieldSpec {
        class: "C_BaseEntity",
        field: "m_iHealth",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_BaseEntity",
        field: "m_iMaxHealth",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_BaseEntity",
        field: "m_iTeamNum",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_BaseEntity",
        field: "m_pModifierProp",
        fallback: Some(0x348),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelBaseAbility",
        field: "m_eAbilitySlot",
        fallback: Some(0x77c),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelBaseAbility",
        field: "m_flCastCompletedTime",
        fallback: Some(0x770),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelBaseAbility",
        field: "m_flCooldownEnd",
        fallback: Some(0x76c),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelBaseAbility",
        field: "m_flCooldownStart",
        fallback: Some(0x768),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_bServerPaused",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_eGameState",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_flGameStartTime",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_iAmberRejuvCount",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_iMidbossKillCount",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_iSapphireRejuvCount",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRules",
        field: "m_unMatchID",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelGameRulesProxy",
        field: "m_pGameRules",
        fallback: Some(0x5f0),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_CCitadelAbilityComponent",
        fallback: Some(0x1438),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_flSimulationTime",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_hController",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_iHealth",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_iMaxHealth",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_iTeamNum",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_nCurrencies",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_nLevel",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_nSpentCurrencies",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_pGameSceneNode",
        fallback: Some(0x330),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelPlayerPawn",
        field: "m_sPlayerDamageTaken",
        fallback: Some(0x13e0),
        inferred: false,
    },
    FieldSpec {
        class: "C_CitadelTeam",
        field: "m_nStreetBrawlScore",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_NPC_Boss_Tier2",
        field: "m_iLane",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "C_Team",
        field: "m_aPlayerControllers",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iDeaths",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iDenies",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iGoldNetWorth",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iHeroDamage",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iHeroHealing",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iLastHits",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iLevel",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iObjectiveDamage",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iPlayerAssists",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_iPlayerKills",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_nHeroID",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_tHeldItem",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "PlayerDataGlobal_t",
        field: "m_vecUpgrades",
        fallback: None,
        inferred: false,
    },
    FieldSpec {
        class: "STrooperFOWEntity",
        field: "m_nFlags",
        fallback: Some(0x32),
        inferred: false,
    },
    FieldSpec {
        class: "STrooperFOWEntity",
        field: "m_nPosX",
        fallback: Some(0x30),
        inferred: false,
    },
    FieldSpec {
        class: "STrooperFOWEntity",
        field: "m_nPosY",
        fallback: Some(0x31),
        inferred: false,
    },
];

/// Look up the hardcoded fallback offset for a `(class, field)` pair.
///
/// Returns `None` when the pair is unknown *or* known but without a fallback, in which
/// case the read is skipped rather than guessed.
pub fn win64_fallback_offset(class: &str, field: &str) -> Option<u32> {
    WIN64_FIELDS
        .iter()
        .find(|s| s.class == class && s.field == field)
        .and_then(|s| s.fallback)
}

/// Look up the full spec for a `(class, field)` pair.
pub fn spec(class: &str, field: &str) -> Option<&'static FieldSpec> {
    WIN64_FIELDS
        .iter()
        .find(|s| s.class == class && s.field == field)
}

/// All distinct class names referenced by [`WIN64_FIELDS`].
pub fn classes() -> impl Iterator<Item = &'static str> {
    let mut seen: Vec<&'static str> = WIN64_FIELDS.iter().map(|s| s.class).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        let mut pairs: Vec<_> = WIN64_FIELDS.iter().map(|s| (s.class, s.field)).collect();
        let before = pairs.len();
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(
            before,
            pairs.len(),
            "duplicate (class, field) rows in WIN64_FIELDS"
        );
    }

    #[test]
    fn known_anchor_offsets() {
        assert_eq!(
            win64_fallback_offset("C_CitadelGameRulesProxy", "m_pGameRules"),
            Some(0x5f0)
        );
        assert_eq!(
            win64_fallback_offset("CCitadelPlayerController", "m_PlayerDataGlobal"),
            Some(0x908)
        );
        assert_eq!(
            win64_fallback_offset("CCitadelPlayerController", "m_steamID"),
            Some(0x788)
        );
    }

    #[test]
    fn the_ability_component_carries_only_the_vector_field() {
        assert!(spec("CCitadelAbilityComponent", "m_vecAbilities").is_some());
        assert!(
            spec("CCitadelAbilityComponent", "m_hAbilities").is_none(),
            "m_hAbilities is not a field of any Deadlock class"
        );
    }

    /// Every `(class, field)` in the table exists in the running client's schema.
    ///
    /// The table is a *fallback*, so a row naming a field the game does not have fails
    /// silently: `offset_of` does a doomed lookup and moves on. This is the only check
    /// that catches it, and it needs a live client.
    ///
    /// Deliberately asks the schema rather than `offset_of`, because `offset_of` answers
    /// from the fallback when the schema misses — which is exactly the case being hunted.
    /// That distinction is what found the three `*TrackedStatsEntity` rows: one had a
    /// fallback and so looked fine, and its two siblings did not.
    ///
    /// `cargo test -p deadlock-reader -- --ignored the_whole_table`
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn the_whole_table_resolves_against_a_live_client() {
        let reader = crate::Reader::attach().expect("attach to deadlock.exe");
        let schema = reader.schema().expect("schema resolved");
        let mut missing = Vec::new();
        for s in WIN64_FIELDS {
            if schema.offset_of(s.class, s.field).is_none() {
                missing.push(format!("{}::{}", s.class, s.field));
            }
        }
        assert!(missing.is_empty(), "not in the live schema: {missing:?}");
    }

    /// Every baked fallback agrees with the offset the live schema reports.
    ///
    /// Separate from the row-exists check because it fails for a different reason: a
    /// stale offset is a wrong read, not a skipped one. The whole table is baselined a
    /// year behind the tracked build, and this is the check that says which rows that
    /// actually cost.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn every_fallback_agrees_with_the_live_schema() {
        let reader = crate::Reader::attach().expect("attach to deadlock.exe");
        let schema = reader.schema().expect("schema resolved");
        let mut stale = Vec::new();
        for s in WIN64_FIELDS {
            let (Some(baked), Some(live)) = (s.fallback, schema.offset_of(s.class, s.field)) else {
                continue;
            };
            if baked != live {
                stale.push(format!(
                    "{}::{} baked {baked:#x}, live {live:#x}",
                    s.class, s.field
                ));
            }
        }
        assert!(stale.is_empty(), "stale fallbacks: {stale:#?}");
    }

    #[test]
    fn scoreboard_fields_present() {
        for f in [
            "m_nHeroID",
            "m_iLevel",
            "m_iGoldNetWorth",
            "m_iPlayerKills",
            "m_iDeaths",
            "m_iPlayerAssists",
            "m_iLastHits",
            "m_iDenies",
            "m_iHeroDamage",
            "m_iObjectiveDamage",
            "m_iHeroHealing",
        ] {
            assert!(
                spec("PlayerDataGlobal_t", f).is_some(),
                "missing PlayerDataGlobal_t::{f}"
            );
        }
    }

    /// Every `(class, field)` pair this crate names is a pair the live schema has.
    ///
    /// [`WIN64_FIELDS`] is checked against the client by
    /// `the_whole_table_resolves_against_a_live_client`, but that table is only the
    /// *fallback* offsets. Most reads name their class and field inline -
    /// `obj.u8(BASE_ENTITY, "m_iTeamNum")`, `reader.field_vec3(node, "CGameSceneNode",
    /// "m_vecAbsOrigin")` - and nothing checked those at all. A misspelling there does not
    /// fail to compile and does not panic; it resolves to nothing and the value silently
    /// becomes `None`, which is indistinguishable from a field the client did not send.
    ///
    /// That is not hypothetical. Three field names taken on trust turned out not to exist
    /// (`ban_until`, `hero_info`, `ping_type`), and `m_flKothRadius` was read from the
    /// wrong parent for a while. This is the check that makes that class of mistake loud.
    ///
    /// # How it reads the source, and what it deliberately skips
    ///
    /// It scans this crate's own `.rs` files for the two-argument tail every field
    /// accessor shares: a class - either a `SCREAMING_CASE` constant defined in this
    /// crate, or a string literal - followed by a `"m_..."` field name. A class named by a
    /// runtime variable (`obj.i32(class, "m_iLane")`, where `class` is whatever entity is
    /// in hand) cannot be resolved statically and is skipped; those reads are correct by
    /// construction anyway, since the class comes from the entity itself.
    ///
    /// Skipping means this is a **lower bound** on coverage, not a complete audit, and it
    /// says so rather than implying it checked everything.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn every_field_name_in_this_crate_exists_in_the_live_schema() {
        use std::collections::{BTreeSet, HashMap};

        //
        fn strip_test_modules(body: &str) -> String {
            let mut kept: Vec<&str> = Vec::new();
            let mut skipping = false;
            for line in body.lines() {
                if !skipping && line == "#[cfg(test)]" {
                    skipping = true;
                    continue;
                }
                if skipping {
                    if line == "}" {
                        skipping = false;
                    }
                    continue;
                }
                kept.push(line);
            }
            kept.join("\n")
        }

        let reader = crate::Reader::attach().expect("attach");
        let schema = reader
            .schema()
            .expect("schema unresolved; try `dlrs probe-schema`");

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let body = std::fs::read_to_string(&path).expect("read source");
                    sources.push(strip_test_modules(&body));
                }
            }
        }
        assert!(sources.len() > 5, "only found {} sources", sources.len());
        let text = sources.join("\n");
        assert!(
            !text.contains("#[cfg(test)]"),
            "a test module survived stripping, so its synthetic class names would be              reported as defects"
        );

        let mut consts: HashMap<&str, &str> = HashMap::new();
        for (i, _) in text.match_indices("const ") {
            let rest = &text[i + 6..];
            let Some(colon) = rest.find(':') else {
                continue;
            };
            let name = rest[..colon].trim();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
            {
                continue;
            }
            let Some(eq) = rest.find('=') else { continue };
            let after = &rest[eq + 1..];
            let Some(q1) = after.find('"') else { continue };
            if after[..q1].contains(';') || after[..q1].contains('\n') {
                continue;
            }
            let Some(q2) = after[q1 + 1..].find('"') else {
                continue;
            };
            let value = &after[q1 + 1..q1 + 1 + q2];
            if value.starts_with('C') || value.starts_with("Player") {
                consts.insert(name, value);
            }
        }

        let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
        for (i, _) in text.match_indices("\"m_") {
            let field_start = i + 1;
            let Some(end) = text[field_start..].find('"') else {
                continue;
            };
            let field = &text[field_start..field_start + end];
            if !field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                continue;
            }
            let before = text[..i].trim_end();
            let Some(before) = before.strip_suffix(',') else {
                continue;
            };
            let before = before.trim_end();
            let class = if let Some(stripped) = before.strip_suffix('"') {
                match stripped.rfind('"') {
                    Some(q) => stripped[q + 1..].to_string(),
                    None => continue,
                }
            } else {
                let token: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_uppercase() || *c == '_' || c.is_ascii_digit())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                match consts.get(token.as_str()) {
                    Some(v) => (*v).to_string(),
                    None => continue,
                }
            };
            if class.starts_with('C') || class.starts_with("Player") {
                pairs.insert((class, field.to_string()));
            }
        }

        assert!(
            pairs.len() > 40,
            "the scanner found only {} pairs, which means it stopped matching the code \
             rather than that the code stopped naming fields",
            pairs.len()
        );

        let mut missing_class = Vec::new();
        let mut missing_field = Vec::new();
        for (class, field) in &pairs {
            match schema.classes.get(class.as_str()) {
                None => missing_class.push(format!("{class} (for {field})")),
                Some(info) => {
                    if !info.fields.contains_key(field.as_str()) {
                        missing_field.push(format!("{class}::{field}"));
                    }
                }
            }
        }

        assert!(
            missing_class.is_empty() && missing_field.is_empty(),
            "checked {} (class, field) pairs from {} source files\n\
             classes the live schema does not have: {missing_class:#?}\n\
             fields the live schema does not have:  {missing_field:#?}",
            pairs.len(),
            sources.len()
        );
    }
}

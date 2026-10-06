//! Drives `LiveSnapshot::build` end to end over a synthetic client.
//!
//! Everything else in the crate's test suite either checks pure decoding on hand-built
//! structs or walks the entity list. The layer in between, the one that turns bytes in
//! another process into a scoreboard, was only ever verified by running the CLI against a
//! real game. That is a poor place for the most intricate code in the crate to live
//! untested, so this file builds a fake schema system and entity system and asserts on
//! what comes out the far end.
//!
//! Building a schema rather than relying on the baked fallback table is deliberate: only
//! 30 of the 73 recovered field pairs carry a baked offset, and none of the scoreboard
//! ones do. Without a schema the snapshot assembles almost entirely `None`, which would
//! make for a test that passes while proving nothing.

use std::sync::Arc;

use deadlock_memory::mock::MockMemory;
use deadlock_reader::Reader;
use deadlock_reader::schema::SchemaLayout;
use deadlock_reader::snapshot::ObjectiveKind;

/// Bump-allocates objects into a sparse fake address space.
///
/// Each object is its own segment, sized generously so a whole-object read lands inside
/// one of them, which is how the real reader fetches a player controller.
struct Fixture {
    objects: std::collections::BTreeMap<u64, Vec<u8>>,
    next: u64,
}

const L: SchemaLayout = SchemaLayout::DEADLOCK;
const INSTANCE_SIZE: u64 = 0x1800;

impl Fixture {
    fn new() -> Self {
        Fixture {
            objects: std::collections::BTreeMap::new(),
            next: 0x1000_0000,
        }
    }

    /// Reserve a zeroed object.
    fn alloc(&mut self, size: u64) -> u64 {
        let at = self.next;
        self.objects.insert(at, vec![0u8; size as usize]);
        // Leave a gap so no two objects abut and a runaway read cannot slide from one
        // into the next without failing.
        self.next += size + 0x1000;
        at
    }

    fn cstr(&mut self, s: &str) -> u64 {
        let at = self.alloc(s.len() as u64 + 1);
        self.poke(at, 0, s.as_bytes());
        at
    }

    /// Overwrite bytes inside an already-allocated object.
    fn poke(&mut self, base: u64, off: u64, bytes: &[u8]) {
        let obj = self
            .objects
            .get_mut(&base)
            .expect("poke into a live object");
        let at = off as usize;
        obj[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Flush every object into a mock address space.
    fn finish(self, extra: &[(u64, Vec<u8>)]) -> MockMemory {
        let mut m = MockMemory::new(99);
        for (at, bytes) in self.objects {
            m.write(at, &bytes);
        }
        for (at, bytes) in extra {
            m.write(*at, bytes);
        }
        m
    }

    fn poke_u32(&mut self, base: u64, off: u64, v: u32) {
        self.poke(base, off, &v.to_le_bytes());
    }
    fn poke_u64(&mut self, base: u64, off: u64, v: u64) {
        self.poke(base, off, &v.to_le_bytes());
    }
    fn poke_u8(&mut self, base: u64, off: u64, v: u8) {
        self.poke(base, off, &[v]);
    }
    fn poke_f32(&mut self, base: u64, off: u64, v: f32) {
        self.poke(base, off, &v.to_le_bytes());
    }

    /// Lay down a `CSchemaClassBinding` and its field array.
    fn class(&mut self, name: &str, size: u32, base: Option<u64>, fields: &[(&str, u32)]) -> u64 {
        let name_ptr = self.cstr(name);
        let fields_ptr = self.alloc((fields.len().max(1) as u64) * L.field_stride);
        for (i, (fname, off)) in fields.iter().enumerate() {
            let fname_ptr = self.cstr(fname);
            let at = i as u64 * L.field_stride;
            self.poke_u64(fields_ptr, at + L.field_name, fname_ptr);
            // Null type pointer: this fixture has no enum-typed fields to recover.
            self.poke_u64(fields_ptr, at + L.field_type, 0);
            self.poke_u32(fields_ptr, at + L.field_offset, *off);
        }

        let binding = self.alloc(0x80);
        self.poke_u64(binding, L.class_name, name_ptr);
        self.poke_u32(binding, L.class_size, size);
        self.poke(
            binding,
            L.class_field_count,
            &(fields.len() as u16).to_le_bytes(),
        );
        self.poke_u64(binding, L.class_fields, fields_ptr);
        if let Some(base_binding) = base {
            let base_info = self.alloc(0x20);
            self.poke_u64(base_info, L.base_info_class, base_binding);
            self.poke_u64(binding, L.class_base_info, base_info);
        }
        binding
    }
}

/// Field offsets this fixture uses. Arbitrary but distinct, and inside `INSTANCE_SIZE`.
mod off {
    // C_BaseEntity
    pub const TEAM_NUM: u32 = 0x100;
    pub const HEALTH: u32 = 0x104;
    pub const MAX_HEALTH: u32 = 0x108;
    pub const SIM_TIME: u32 = 0x10c;
    /// Left null in the fixture: the pawns carry no modifier property.
    pub const MODIFIER_PROP: u32 = 0x110;
    // C_CitadelGameRules
    pub const GAME_STATE: u32 = 0x200;
    pub const MATCH_MODE: u32 = 0x204;
    pub const GAME_MODE: u32 = 0x208;
    pub const GAME_START: u32 = 0x210;
    pub const PAUSED_TICKS: u32 = 0x214;
    pub const GAME_PAUSED: u32 = 0x218;
    pub const MIDBOSS_KILLS: u32 = 0x21c;
    pub const MIDBOSS_SPAWN: u32 = 0x220;
    pub const AMBER_REJUV: u32 = 0x224;
    // Eight bytes wide, not four: `m_unMatchID` is a `MatchID_t`, which boxes a `uint64`.
    // Sat at 0x20c next to a four-byte neighbour until the read was widened, at which
    // point the neighbour's bits became the high half of every match id.
    pub const MATCH_ID: u32 = 0x228;
    // Deliberately past `read_object`'s 4 KiB cap, and still inside INSTANCE_SIZE. A
    // snapshot reads the whole rules object in one go now, sized from the schema; if that
    // sizing regressed to a fixed cap this field would silently read as absent.
    pub const SAPPHIRE_REJUV: u32 = 0x1500;
    pub const NOT_SCORED: u32 = 0x240;
    pub const EXPECTED_PLAYERS: u32 = 0x244;
    pub const DONT_UPLOAD: u32 = 0x248;
    // CCitadelPlayerController
    pub const CTRL_TEAM: u32 = 0x300;
    pub const LOBBY_SLOT: u32 = 0x304;
    pub const STEAM_ID: u32 = 0x308;
    pub const IS_LOCAL: u32 = 0x310;
    pub const PLAYER_NAME: u32 = 0x320;
    pub const CONNECTED: u32 = 0x3a0;
    pub const PDG: u32 = 0x400;
    // PlayerDataGlobal_t, relative to the embedded struct
    pub const HERO_ID: u32 = 0x00;
    pub const LEVEL: u32 = 0x04;
    pub const NET_WORTH: u32 = 0x08;
    pub const KILLS: u32 = 0x0c;
    pub const DEATHS: u32 = 0x10;
    pub const ASSISTS: u32 = 0x14;
    pub const LAST_HITS: u32 = 0x18;
    pub const DENIES: u32 = 0x1c;
    // C_CitadelPlayerPawn
    pub const PAWN_CONTROLLER: u32 = 0x500;
    pub const SCENE_NODE: u32 = 0x508;
    // CGameSceneNode
    pub const ABS_ORIGIN: u32 = 0x510;
    // C_CitadelTeam / C_Team
    pub const TEAM_SCORE: u32 = 0x600;
    pub const TEAM_NAME: u32 = 0x610;
    // C_CitadelGameRulesProxy
    pub const P_GAME_RULES: u32 = 0x700;
}

/// The classes the snapshot reads, plus filler to clear `MIN_PLAUSIBLE_CLASSES`.
fn build_schema(f: &mut Fixture) -> Vec<(String, u64)> {
    let base_entity = f.class(
        "C_BaseEntity",
        INSTANCE_SIZE as u32,
        None,
        &[
            ("m_iTeamNum", off::TEAM_NUM),
            ("m_iHealth", off::HEALTH),
            ("m_iMaxHealth", off::MAX_HEALTH),
            ("m_flSimulationTime", off::SIM_TIME),
            // Declared so a clean client stays clean under the `modifiers` feature: the
            // baked table carries a fallback for this field, and falling back to it is
            // reported as drift.
            ("m_pModifierProp", off::MODIFIER_PROP),
        ],
    );

    let mut out = vec![("C_BaseEntity".to_string(), base_entity)];
    let add = |name: &str, binding: u64, out: &mut Vec<(String, u64)>| {
        out.push((name.to_string(), binding));
    };

    let rules = f.class(
        "C_CitadelGameRules",
        INSTANCE_SIZE as u32,
        None,
        &[
            ("m_eGameState", off::GAME_STATE),
            ("m_eMatchMode", off::MATCH_MODE),
            ("m_eGameMode", off::GAME_MODE),
            ("m_unMatchID", off::MATCH_ID),
            ("m_flGameStartTime", off::GAME_START),
            ("m_nTotalPausedTicks", off::PAUSED_TICKS),
            ("m_bGamePaused", off::GAME_PAUSED),
            ("m_iMidbossKillCount", off::MIDBOSS_KILLS),
            ("m_tNextMidBossSpawnTime", off::MIDBOSS_SPAWN),
            ("m_iAmberRejuvCount", off::AMBER_REJUV),
            ("m_iSapphireRejuvCount", off::SAPPHIRE_REJUV),
            ("m_bMatchNotScored", off::NOT_SCORED),
            ("m_unExpectedPlayerCount", off::EXPECTED_PLAYERS),
            ("m_bDontUploadStats", off::DONT_UPLOAD),
        ],
    );
    add("C_CitadelGameRules", rules, &mut out);

    let proxy = f.class(
        "C_CitadelGameRulesProxy",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[("m_pGameRules", off::P_GAME_RULES)],
    );
    add("C_CitadelGameRulesProxy", proxy, &mut out);

    let base_ctrl = f.class(
        "CBasePlayerController",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[
            ("m_iszPlayerName", off::PLAYER_NAME),
            ("m_iConnected", off::CONNECTED),
        ],
    );
    add("CBasePlayerController", base_ctrl, &mut out);

    let ctrl = f.class(
        "CCitadelPlayerController",
        INSTANCE_SIZE as u32,
        Some(base_ctrl),
        &[
            ("m_iTeamNum", off::CTRL_TEAM),
            ("m_unLobbyPlayerSlot", off::LOBBY_SLOT),
            ("m_steamID", off::STEAM_ID),
            ("m_bIsLocalPlayerController", off::IS_LOCAL),
            ("m_PlayerDataGlobal", off::PDG),
        ],
    );
    add("CCitadelPlayerController", ctrl, &mut out);

    let pdg = f.class(
        "PlayerDataGlobal_t",
        0x200,
        None,
        &[
            ("m_nHeroID", off::HERO_ID),
            ("m_iLevel", off::LEVEL),
            ("m_iGoldNetWorth", off::NET_WORTH),
            ("m_iPlayerKills", off::KILLS),
            ("m_iDeaths", off::DEATHS),
            ("m_iPlayerAssists", off::ASSISTS),
            ("m_iLastHits", off::LAST_HITS),
            ("m_iDenies", off::DENIES),
        ],
    );
    add("PlayerDataGlobal_t", pdg, &mut out);

    let pawn = f.class(
        "C_CitadelPlayerPawn",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[
            ("m_hController", off::PAWN_CONTROLLER),
            // Read only under the `positions` feature, but the fixture defines it so a
            // full-feature run resolves everything and reports no drift.
            ("m_pGameSceneNode", off::SCENE_NODE),
        ],
    );
    add("C_CitadelPlayerPawn", pawn, &mut out);

    let scene = f.class(
        "CGameSceneNode",
        INSTANCE_SIZE as u32,
        None,
        &[("m_vecAbsOrigin", off::ABS_ORIGIN)],
    );
    add("CGameSceneNode", scene, &mut out);

    let team = f.class(
        "C_CitadelTeam",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[("m_iScore", off::TEAM_SCORE)],
    );
    add("C_CitadelTeam", team, &mut out);

    let c_team = f.class(
        "C_Team",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[("m_szTeamname", off::TEAM_NAME)],
    );
    add("C_Team", c_team, &mut out);

    let walker = f.class(
        "C_NPC_Boss_Tier2",
        INSTANCE_SIZE as u32,
        Some(base_entity),
        &[],
    );
    add("C_NPC_Boss_Tier2", walker, &mut out);

    // Filler. The walk rejects a layout yielding fewer than 200 classes, on the grounds
    // that a real client has thousands and a near-empty result means a wrong offset.
    for i in 0..220 {
        let name = format!("C_Filler{i:03}");
        let b = f.class(&name, 0x40, None, &[("m_nUnused", 0x10)]);
        out.push((name, b));
    }
    out
}

/// Wire the class bindings into a scope's bucket array and point the schema system at it.
fn build_schema_system(f: &mut Fixture, classes: &[(String, u64)]) -> u64 {
    let scope = f.alloc(L.scope_class_buckets + L.bucket_count * L.bucket_stride + 0x40);
    let scope_name = f.cstr("client.dll");
    f.poke_u64(scope, L.scope_name, scope_name);

    // One chain per bucket, classes spread across them.
    let mut chains: Vec<Vec<u64>> = vec![Vec::new(); L.bucket_count as usize];
    for (i, (_, binding)) in classes.iter().enumerate() {
        chains[i % L.bucket_count as usize].push(*binding);
    }
    for (b, chain) in chains.iter().enumerate() {
        let mut next = 0u64;
        // Build the chain back to front so each entry can point at its successor.
        for binding in chain.iter().rev() {
            let entry = f.alloc(0x20);
            f.poke_u64(entry, L.tshash_entry_next, next);
            f.poke_u64(entry, L.tshash_entry_data, *binding);
            next = entry;
        }
        let bucket = L.scope_class_buckets + b as u64 * L.bucket_stride;
        f.poke_u64(scope, bucket + L.bucket_head, next);
        f.poke_u64(scope, bucket + L.bucket_head_uncommitted, 0);
    }

    let scope_array = f.alloc(0x40);
    f.poke_u64(scope_array, 0, scope);

    let system = f.alloc(L.system_type_scopes + 0x40);
    f.poke_u32(system, L.system_type_scopes, 1);
    f.poke_u64(system, L.system_type_scopes + 8, scope_array);
    system
}

/// One entity to place in the fake entity list.
struct Ent {
    class: &'static str,
    instance: u64,
}

/// Lay out the entity system: chunk table, identities, class-name plumbing.
fn build_entity_system(f: &mut Fixture, ents: &[Ent]) -> u64 {
    let layout = deadlock_reader::entity::EntityLayout::DEADLOCK;
    let per_chunk = 1u64 << layout.chunk_shift;
    let chunk = f.alloc(per_chunk * layout.identity_stride);

    for (slot, e) in ents.iter().enumerate() {
        // Each class needs a CEntityClass -> info -> name chain.
        let name_ptr = f.cstr(e.class);
        let info = f.alloc(0x20);
        f.poke_u64(info, layout.class_info_name, name_ptr);
        let class_ptr = f.alloc(0x20);
        f.poke_u64(class_ptr, layout.class_info_ptr, info);
        let designer = f.cstr("designer");

        let at = slot as u64 * layout.identity_stride;
        f.poke_u64(chunk, at + layout.identity_instance, e.instance);
        f.poke_u64(chunk, at + layout.identity_class, class_ptr);
        f.poke_u32(chunk, at + layout.identity_handle, slot as u32 | 0x8000);
        f.poke_u64(chunk, at + layout.identity_designer_name, designer);
    }

    let system = f.alloc(layout.chunk_table_base + 0x200);
    f.poke_u64(system, layout.chunk_table_base, chunk);
    system
}

/// A complete synthetic client: schema, entities, and a two-player ranked match.
fn synthetic_match() -> Reader {
    synthetic_match_with_rules_schema(true)
}

/// The synthetic client, optionally with the rules proxy hidden from the live schema.
///
/// `m_pGameRules` is in `UNSAFE_FALLBACKS`, so with no schema entry the baked offset is
/// refused rather than guessed and the pointer reads as absent - which is the exact
/// condition that used to be reported as "not in a match".
fn synthetic_match_with_rules_schema(rules_in_schema: bool) -> Reader {
    let mut f = Fixture::new();

    let classes = build_schema(&mut f);
    let classes: Vec<(String, u64)> = if rules_in_schema {
        classes
    } else {
        classes
            .into_iter()
            .filter(|(n, _)| n != "C_CitadelGameRulesProxy")
            .collect()
    };
    let schema_system = build_schema_system(&mut f, &classes);

    // --- the match ------------------------------------------------------------
    let rules = f.alloc(INSTANCE_SIZE);
    f.poke_u32(rules, off::GAME_STATE as u64, 7); // GameInProgress
    f.poke_u32(rules, off::MATCH_MODE as u64, 4); // Ranked
    f.poke_u32(rules, off::GAME_MODE as u64, 1); // Normal
    f.poke_u64(rules, off::MATCH_ID as u64, 98_098_971);
    f.poke_f32(rules, off::GAME_START as u64, 100.0);
    f.poke_u32(rules, off::PAUSED_TICKS as u64, 64); // one second of pause
    f.poke_u8(rules, off::GAME_PAUSED as u64, 0);
    f.poke_u32(rules, off::MIDBOSS_KILLS as u64, 2);
    f.poke_f32(rules, off::MIDBOSS_SPAWN as u64, 900.0);
    f.poke_u32(rules, off::AMBER_REJUV as u64, 1);
    f.poke_u32(rules, off::SAPPHIRE_REJUV as u64, 3);

    let proxy = f.alloc(INSTANCE_SIZE);
    f.poke_u64(proxy, off::P_GAME_RULES as u64, rules);
    f.poke_f32(proxy, off::SIM_TIME as u64, 800.0);

    // Two players, one per side.
    let mut controllers = Vec::new();
    for (team, slot, steam, name, hero, kills, deaths, worth, local) in [
        (
            2u8,
            1u8,
            7_600_001u64,
            "alice",
            15u32,
            7u32,
            2u32,
            30_000u32,
            true,
        ),
        (3, 7, 7_600_002, "bob", 31, 2, 7, 21_000, false),
    ] {
        let c = f.alloc(INSTANCE_SIZE);
        f.poke_u8(c, off::CTRL_TEAM as u64, team);
        f.poke_u8(c, off::LOBBY_SLOT as u64, slot);
        f.poke_u64(c, off::STEAM_ID as u64, steam);
        f.poke_u8(c, off::IS_LOCAL as u64, local as u8);
        f.poke(c, off::PLAYER_NAME as u64, name.as_bytes());
        f.poke_u32(c, off::CONNECTED as u64, 0); // connected
        let p = off::PDG as u64;
        f.poke_u32(c, p + off::HERO_ID as u64, hero);
        f.poke_u32(c, p + off::LEVEL as u64, 24); // shows as 23
        f.poke_u32(c, p + off::NET_WORTH as u64, worth);
        f.poke_u32(c, p + off::KILLS as u64, kills);
        f.poke_u32(c, p + off::DEATHS as u64, deaths);
        f.poke_u32(c, p + off::ASSISTS as u64, 5);
        f.poke_u32(c, p + off::LAST_HITS as u64, 100);
        f.poke_u32(c, p + off::DENIES as u64, 10);
        f.poke_f32(c, off::SIM_TIME as u64, 810.0);
        controllers.push(c);
    }

    // A pawn for the first player only, so the "no pawn" path is covered too.
    let pawn = f.alloc(INSTANCE_SIZE);
    f.poke_u32(pawn, off::PAWN_CONTROLLER as u64, 0x8000); // handle of slot 0
    f.poke_u32(pawn, off::HEALTH as u64, 800);
    f.poke_u32(pawn, off::MAX_HEALTH as u64, 1000);
    f.poke_f32(pawn, off::SIM_TIME as u64, 812.5);

    // Two team entities, so team names and scores resolve.
    let mut teams = Vec::new();
    for (num, name, score) in [(2u8, "Amber", 15i32), (3, "Sapphire", 31)] {
        let t = f.alloc(INSTANCE_SIZE);
        f.poke_u8(t, off::TEAM_NUM as u64, num);
        f.poke_u32(t, off::TEAM_SCORE as u64, score as u32);
        f.poke(t, off::TEAM_NAME as u64, name.as_bytes());
        teams.push(t);
    }

    // One standing walker and one destroyed, both Amber.
    let walker_up = f.alloc(INSTANCE_SIZE);
    f.poke_u8(walker_up, off::TEAM_NUM as u64, 2);
    f.poke_u32(walker_up, off::HEALTH as u64, 4000);
    f.poke_u32(walker_up, off::MAX_HEALTH as u64, 4000);
    let walker_dead = f.alloc(INSTANCE_SIZE);
    f.poke_u8(walker_dead, off::TEAM_NUM as u64, 2);
    f.poke_u32(walker_dead, off::HEALTH as u64, 0);
    f.poke_u32(walker_dead, off::MAX_HEALTH as u64, 4000);

    // Slot order matters: the pawn's controller handle is 0x8000, i.e. slot 0.
    let ents = vec![
        Ent {
            class: "CCitadelPlayerController",
            instance: controllers[0],
        },
        Ent {
            class: "CCitadelPlayerController",
            instance: controllers[1],
        },
        Ent {
            class: "C_CitadelGameRulesProxy",
            instance: proxy,
        },
        Ent {
            class: "C_CitadelPlayerPawn",
            instance: pawn,
        },
        Ent {
            class: "C_CitadelTeam",
            instance: teams[0],
        },
        Ent {
            class: "C_CitadelTeam",
            instance: teams[1],
        },
        Ent {
            class: "C_NPC_Boss_Tier2",
            instance: walker_up,
        },
        Ent {
            class: "C_NPC_Boss_Tier2",
            instance: walker_dead,
        },
    ];
    let entity_system = build_entity_system(&mut f, &ents);

    into_reader(f, entity_system, schema_system)
}

/// A fake `client.dll` carrying the three signatures, with the globals aimed at the
/// systems this fixture built.
///
/// Going through real signature resolution rather than injecting globals keeps the test
/// honest: it exercises the same path a real attach takes, and it needs no constructor
/// that exists only for tests.
fn into_reader(f: Fixture, entity_system: u64, schema_system: u64) -> Reader {
    use deadlock_memory::sig::Pattern;
    use deadlock_reader::globals::{Globals, SIGNATURES};

    const BASE: u64 = 0x7F00_0000_0000;
    const SIZE: usize = 0x4000;
    let mut image = vec![0xCCu8; SIZE];
    image[0] = 0x4D;
    image[1] = 0x5A;

    let mut at = 0x100usize;
    for (i, d) in SIGNATURES.iter().enumerate() {
        let pat = Pattern::parse(d.pattern).unwrap();
        let mut bytes = vec![0u8; pat.len()];
        for (j, tok) in d.pattern.split_whitespace().enumerate() {
            if tok != "??" && tok != "?" {
                bytes[j] = u8::from_str_radix(tok, 16).unwrap();
            }
        }
        // Aim each reference at its own scratch slot late in the image.
        let want = 0x3000u64 + i as u64 * 8;
        let next = (at + d.instr_len) as u64;
        let disp = (want as i64 - next as i64) as i32;
        bytes[d.disp_off..d.disp_off + 4].copy_from_slice(&disp.to_le_bytes());
        image[at..at + bytes.len()].copy_from_slice(&bytes);
        at += 0x200;
    }

    // Resolve first, then fill the slots the resolver landed on. Which signature owns
    // which slot is the resolver's business, not this test's.
    let g = Globals::resolve(&image, BASE, SIZE).expect("signatures resolve");
    for (va, value) in [
        (g.entity_system, entity_system),
        (g.schema_system, schema_system),
    ] {
        let off = (va - BASE) as usize;
        image[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    let mut mem = f.finish(&[(BASE, image)]);
    mem.add_module("client.dll", BASE, SIZE);
    Reader::with_memory(Arc::new(mem), "client.dll").expect("reader builds over the fixture")
}

#[test]
fn the_schema_walk_succeeds_and_flattens_inherited_fields() {
    let r = synthetic_match();
    let s = r
        .schema()
        .expect("the walk must succeed over a plausible fixture");
    assert!(s.class_count() >= 200);

    assert_eq!(
        s.offset_of("CCitadelPlayerController", "m_steamID"),
        Some(off::STEAM_ID)
    );
    assert_eq!(
        s.offset_of("CCitadelPlayerController", "m_flSimulationTime"),
        Some(off::SIM_TIME)
    );
    assert!(
        r.offset_is_from_schema("C_CitadelGameRules", "m_unMatchID"),
        "offsets must come from the fixture schema, not the baked table"
    );
}

/// A match id past `u32::MAX` survives the read.
///
/// `m_unMatchID` is a `MatchID_t`, which the schema declares as a boxed `uint64`; the
/// `un` in the name is not the width. Reading it as a `u32` is a narrowing that happens
/// to be lossless while live ids sit around 4e7, and a truncated match id is a number
/// that looks entirely plausible.
#[test]
fn a_match_id_wider_than_thirty_two_bits_is_not_truncated() {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);
    let (entity_system, rules) = build_minimal_match(&mut f);

    let huge = 0x1_0000_0000_u64 | 98_098_971;
    f.poke_u64(rules, off::MATCH_ID as u64, huge);

    let r = into_reader(f, entity_system, schema_system);
    let s = r.live_snapshot().expect("read").expect("a match");

    assert_eq!(s.match_id, Some(huge));
    assert_ne!(
        s.match_id,
        Some(98_098_971),
        "the low half alone means the high half was dropped"
    );
}

#[test]
fn a_snapshot_assembles_the_match_state() {
    let r = synthetic_match();
    let s = r.live_snapshot().expect("read").expect("a match");

    assert_eq!(s.match_id, Some(98_098_971));
    assert_eq!(s.describe(), "Ranked / Normal");
    assert!(s.is_match() && s.is_ranked() && !s.is_hideout());
    assert_eq!(s.midboss_kills, Some(2));
    assert_eq!(s.amber_rejuv, Some(1));
    //
    assert_eq!(
        s.sapphire_rejuv,
        Some(3),
        "a field above the object cap must still read"
    );
    assert_eq!(s.paused, Some(false));
}

#[test]
fn the_scoreboard_maps_every_field_to_the_right_player() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    let rows: Vec<_> = s.scoreboard().collect();
    assert_eq!(rows.len(), 2, "two playing sides, one player each");

    let alice = rows.iter().find(|p| p.slot == Some(1)).expect("slot 1");
    assert_eq!(alice.name.as_deref(), Some("alice"));
    assert_eq!(alice.steam_id, Some(7_600_001));
    assert_eq!(alice.kills, Some(7));
    assert_eq!(alice.deaths, Some(2));
    assert_eq!(alice.net_worth, Some(30_000));
    assert_eq!(alice.hero_id.map(|h| h.get()), Some(15));
    assert_eq!(alice.level_raw, Some(24));
    assert_eq!(alice.level, Some(23));
    assert_eq!(alice.is_local, Some(true));

    let bob = rows.iter().find(|p| p.slot == Some(7)).expect("slot 7");
    assert_eq!(bob.name.as_deref(), Some("bob"));
    assert_eq!(bob.kills, Some(2));
    assert_eq!(bob.hero_id.map(|h| h.get()), Some(31));
    assert_ne!(alice.team, bob.team, "they are on opposite sides");
}

#[test]
fn a_pawn_is_joined_to_its_controller_by_handle() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    let alice = s.scoreboard().find(|p| p.slot == Some(1)).unwrap();
    assert_eq!(alice.health, Some(800), "health comes from the pawn");
    assert_eq!(alice.max_health, Some(1000));
    assert!(alice.pawn.is_some());
    assert!(alice.alive());

    let bob = s.scoreboard().find(|p| p.slot == Some(7)).unwrap();
    assert_eq!(bob.health, None, "no pawn means unknown, not dead");
    assert!(bob.pawn.is_none());
}

#[test]
fn team_totals_come_from_the_players_on_each_side() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    let amber = s.team_stats(deadlock_reader::Team::AMBER).expect("amber");
    assert_eq!(amber.players, 1);
    assert_eq!(amber.souls, 30_000);
    assert_eq!(amber.kills, 7);
    assert_eq!(amber.score, Some(15), "read from C_CitadelTeam::m_iScore");
    assert_eq!(amber.rejuvenators, Some(1));
    assert_eq!(amber.structures_alive, 1);

    assert_eq!(s.soul_lead(), Some((deadlock_reader::Team::AMBER, 9_000)));
    assert_eq!(s.team_name(deadlock_reader::Team::AMBER), "Amber");
    assert_eq!(s.team_name(deadlock_reader::Team::SAPPHIRE), "Sapphire");
}

#[test]
fn objectives_include_the_destroyed_one_with_zero_health() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    let walkers: Vec<_> = s.structures(ObjectiveKind::Walker).collect();
    assert_eq!(walkers.len(), 2, "both walkers are listed");
    assert!(walkers.iter().any(|o| o.health == Some(4000)));
    assert!(
        walkers.iter().any(|o| o.health == Some(0)),
        "a destroyed structure stays in the list at zero health"
    );
}

#[test]
fn the_clock_takes_the_highest_simulation_time() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    assert_eq!(s.clock.now, Some(812.5));
    assert_eq!(s.clock.game_start_time, Some(100.0));
    assert_eq!(s.clock.elapsed_seconds(), Some(712.5));
    assert_eq!(s.clock.playing_seconds(), Some(711.5));
    assert_eq!(s.timers.match_time, Some(711.5));
}

#[test]
fn the_midboss_timer_counts_from_the_engine_clock() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();

    assert!(!s.midboss_alive(), "no midboss entity in the fixture");
    let left = s.seconds_until_midboss().expect("a countdown");
    assert!((left - 87.5).abs() < 0.01, "{left}");
}

/// A rules proxy that is present but unreadable is a broken read, not an absent match.
///
/// Both used to be `None`. A rename of this one field therefore made the whole library
/// report "not in a match" forever, silently, with no drift signal either - drift is only
/// attached to a snapshot that was actually built.
#[test]
fn an_unreadable_rules_pointer_is_an_error_not_an_absent_match() {
    let r = synthetic_match_with_rules_schema(false);
    let err = r
        .live_snapshot()
        .expect_err("the proxy is present, so this is a failure to read it");
    let text = err.to_string();
    assert!(
        text.contains("m_pGameRules"),
        "the error should name the field that failed: {text}"
    );
}

#[test]
fn a_client_with_no_game_rules_yields_no_snapshot() {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);
    let lone = f.alloc(INSTANCE_SIZE);
    let entity_system = build_entity_system(
        &mut f,
        &[Ent {
            class: "C_CitadelTeam",
            instance: lone,
        }],
    );
    let r = into_reader(f, entity_system, schema_system);
    assert!(r.live_snapshot().unwrap().is_none());
}

/// A healthy client must report nothing, or the signal is noise.
#[test]
fn a_matching_schema_reports_no_drift() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();
    assert!(
        s.drift.is_empty(),
        "clean client, unexpected drift: {:?}",
        s.drift
    );
    assert!(!r.has_drifted());
    assert!(!r.is_reading_corrupt_data());
}

/// The failure this whole mechanism exists for: a renumbered enum. The game says one
/// thing, the baked mapping says another, and the decoded variant is simply wrong.
#[test]
fn a_renumbered_enum_is_caught_and_reported() {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system =
        build_schema_system_with_game_state_enum(&mut f, &classes, 7, "EGameState_HeroSelection");
    let (entity_system, _) = build_minimal_match(&mut f);
    let r = into_reader(f, entity_system, schema_system);

    let s = r.live_snapshot().unwrap().expect("a match");
    let mismatch = s.drift.iter().find_map(|d| match d {
        deadlock_reader::Drift::EnumMismatch {
            raw, schema, baked, ..
        } => Some((*raw, schema.clone(), baked.clone())),
        _ => None,
    });
    let (raw, schema, baked) = mismatch.expect("the disagreement must be reported");
    assert_eq!(raw, 7);
    assert_eq!(schema, "EGameState_HeroSelection");
    assert_eq!(baked, "GameInProgress");
    assert!(
        r.is_reading_corrupt_data(),
        "a wrong game state is corrupting, not merely absent"
    );
}

/// A `C_CitadelGameRules` whose `m_eGameState` carries a real enum binding, so the walk
/// recovers an enumerator name for `raw` and the reader can compare it against its own
/// mapping.
fn build_schema_system_with_game_state_enum(
    f: &mut Fixture,
    classes: &[(String, u64)],
    raw: u32,
    enumerator: &str,
) -> u64 {
    // CSchemaEnumBinding: name, size, count, and an array of {name, value}.
    let enum_name = f.cstr("EGameState");
    let values = f.alloc(L.enum_value_stride * 2);
    let val_name = f.cstr(enumerator);
    f.poke_u64(values, L.enum_value_name, val_name);
    f.poke_u64(values, L.enum_value_value, raw as u64);

    let binding = f.alloc(0x40);
    f.poke_u64(binding, L.enum_name, enum_name);
    f.poke_u8(binding, L.enum_size, 4);
    f.poke_u32(binding, L.enum_count, 1);
    f.poke_u64(binding, L.enum_values, values);

    // CSchemaType: its own name must match the binding's, which is how the walk rejects
    // non-enum types that happen to sit at that offset.
    let ty = f.alloc(0x40);
    f.poke_u64(ty, L.type_name, enum_name);
    f.poke_u64(ty, L.type_enum_binding, binding);

    // Point m_eGameState's field record at that type.
    let rules_binding = classes
        .iter()
        .find(|(n, _)| n == "C_CitadelGameRules")
        .map(|(_, b)| *b)
        .expect("rules class");
    let fields_ptr = {
        let obj = f.objects.get(&rules_binding).expect("binding");
        u64::from_le_bytes(
            obj[L.class_fields as usize..L.class_fields as usize + 8]
                .try_into()
                .unwrap(),
        )
    };
    // m_eGameState is the first field declared on the class.
    f.poke_u64(fields_ptr, L.field_type, ty);

    build_schema_system(f, classes)
}

/// The smallest world that still yields a snapshot: a rules proxy and nothing else.
fn build_minimal_match(f: &mut Fixture) -> (u64, u64) {
    let rules = f.alloc(INSTANCE_SIZE);
    f.poke_u32(rules, off::GAME_STATE as u64, 7); // GameInProgress
    f.poke_u64(rules, off::MATCH_ID as u64, 1234);
    let proxy = f.alloc(INSTANCE_SIZE);
    f.poke_u64(proxy, off::P_GAME_RULES as u64, rules);
    let entity_system = build_entity_system(
        f,
        &[Ent {
            class: "C_CitadelGameRulesProxy",
            instance: proxy,
        }],
    );
    (entity_system, rules)
}

/// A rules proxy that is absent means the client is loading, which is not the same answer
/// as a missing game.
#[test]
fn a_client_with_no_game_rules_is_loading_not_absent() {
    use deadlock_reader::snapshot::LiveState;

    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);
    let lone = f.alloc(INSTANCE_SIZE);
    let entity_system = build_entity_system(
        &mut f,
        &[Ent {
            class: "C_CitadelTeam",
            instance: lone,
        }],
    );
    let r = into_reader(f, entity_system, schema_system);

    match r.live_state().expect("a read, not a failure") {
        LiveState::Loading(l) => assert_eq!(l.entity_count, 1),
        other => panic!("expected Loading, got {other:?}"),
    }
    assert!(
        r.live_snapshot().unwrap().is_none(),
        "the old call is unchanged"
    );
}

#[test]
fn a_client_with_game_rules_is_live() {
    let r = synthetic_match();
    let state = r.live_state().expect("read");
    assert!(!state.is_loading());
    let snap = state.into_snapshot().expect("a snapshot");
    assert_eq!(snap.match_id, Some(98_098_971));
}

/// A rules proxy plus extra entities of the given classes, with no match id.
fn offline_client(extra: &[&'static str]) -> Reader {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);

    let rules = f.alloc(INSTANCE_SIZE);
    f.poke_u32(rules, off::GAME_STATE as u64, 7);
    let proxy = f.alloc(INSTANCE_SIZE);
    f.poke_u64(proxy, off::P_GAME_RULES as u64, rules);

    let mut ents = vec![Ent {
        class: "C_CitadelGameRulesProxy",
        instance: proxy,
    }];
    for class in extra {
        let instance = f.alloc(INSTANCE_SIZE);
        ents.push(Ent { class, instance });
    }
    let entity_system = build_entity_system(&mut f, &ents);
    into_reader(f, entity_system, schema_system)
}

#[test]
fn the_sandbox_is_read_off_the_entities_it_loads() {
    use deadlock_reader::snapshot::Context;

    let r = offline_client(&[
        "CCitadelHideoutTeleportTrigger",
        "CCitadelTunnelTrigger",
        "CCitadel_ShopProp",
    ]);
    let s = r.live_snapshot().unwrap().unwrap();
    assert_eq!(s.match_id, None);
    assert_eq!(s.context, Context::Sandbox);
}

#[test]
fn explore_nyc_is_read_off_the_entities_it_loads() {
    use deadlock_reader::snapshot::Context;

    let r = offline_client(&["CCitadelTriggerCapturePoint", "C_NPC_BarrackBoss"]);
    let s = r.live_snapshot().unwrap().unwrap();
    assert_eq!(s.context, Context::ExploreNyc);
}

#[test]
fn a_retuned_class_list_changes_what_counts_as_the_sandbox() {
    use deadlock_reader::snapshot::Context;

    let mut r = offline_client(&["CSandboxOnlyNextPatch"]);
    assert_eq!(r.live_snapshot().unwrap().unwrap().context, Context::Other);
    r.tunables_mut().sandbox_classes = vec!["CSandboxOnlyNextPatch".to_string()];
    assert_eq!(
        r.live_snapshot().unwrap().unwrap().context,
        Context::Sandbox
    );
}

#[test]
fn the_hero_menu_shows_up_as_a_snapshot_fact() {
    let mut extra = vec!["C_PointCamera"; 14];
    extra.push("C_PortraitWorldUnit");
    let s = offline_client(&extra).live_snapshot().unwrap().unwrap();
    assert!(s.menu.hero_menu_open && s.menu.menu_open);
    assert_eq!((s.menu.portrait_units, s.menu.point_cameras), (1, 14));

    let quiet = offline_client(&["C_PointCamera"; 6])
        .live_snapshot()
        .unwrap()
        .unwrap();
    assert!(!quiet.menu.menu_open && !quiet.menu.hero_menu_open);
}

#[test]
fn bots_are_flagged_and_humans_are_not() {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);
    let (_, rules) = build_minimal_match(&mut f);
    let proxy = f.alloc(INSTANCE_SIZE);
    f.poke_u64(proxy, off::P_GAME_RULES as u64, rules);

    let mut ents = vec![Ent {
        class: "C_CitadelGameRulesProxy",
        instance: proxy,
    }];
    for (name, steam) in [("Bot3", 0u64), ("Bot4", 0), ("Bot5", 7_600_009)] {
        let c = f.alloc(INSTANCE_SIZE);
        f.poke_u8(c, off::CTRL_TEAM as u64, 2);
        f.poke_u64(c, off::STEAM_ID as u64, steam);
        f.poke(c, off::PLAYER_NAME as u64, name.as_bytes());
        ents.push(Ent {
            class: "CCitadelPlayerController",
            instance: c,
        });
    }
    let entity_system = build_entity_system(&mut f, &ents);
    let r = into_reader(f, entity_system, schema_system);

    let s = r.live_snapshot().unwrap().unwrap();
    let flag = |name: &str| {
        s.players
            .iter()
            .find(|p| p.name.as_deref() == Some(name))
            .map(|p| p.is_bot)
    };
    assert_eq!(flag("Bot3"), Some(true));
    assert_eq!(flag("Bot4"), Some(true));
    assert_eq!(flag("Bot5"), Some(false), "a Steam id makes it a person");
}

#[test]
fn the_rules_flags_are_exposed_on_the_snapshot() {
    let mut f = Fixture::new();
    let classes = build_schema(&mut f);
    let schema_system = build_schema_system(&mut f, &classes);
    let (entity_system, rules) = build_minimal_match(&mut f);
    f.poke_u8(rules, off::NOT_SCORED as u64, 1);
    f.poke_u32(rules, off::EXPECTED_PLAYERS as u64, 12);
    f.poke_u8(rules, off::DONT_UPLOAD as u64, 1);
    let r = into_reader(f, entity_system, schema_system);

    let s = r.live_snapshot().unwrap().unwrap();
    assert_eq!(s.match_not_scored, Some(true));
    assert_eq!(s.expected_player_count, Some(12));
    assert_eq!(s.dont_upload_stats, Some(true));
}

#[test]
fn unset_rules_flags_read_as_false_and_zero_not_absent() {
    let r = synthetic_match();
    let s = r.live_snapshot().unwrap().unwrap();
    assert_eq!(s.match_not_scored, Some(false));
    assert_eq!(s.dont_upload_stats, Some(false));
    assert_eq!(s.expected_player_count, Some(0));
}

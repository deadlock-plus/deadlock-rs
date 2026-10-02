# Deadlock memory layout

A reference for how a running Deadlock client lays out the state this crate reads: the
Source 2 schema system, the entity system, the game-rules enums, the Game Coordinator
objects that sit in the heap, and the replay metadata published after a match.

It is also the provenance record for every constant `deadlock-reader` depends on. It says
where each number came from, how it is re-verified against the live runtime schema, and
which ones drift with a game build.

Nothing here is a build artefact of the reader. Offsets that the game exposes through its
own reflection data are resolved at attach and are *not* baked in; the tables below are
the fallback for a lookup miss and the documentation for what the walk is walking.

---

## 1. Overview

Match data reachable from a running client falls into three groups, each with a different
mechanism and a different window of availability.

| Source | Available when | Mechanism |
|---|---|---|
| Live entity state | In a match, or in the Hideout | Source 2 schema-driven reads of `deadlock.exe`'s `client.dll` |
| Lobby, party, account, post-game metadata | Client running, in or out of a match | `deadlock-walker` finds live Game Coordinator protobuf objects in the heap by RTTI vtable |
| Historical / post-match | After a match | Replay salts → Valve replay CDN |

The first is §4–§6, the second §7, the third §8.

The three are not interchangeable. The schema and entity systems model the *world*; a
class-by-class sweep of a live client's 3605-class schema finds no party, lobby or account
membership anywhere — the only field whose name even contains "party" is
`CCitadelPlayerController::m_bInPartyChat`, a chat-channel flag
(see `deadlock-walker`).

## 2. Provenance and confidence

Constants come from two places, and the code says which is which.

**Resolved at runtime.** Field offsets come from the game's own schema system, walked on
every attach (§4.5). This is the central design decision: a reader built this way survives
a Deadlock patch instead of breaking on it.

**Measured against a live client.** The schema system's and entity system's *own* struct
layouts are not in the schema — nothing reflects the reflection — so
`SchemaLayout::DEADLOCK` (`crates/deadlock-reader/src/schema.rs`) and
`EntityLayout::DEADLOCK` (`crates/deadlock-reader/src/entity.rs`) were derived by
structural search against a running game: for each candidate offset, follow the pointer
and score whether it lands on something whose name field reads back plausibly, or whose
instance pointer carries a vtable inside `client.dll`. `cargo run --example probe`
re-derives all of them and prints `ok` or `MISMATCH` against what the crate assumes.

**Baked fallbacks.** The `(class, field, fallback)` table in §5 is used only when a schema
lookup misses. It is baselined against **build 6723** (`VersionDate=Sep 30 2026`), re-probed
from a live client. Two `#[ignore]`d live tests in `crates/deadlock-reader/src/fields.rs`
keep it honest: every row must name a field the schema has, and every fallback must equal
the live offset.

**Not exercised.** No decoder for the replay CDN path lives in this crate; the URL
construction does (§8). The Linux backend compiles and is unit-tested against recorded
`/proc/<pid>/maps` fixtures, but no `process_vm_readv` call has ever been made against a
real client.

## 3. Read-only surface

Reading is strictly observation. There is no write path anywhere in the crate: no
`WriteProcessMemory`, no injection, no hooking, no remote thread, no call into the target.

Windows (`crates/deadlock-memory/src/process.rs`): `OpenProcess`, `ReadProcessMemory`,
`VirtualQueryEx`, the Toolhelp32 snapshot family, `RegGetValueW`, `CloseHandle`.

Two access masks are used, and the narrower one is not an accident:

| Mask | Constant | Used for |
|---|---|---|
| `PROCESS_QUERY_INFORMATION \| PROCESS_VM_READ` (`0x410`) | `process::ACCESS_READ` | reading |
| `PROCESS_QUERY_INFORMATION` (`0x400`) | `process::ACCESS_QUERY` | region enumeration only |

Linux (`crates/deadlock-memory/src/linux.rs`): `process_vm_readv(2)` for reads — one
syscall, no `PTRACE_ATTACH`, the target is never stopped — and `/proc/<pid>/maps` for both
modules and regions, which replaces `VirtualQueryEx` *and* the Toolhelp32 snapshot.

## 4. From process to field offsets

### 4.1 Attach

1. Enumerate modules of `deadlock.exe` (`CreateToolhelp32Snapshot` → `Module32FirstW` /
   `Module32NextW`; `/proc/<pid>/maps` on Linux).
2. Locate `client.dll`; record its base address and image size.
3. Detect the target ABI from the mapped image's `MZ` / `ELF` magic. Signatures, fallback
   offsets and struct layouts depend on how the *target* was compiled, not on the host, so
   a Linux host reading a Proton-hosted game needs the Windows tables — Wine maps the real
   PE and the game code is still MSVC-compiled x86-64
   (`crates/deadlock-reader/src/abi.rs`).

Under Proton, three details matter: pid discovery must go through `/proc/<pid>/cmdline`
rather than `comm` (truncated to 15 bytes) and prefer the process with a client module
mapped, because a prefix runs several Wine processes; module lookup must be by basename,
because pressure-vessel's mount namespace makes the paths in `maps` container-relative;
and `[vvar]` / `[vsyscall]` must be excluded from the scan set.

The game build id is not a memory read at all: it comes from `steam.inf` in the installed
`citadel` directory.

### 4.2 Copying the module image

The whole `client.dll` image (about 60 MiB) is copied into a local buffer before any
scanning, in **1 MiB** chunks, retrying a failed chunk in **4 KiB** pages so that a handful
of unreadable pages does not abort the scan
(`mem::IMAGE_CHUNK` / `mem::IMAGE_RETRY_CHUNK`, `crates/deadlock-memory/src/mem.rs`).

This copy dominates attach cost — roughly 270 ms all told, with the schema walk. After that
a tick is ~135 µs.

### 4.3 Signature scan: the three globals

Three globals in `client.dll` are the whole entry surface. Each is located by an AOB
pattern over the copied image, anchored on an instruction that *references* the global,
and then resolved through the instruction's RIP-relative displacement (§4.4).

A descriptor is `{ name, pattern, disp_off, instr_len }`, where `disp_off` is the byte
offset of the `disp32` inside the match and `instr_len` is the total length of the
RIP-relative instruction (`sig::SigDesc`).

| Global | Pattern | `disp_off` | `instr_len` |
|---|---|---|---|
| `entity_identity_list` | `48 63 43 08 48 69 C8 30 01 00 00 48 8B 05 ?? ?? ?? ?? 48 8B 8C` | 14 | 18 |
| `entity_system` | `48 8B 0D ?? ?? ?? ?? 45 33 C9 C6 44 24 30 01 4C` | 3 | 7 |
| `schema_system` | `48 8B CB 48 89 1D ?? ?? ?? ??` | 6 | 10 |

Decoded, the anchors are:

```asm
; entity_identity_list
movsxd rax, dword [rbx+8]
imul   rcx, rax, 0x130        ; NOT sizeof(CEntityIdentity) - see §6.1
mov    rax, [rip+GLOBAL]      ; <- disp32 at +14, instruction ends at +18
mov    rcx, [rax+rcx+...]

; entity_system
mov    rcx, [rip+GLOBAL]      ; <- disp32 at +3, ends at +7
xor    r9d, r9d
mov    byte [rsp+0x30], 1

; schema_system
mov    rcx, rbx
mov    [rip+GLOBAL], rbx      ; <- disp32 at +6, ends at +10
```

The `schema_system` anchor targets the *store* where `client.dll` caches the
`CSchemaSystem*` it received from the interface factory, so `schemasystem.dll` never has to
be opened or scanned.

The descriptors live in `crates/deadlock-reader/src/globals.rs`, grouped in a
`SignatureSet` per ABI rather than a flat slice, so each pattern stays bound to the global
it resolves; matching by name at the call site would be a silent-failure hazard.

`Abi::SysV` currently supplies no signatures, so a native Linux client is refused with
`Error::UnsupportedAbi` rather than read with MSVC offsets, which would produce confident
nonsense.

### 4.4 RIP-relative resolution

```text
disp32    = i32le(image[match_off + disp_off ..][..4])
global_va = module_base + match_off + instr_len + disp32
```

The displacement is signed: a negative one must sign-extend, not wrap through `u32`. A
resolved address outside `[module_base, module_base + module_size)` is rejected as a bad
match rather than trusted (`sig::scan_and_resolve`,
`crates/deadlock-memory/src/sig.rs`).

These are addresses *of the variables*, not of what they point to. Each needs a further
read to dereference.

### 4.5 The schema walk

Dereference `schema_system`, iterate its type scopes, and build a
`class → field → offset` index. Confirmed against a running client (20 type scopes, 2914
classes in the `client.dll` scope alone, 3605 classes overall, 200 enums):

```text
CSchemaSystem     +0x190  CUtlVector<CSchemaSystemTypeScope*>  (count @ +0x190, data @ +0x198)
TypeScope         +0x008  char m_szScopeName[]                 "client.dll", "engine2.dll", ...
                  +0x5c0  HashBucket_t m_Buckets[256]          stride 0x18
HashBucket_t      +0x008  first entry      +0x010  first uncommitted entry
Entry             +0x008  next             +0x010  CSchemaClassBinding*
ClassBinding      +0x008  name  +0x018 size  +0x01c field count
                  +0x028  fields        +0x030 -> +0x008  base class binding
Field             stride 0x20, +0x000 name, +0x008 type, +0x010 offset
CSchemaType       +0x008  type name     +0x020  enum binding (when enum-typed)
EnumBinding       +0x008  name  +0x018 size  +0x01c count  +0x020 enumerators
Enumerator        stride 0x20, +0x000 name, +0x008 value
```

Three properties are easy to get wrong:

- **The declared-class table is a 256-bucket hash**, not a single linked list.
- **Merge order is load-bearing.** 2637 class names exist in *both* the `client.dll` and
  `server.dll` scopes, with different offsets. A client process must be read with
  `client.dll`'s numbers, so the walk sorts that scope to the front and keeps the first
  binding for a name (`schema::PRIORITY_SCOPE`, `crates/deadlock-reader/src/schema.rs`).
  Left unordered, `server.dll` silently wins.
- **A class binding lists only the fields declared directly on it.**
  `CCitadelPlayerController` owns `m_PlayerDataGlobal`; `m_iTeamNum` and `m_iHealth` live up
  on `C_BaseEntity`. Inherited fields must be folded in by following
  `binding +0x30 → +0x08 → base binding`.

Enum *values* are recovered by following a field's type pointer
(`field +0x08 → CSchemaType +0x20 → CSchemaEnumBinding`) rather than by locating the
scope's declared-enum hash. It is more direct, and it only pays for enums a visited field
actually references. Enumerators are an array at `+0x20` of `{ const char* name; u64 value }`
records with **stride `0x20`**; a stride of `0x10` reads only the even-indexed half and
yields a plausible-looking but truncated sequence.

The walk is bounded at every step (`schema::limits`): scope counts, field counts, class
sizes, field offsets and chain lengths all have sanity caps, and a walk yielding fewer than
200 classes is treated as a failed layout guess rather than a result. `system_type_scopes`
and `scope_class_buckets` each have a candidate sweep
(`SchemaLayout::TYPE_SCOPE_CANDIDATES`, `BUCKET_CANDIDATES`), so a patch that moves either
can be recovered from automatically instead of needing a code change.

A failed walk is not fatal. Offsets fall through to the baked table in §5, at reduced
coverage.

### 4.6 Reading a field by name

Every read is `offset_of(class, field)` against the schema index, falling back to the baked
offset for that pair when the lookup misses, then a typed read at `entity + offset`.

**Networked value types carry a `0x30` prologue.** At runtime, every Citadel networked
`S*` / `*_t` value type has 0x30 bytes ahead of its first member. The schema dump declares
the *logical* struct — `STrooperFOWEntity` as
`{ uint8 m_nPosX; uint8 m_nPosY; uint8 m_nFlags; }` — and reading that
literally puts the members at `0x0`/`0x1`/`0x2`, which is wrong. The real offsets are the
ones the runtime schema reports, and the element *size* the schema reports is the walk
stride.

Two independent measurements land on the same shape:

| Type | Stride | Members |
|---|---|---|
| `StatViewerModifierValues_t` | `0x40` | `m_SourceModifierID` `0x30`, `m_eValType` `0x34`, `m_flValue` `0x38` |
| `AbilityUpgradeState_t` | `0x38` | `m_ItemID` `0x30`, `m_nUpgradeInfo` `0x34` |

The second (`tunables::DEFAULT_ABILITY_UPGRADE_LAYOUT`) was probed out of the live process
independently of the schema and agrees with what the schema declares for its own element
type. Anything walking such a struct as a packed array must use the schema's offsets and
the element size; walking by anything smaller reads the next element's prologue.

## 5. Class and field reference

73 `(class, field)` pairs, the set `deadlock-reader` looks up by name
(`crates/deadlock-reader/src/fields.rs`). `fallback` is used only when the runtime
schema lookup misses. `-` means there is none and the read is skipped instead. Fallbacks
are build-specific and drift with every Deadlock build — always prefer a resolved schema
offset.

Rows marked † have a class name that was inferred from context rather than read from a
literal.

### `C_CitadelGameRulesProxy` — entry point to game rules

| Field | Fallback |
|---|---|
| `m_pGameRules` | `0x5f0` |

### `C_CitadelGameRules` — match-level state

| Field | Fallback |
|---|---|
| `m_unMatchID` | - |
| `m_eGameState` | - |
| `m_flGameStartTime` | - |
| `m_bServerPaused` | - |
| `m_iMidbossKillCount` | - |
| `m_iAmberRejuvCount` | - |
| `m_iSapphireRejuvCount` | - |

A snapshot reads considerably more off this class than the fallback table covers, all by
name: `m_eMatchMode`, `m_eGameMode`, `m_bGamePaused`, `m_nPauseStartTick`,
`m_nTotalPausedTicks`, `m_iPauseTeam`, `m_pausingPlayerId`, `m_nHideoutOwner`,
`m_iWinningTeam`, `m_tNextMidBossSpawnTime`, `m_flGameStateStartTime`,
`m_flGameStateEndTime`, `m_flMatchClockAtLastUpdate`, `m_nMatchClockUpdateTick`, the KOTH
timers and the two rejuvenator gold counters
(`snapshot::RULES_FIELDS`, `crates/deadlock-reader/src/snapshot/mod.rs`). They are
fetched in one bulk read that the snapshot, the clock and the objective timers share.

`m_bServerPaused` alone is not sufficient: it was observed *not* flipping during a real
pause seen from a spectating client. `is_paused()` ORs `m_bGamePaused`, `m_bServerPaused`
and `m_nPauseStartTick != 0`, the last being the most direct evidence.

`m_iMidbossKillCount` is a kill count, not an aliveness flag — it went `0 → 1` when the
Midboss died.

`m_flMatchClockAtLastUpdate` looks authoritative but was observed **frozen** across a live
match. Elapsed time is derived instead from a live entity's `m_flSimulationTime` minus
`m_flGameStartTime`, with `m_nTotalPausedTicks` removed for playing time.

### `CCitadelPlayerController`

| Field | Fallback |
|---|---|
| `m_PlayerDataGlobal` | `0x908` |
| `m_steamID` | `0x788` |
| `m_iTeamNum` | - |
| `m_bIsLocalPlayerController` | - |
| `m_unLobbyPlayerSlot` | - |

`m_iszPlayerName` sits at `0x6fc` with `m_steamID` at `0x788` on the baselined build, which
bounds how much of the inline string is worth reading.

### `PlayerDataGlobal_t` — the scoreboard

| Field | Fallback |
|---|---|
| `m_nHeroID` | - |
| `m_iLevel` | - |
| `m_iGoldNetWorth` | - |
| `m_iPlayerKills` | - |
| `m_iDeaths` | - |
| `m_iPlayerAssists` | - |
| `m_iLastHits` | - |
| `m_iDenies` | - |
| `m_iHeroDamage` | - |
| `m_iObjectiveDamage` | - |
| `m_iHeroHealing` | - |
| `m_vecUpgrades` | - |
| `m_tHeldItem` | - |

Three quirks confirmed live:

- `m_iLevel` reads one higher than the game displays (UI 23 / field 24, UI 27 / field 28).
- Bools in this struct sit at unaligned byte offsets (`0x80`..`0x83`) and must be read as
  `u8`; a `u32` read there returns three neighbouring fields.
- The creep-souls breakdown fields (`m_iCreepGold*`, `m_iFarmBaseline`) read **zero on a
  spectating client** and are probably not networked.

`m_vecAbilityUpgradeState` holds `AbilityUpgradeState_t` records (see §4.6).
`m_nUpgradeInfo` packs a tier bitmask into its high 16 bits: bit 0 means the ability is
learned, and each further bit is one point spent. Verified against a live match where a
Yamato had Power Slash at 1 point (`0x3_0001`), Flying Slash at 2 (`0x7_0001`), Crimson
Slash at 0 (`0x1_0001`) and an unlearned ultimate (`0x1`). The low 16 bits have read
`0x0001` on every sample so far and their meaning is unknown.

### `C_CitadelPlayerPawn`

| Field | Fallback |
|---|---|
| `m_nCurrencies` | - |
| `m_nSpentCurrencies` | - |
| `m_hController` | - |
| `m_iTeamNum` | - |
| `m_iHealth` | - |
| `m_iMaxHealth` | - |
| `m_nLevel` | - |
| `m_flSimulationTime` | - |
| `m_CCitadelAbilityComponent` | `0x1438` |
| `m_pGameSceneNode` | `0x330` |
| `m_sPlayerDamageTaken` | `0x13e0` |

Who is on screen is a chain, not a field:
`pawn → m_pObserverServices → m_hObserverTarget → pawn → m_hController`. Both an observer
pawn (spectating) and a normal player pawn (dead, watching a teammate) carry observer
services, so both are checked.

### `C_BaseEntity`

| Field | Fallback |
|---|---|
| `m_iTeamNum` | - |
| `m_iHealth` | - |
| `m_iMaxHealth` | - |
| `m_pModifierProp` | `0x348` |

### Modifiers (buffs / debuffs)

Chain: `C_BaseEntity::m_pModifierProp → CModifierProperty::m_vecModifiers → CBaseModifier[]`

| Class | Field | Fallback |
|---|---|---|
| `CModifierProperty` | `m_vecModifiers` | `0x40` |
| `CBaseModifier` | `m_nSerialNumber` | `0x28` |
| `CBaseModifier` | `m_flCreationTime` | `0x30` |
| `CBaseModifier` | `m_flDuration` | `0x34` |
| `CBaseModifier` | `m_hAbility` | `0x3c` |
| `CBaseModifier` | `m_hCaster` | - |
| `CBaseModifier` | `m_nAbilitySubclassID` | `0x5c` |
| `CBaseModifier` | `m_iStackCount` | `0x62` |
| `CBaseModifier` | `m_iMaxStackCount` | `0x64` |
| `CBaseModifier` | `m_bDisabled` | `0x71` |

### Abilities

| Class | Field | Fallback |
|---|---|---|
| `CCitadelAbilityComponent` | `m_vecAbilities` | - |
| `C_CitadelBaseAbility` | `m_flCooldownStart` | `0x768` |
| `C_CitadelBaseAbility` | `m_flCooldownEnd` | `0x76c` |
| `C_CitadelBaseAbility` | `m_flCastCompletedTime` | `0x770` |
| `C_CitadelBaseAbility` | `m_eAbilitySlot` | `0x77c` |

### Stat viewer / hero power

| Class | Field | Fallback |
|---|---|---|
| `CCitadel_Modifier_HeroUpgradeBonuses` | `m_flWeaponPower` | `0x138` |
| `CCitadel_Modifier_HeroUpgradeBonuses` | `m_flArmorPower` | `0x13c` |
| `CCitadel_Modifier_HeroUpgradeBonuses` | `m_flTechPower` | `0x140` |

`StatViewerModifierValues_t` (`m_eValType`, `m_flValue`, `m_SourceModifierID`) is the
element type of the stat-viewer vectors; its layout is in §4.6.

### Damage attribution

| Class | Field | Fallback |
|---|---|---|
| `CCitadelRecentDamage` | `m_flLastDamageTime` | `0x8` |
| `CCitadelRecentDamage` | `m_flStartTime` | `0xc` |
| `CCitadelRecentDamage` | `m_hPlayerEntToStore` | `0x14` |

### Tracked stats

The client keeps per-player and per-team tracked-stat entities whose element type is
`TrackedStatNetworkData_t`.

| Class | Field | Fallback | |
|---|---|---|---|
| `CPlayerTrackedStatsEntity` | `m_vecTrackedStats` | `0x5f0` | † |
| `CPlayerTrackedStatsEntity` | `m_nPlayerSlot` | `0x660` | † |
| `CPlayerTrackedStatsEntity` | `m_nTeam` | `0x664` | † |
| `CTeamTrackedStatsEntity` | `m_vecTrackedStats` | `0x5f0` | † |
| `CTeamTrackedStatsEntity` | `m_nTeam` | `0x660` | † |

### Teams, position, minimap, NPCs

| Class | Field | Fallback |
|---|---|---|
| `C_Team` | `m_aPlayerControllers` | - |
| `C_CitadelTeam` | `m_nStreetBrawlScore` | - |
| `CGameSceneNode` | `m_vecAbsOrigin` | `0xc8` |
| `C_NPC_Boss_Tier2` | `m_iLane` | - |
| `CCitadelTrooperMinimap` | `m_vecFOWEntities` | - |
| `STrooperFOWEntity` | `m_nFlags` | `0x32` |
| `STrooperFOWEntity` | `m_nPosX` | `0x30` |
| `STrooperFOWEntity` | `m_nPosY` | `0x31` |

Team numbers are `0` Unassigned, `1` Spectator, `2` Amber, `3` Sapphire, `4` Neutral. Names
come from the game's own `C_Team::m_szTeamname` rather than being hardcoded.

A spectating client's own controller reports `m_unLobbyPlayerSlot == 1` — the same slot as
the first Amber player — and sits on team 1, so a spectator's slot is not a real slot.

`C_CitadelTeam::m_iScore` was observed to equal the side's summed kills exactly. The game
exposes no other team-level totals; anything else per side has to be summed from the
players.

### Not reachable this way

**Chat.** The client schema declares no chat classes at all (the closest is
`CCitadel_Modifier_UIHudMessage`). Chat lives in the Panorama UI layer, outside both the
entity and the schema systems.

**Rejuvenator remaining time.** `scripts/generic_data.vdata_c` ships the duration
(`m_RejuvParams.m_flRejuvinatorBuffDuration = 180`,
`m_flRejuvinatorExpirationWarningTiming = 30`), but the client networks no pickup
timestamp: sweeping the runtime schema for `rejuv` finds only the two rules counters and
`PlayerDataGlobal_t::m_bHasRejuvenator`, a bare flag. A countdown needs a layer that
watches the flag turn on and stamps the clock itself.

## 6. Entity system

### 6.1 Layout, and the `0x130` that is not the stride

The `imul rcx, rax, 0x130` inside the `entity_identity_list` signature is **not**
`sizeof(CEntityIdentity)`. Candidate strides were scored against a live client on whether
the resulting instance pointers carry vtables inside `client.dll`
(`crates/deadlock-reader/src/entity.rs`):

```text
stride 0x060:  39/512 slots valid
stride 0x068:  19/512
stride 0x070: 272/512   <- actual
stride 0x078:  18/512
stride 0x130:  44/512   <- indistinguishable from noise
```

The chunk table also hangs off **`*entity_system + 0x10`**, not off the
`entity_identity_list` global, which in the current build points at unrelated resource data
(`scripts/misc.vdata`). The signature remains a valid way to locate the entity system; the
structure its embedded constant describes is something else.

Confirmed layout (`EntityLayout::DEADLOCK`):

```text
*entity_system   +0x10   CEntityIdentity* chunks[64]   stride 8, 512 identities each
CEntityIdentity  +0x00   CEntityInstance* m_pInstance
                 +0x08   CEntityClass*    m_pClass
                 +0x10   CEntityHandle    m_EHandle
                 +0x20   const char*      m_designerName   "citadel_player_pawn"
CEntityClass     +0x08 -> +0x00 -> const char*              "C_CitadelPlayerPawn"
```

Chunk selection is `idx >> 9`, slot within a chunk `idx & 0x1FF`; the walk runs to entity
index `0x8000`.

`m_designerName` at `+0x20` is a probed offset with a nasty failure mode: moving it to
`+0x28` passes the whole test suite, live tests included, because the name accessor prefers
the designer name and falls back to the class name — so a wrong offset produces neither an
error nor an empty snapshot, just quietly worse names.

### 6.2 Handles

A `CHandle`'s low 15 bits are the entity index: `HANDLE_INDEX_MASK = 0x7FFF`.
`u32::MAX` is the invalid handle.

### 6.3 Objective and NPC classes

| Class | Label | Structure? |
|---|---|---|
| `C_NPC_TrooperBoss` | `guardian` | yes |
| `C_NPC_Boss_Tier2` | `walker` | yes |
| `C_NPC_BarrackBoss` | `base_guardian` | yes |
| `C_NPC_Boss_Tier3` | `patron` | yes |
| `C_Citadel_Destroyable_Building` | `shrine` | yes |
| `C_NPC_MidBoss` | `midboss` | yes |
| `C_NPC_Trooper` | `lane_trooper` | no |
| `C_NPC_TrooperNeutral` | `neutral_creep` | no |
| `C_NPC_Neutral_SinnersSacrifice` | `sinners_sacrifice` | no |

The structure/creep split matters: a live match holds roughly 340 creeps against 20
structures, so a list that lumps them together is not an objective list. `objectives` is
structures only (`entity::STRUCTURE_CLASSES`,
`crates/deadlock-reader/src/entity.rs`); creeps stay reachable through `entities()`.

These names are compared against an entity's own class name and never used to look up a
field, so a misspelling resolves to nothing and the objective silently never appears — which
is why they are a walkable list with a test over it rather than bare `match` arms.

Structure liveness is settled from the destruction signals the game networks, not from
health, which can read a stale positive value for a tick after a structure falls.

### 6.4 Game state, match mode and game mode

Values come from the game's own enum bindings (§4.5), not from the order names appear in
the binary. Inferring `EGameState` from string order is wrong twice over: it misses
`Invalid = 0`, and it swaps `WaitForMapToLoad` with `WaitingForPlayersToJoin`. The symptom
is a live match reporting `PostGame`.

`EGameState` (`C_CitadelGameRules::m_eGameState`):

| Value | Name | | Value | Name |
|---|---|---|---|---|
| 0 | `Invalid` | | 6 | `PreGameWait` |
| 1 | `Init` | | 7 | `GameInProgress` |
| 2 | `WaitingForPlayersToJoin` | | 8 | `PostGame` |
| 3 | `HeroSelection` | | 9 | `PostGame_PlayOfTheGame` |
| 4 | `MatchIntro` | | 10 | `Abandoned` |
| 5 | `WaitForMapToLoad` | | 11 | `End` |

Only `PostGame` is dependable as an ending: over recorded match endings every one passed
through `PostGame`, about 40% reached `End`, and `PostGame_PlayOfTheGame` never appeared at
all.

`ECitadelMatchMode` (`m_eMatchMode`) — what the matchmaker put you in:

| 0 `Invalid` | 1 `Unranked` | 2 `PrivateLobby` | 3 `CoopBot` | 4 `Ranked` |
|---|---|---|---|---|
| 5 `ServerTest` | 6 `Tutorial` | 7 `HeroLabs` | 8 `NewPlayerPlacement` | |

`ECitadelGameMode` (`m_eGameMode`) — what is actually being played:

| 0 `Invalid` | 1 `Normal` | 2 `1v1Test` | 3 `Sandbox` |
|---|---|---|---|
| 4 `StreetBrawl` | 5 `ExploreNYC` | 6 `Internal` | |

All three are also read from the runtime enum binding at attach, so the tables above are the
fallback. `dlrs enums` dumps all ~200 enums the client declares.

### 6.5 Detecting the Hideout

The Hideout is **not** a distinct game state. It reports:

```text
m_eGameState = 7 (GameInProgress)   m_eMatchMode = 0 (Invalid)
m_unMatchID  = 0                    m_eGameMode  = 0 (Invalid)
```

which is indistinguishable from a half-initialised match on those fields alone. The reliable
signal is entity presence — these classes exist only on the Hideout map
(`tunables::DEFAULT_HIDEOUT_CLASSES`, `crates/deadlock-reader/src/tunables.rs`):

```text
C_CitadelTriggerHideout          C_Citadel_Hideout_Ball
C_Citadel_Hideout_Clock          CCitadelHideoutTeleportTrigger
CCitadelHideoutInteractableProp  C_NPC_Neutral_Hideout_Cat
```

`C_CitadelGameRules::m_nHideoutOwner` exists but read back as `0` in a solo Hideout, so it
is not sufficient on its own. A Hideout client reports no match id rather than match id
zero, so downstream consumers are not fed a match that never existed.

Street Brawl has its own markers: `C_CitadelGameRules::m_tStreetBrawl` and
`C_CitadelTeam::m_nStreetBrawlScore`.

## 7. Game Coordinator objects in the heap

Party, lobby, friends and account data are matchmaking state, not world state, so nothing
in §4 to §6 reaches them (§1). The client keeps the Steam Game Coordinator's shared objects
in the heap as live C++ protobuf messages. It does not keep them as serialised wire bytes.

### 7.1 Region filter

No pointer leads to these objects and no table indexes them, so the search surface is every
committed read-write region of the target (`crates/deadlock-memory/src/region.rs`):

```text
addr = 0x10000                                   region::SCAN_START
while (addr >> 16) < 0x7FFFFFFF:                 region::SCAN_END_SHIFTED
    accept if:
        State   == MEM_COMMIT (0x1000)
        Type    == MEM_PRIVATE (0x20000) or MEM_MAPPED (0x40000)
        Protect & (PAGE_READWRITE|PAGE_WRITECOPY) != 0     i.e. & 0x0C
        Protect & (PAGE_NOACCESS|PAGE_GUARD)      == 0     i.e. & 0x101 clear
        RegionSize <= 0x8000000                            region::MAX_REGION, 128 MiB
```

Rejecting guard pages is not only about readability: touching one would perturb the target.

`MEMORY_BASIC_INFORMATION` field offsets on x64: `BaseAddress +0x00`, `RegionSize +0x18`,
`State +0x20`, `Protect +0x24`, `Type +0x28`.

Two practical details on top of the filter. Regions are read in `SCAN_CHUNK` (16 MiB) pieces
rather than sized to the largest region, so a threaded scan does not reserve one 128 MiB
buffer per thread. And `ReadProcessMemory` refuses an entire range if any page in it is
inaccessible, so a region with one unmapped page in the middle reads as zero bytes and its
other pages would never be searched — the recovery path probes a page (`PROBE_PAGE`, 4096)
at a time to find the contiguous readable spans.

### 7.2 Finding and validating objects

`deadlock-walker` finds each object in four steps.

1. **Vtable.** It reads the type descriptor from the client's RTTI, follows it to the
   complete-object locator, and from there to the message class's vtable.
2. **Heap search.** Every 8-aligned qword in the readable regions that equals the vtable is
   an instance. The loaded modules are excluded, because statically allocated default
   instances carry the same vtable as real objects.
3. **Layout.** Field offsets and has-bits come from the protobuf-cpp 3.21 `DescriptorTable`
   compiled into `client.dll`: its `MigrationSchema`, its `offsets[]`, and its default
   instances. Field numbers and types come from the embedded descriptors. The tables list
   fields in declaration order, not number order. A scalar `RepeatedField` pointer points at
   element 0.
4. **Walk.** The walker writes the object out as wire bytes, and `prost` decodes them into
   the `valveprotos` message.

A heap search reads every writable region and takes about a second on a live client. Polling
cannot afford that, so `GcSession` tries the cheapest step first:

| Step | Cost | When |
|---|---|---|
| Re-read pinned objects | microseconds | every call |
| Probe remembered addresses | microseconds | after a pin died |
| Search the regions those addresses sit in | tens of ms | after a pin died, rate limited |
| Search the whole heap | about a second | no object known, rate limited |

A pin is only an address. The client frees and reuses memory, so each re-read checks the
vtable before and after the walk. It drops the pin when the object is gone or no longer
walks. An object can also be freed in the middle of a walk, so a read that comes back
empty is retried and is not treated as an error.

A vtable hit is a live object of that class, but not necessarily the wanted one. The client
holds copies, caches and other accounts' objects. Each kind therefore also has to name the
local account: a party lists it as a member, and a hero build is authored by it. Every copy
that passes is returned.

| Kind | Message |
|---|---|
| Party | `CSOCitadelParty` |
| Lobby | `CSOCitadelLobby` |
| Hideout | `CSOCitadelHideoutLobby` |
| GameAccount | `CSOGameAccountClient` |
| AccountStats | `CMsgAccountStats` |
| AccountHeroes | `CSOAccountHeroInfo` |
| HeroBuilds | `CMsgHeroBuild` |
| PostGameProgress | `CMsgPostGameProgressData` |
| MatchMetaData | `CMsgMatchMetaDataContents` |

Match metadata is the exception to the account rule. The client holds one object per match
the player has opened, and none of them names an account. A copy counts when it is
complete, meaning a non-zero match id and at least one player. `GcSession::match_metadata`
picks among the pinned copies by match id on each call. A match the player has not opened
is not resident, and the call returns `None`.

The Game Coordinator never sends this message. `CMsgClientToGCGetMatchMetaDataResponse`
carries a `metadata_salt`, a `replay_salt` and validity stamps, but no metadata. The client
builds the object itself at match end. It is resident only while the client holds it.

A pinned read cannot be checked against a serial the way an entity handle can. The vtable
check and the completeness check catch a freed or reused object, but a shared object may be
reallocated when it changes. The old pin then dies on the very change you wanted to see,
and the next search finds the new object.

### 7.3 Local Steam account id

The logged-in account id is not read from game memory at all. Steam keeps the same tree on
both platforms:

```text
Windows  HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser   (REG_DWORD)
Linux    ~/.steam/registry.vdf -> HKCU/Software/Valve/Steam/ActiveProcess/ActiveUser
```

It is a 32-bit account id; `steam::to_steam64` / `to_account_id` convert against the
individual-universe base.

## 8. Replay metadata

Deadlock's Steam AppID is **1422450**. When a match ends, Valve publishes its
`CMsgMatchMetaData`, bzip2'd, at:

```text
http://replay{cluster_id}.valve.net/1422450/{match_id}_{metadata_salt}.meta.bz2
```

`deadlock_reader::replay_meta_url` builds it (`crates/deadlock-reader/src/lib.rs`).
`match_id` comes from `C_CitadelGameRulesProxy → m_pGameRules → m_unMatchID`; the
`cluster_id`, `metadata_salt` and `replay_salt` come from the GC side (§7.2), where the
client receives them in `CMsgClientToGCGetMatchMetaDataResponse`.

This file is the only place several numbers exist at all: weapon accuracy (`shots_hit`,
`shots_missed`), `headshot_kills`, `damage_mitigated`, `heal_prevented`, per-death
positions, the damage matrix and objective first-damage times have no live-memory
equivalent.

`.dem` files in `citadel/replays/` are a different thing entirely: Source 2 demo streams,
not metadata, and large (37 local ones ran 269 MB to 773 MB), so they are streamed a packet
at a time rather than loaded.

## 9. After a Deadlock update

1. `cargo run --release --example probe` — rediscovers every layout constant by structural
   search and prints `ok` or `MISMATCH`.
2. `dlrs status` — do the three globals resolve, and does the schema walk succeed?
3. `dlrs entities` — are class names sensible?
4. `dlrs players` — does the scoreboard match what is on screen?

If a signature stops matching, that pattern has drifted and needs re-deriving against
`client.dll`. If only the schema walk fails, the reader keeps working on the baked fallback
table at reduced coverage.

The walker reads its layouts from the client's own protobuf tables, so a patch that changes
a Game Coordinator message does not need a code change. A patch that changes the table
format or the RTTI layout does. `dlrs party` and `dlrs account` are the quick check that the
walker still reads a live client.

Constants the game never exposes to the client — the bridge-buff cadence, the tick rate, the
entity class names matched by string — cannot be re-derived at runtime. They live in
`Tunables` with defaults an application can override without waiting for a release.

A field or class *rename* is invisible to the schema walk, which only ever asks for names it
already knows; `drift.rs` is what notices one.

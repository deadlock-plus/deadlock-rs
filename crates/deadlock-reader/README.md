# deadlock-reader

A read-only external reader for Deadlock's live match state, in Rust. Process access comes
from [`deadlock-memory`](https://github.com/deadlock-plus/deadlock-rs/tree/main/crates/deadlock-memory);
this crate adds everything that knows about Deadlock.

Field offsets come from the game's own Source 2 runtime schema, walked at attach. The
memory and schema layout this relies on is written up in
[`LAYOUT.md`](https://github.com/deadlock-plus/deadlock-rs/blob/main/crates/deadlock-reader/LAYOUT.md);
the class/field table is also shipped as
[`schema-fields.json`](https://github.com/deadlock-plus/deadlock-rs/blob/main/crates/deadlock-reader/schema-fields.json).

```
cargo run --release -p dlrs -- status
```

```text
pid                14188
client.dll         0x00007ffaf4ce0000 (63696896 bytes, 63696896 readable)
entity_system      0x00007ffaf85f5498
entity_identity    0x00007ffaf80905d0
schema_system      0x00007ffaf8664f20
schema             resolved: 3605 classes, scopes: client.dll, schemasystem.dll, ...
schema layout      type_scopes @ 0x190
schema enums       200 enums over 1886 enum-typed fields
entities           459
     78  C_ParticleSystem
     70  C_OmniLight
     ...
context            Hideout
state              GameInProgress (raw Some(7))  schema: EGameState_GameInProgress
match              id=Some(0) match_mode=Invalid game_mode=Invalid players=3
```

---

## What it does

1. Attaches to `deadlock.exe` with `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`.
2. Copies `client.dll`'s image (1 MiB chunks, 4 KiB retry) and AOB-scans it for three
   globals: `schema_system`, `entity_system`, `entity_identity_list`.
3. Walks the Source 2 schema system to recover **field offsets at runtime**, so the
   reader survives game patches instead of breaking on every update.
4. Walks the entity system into a per-tick snapshot.
5. Reads match, player and objective state **by name**, not by hardcoded offset, with
   the offsets recovered from the original binary as a fallback when a schema lookup
   misses.

It is strictly read-only. There is no `WriteProcessMemory`, no injection, no hooking, no
remote thread creation, and no calling into the target anywhere in this crate or in
`deadlock-memory` beneath it. The only
Win32 calls it makes are `OpenProcess`, `ReadProcessMemory`, `VirtualQueryEx`, the
Toolhelp32 snapshot family, `RegGetValueW` and `CloseHandle`.

## Layout

| Module | Role |
|---|---|
| `abi` | Which target is mapped (PE/MSVC vs ELF/SysV) and the tables that follow |
| `globals` | The three signatures and their descriptors |
| `fields` | Generated `(class, field, fallback)` table, 73 pairs |
| `schema` | Schema walk -> `class -> field -> offset` index, plus enums |
| `entity` | Entity list walk, handle masking, per-tick snapshot |
| `snapshot` | Match state, modes, scoreboard, objectives, Hideout detection |
| `timers` | Match clock and objective timers, each tagged with its provenance |
| `tunables` | Patch-sensitive constants (cadences, class names), overridable at runtime |
| `cache` | Optional pinned-entity cache that skips the walk on most ticks |
| `drift` | Notices a patch that renamed a field, class or enumerator, which the schema walk cannot |
| `supervise` | Keeps a `Reader` attached across the game starting, stopping and hiccuping |
| `events` | Snapshot-to-snapshot diffing into events (`events` feature) |
| `watcher` | Background polling thread and channel (`events` feature) |
| `steam` | Local Steam account id from the registry |
| `error` | The crate's error type; wraps `deadlock_reader::Error` |
| `reader` | The `Reader` facade tying it together |

The memory backends (`MemoryReader`, `mock`, `procmaps`, `linux`, `process`, `region`, `sig`)
live in `deadlock-memory`.

Hero names are **not** here; that is `deadlock-data`, so this crate stays offline and
dependency-light. Bridge them with `deadlock_core::HeroNames`.

## Usage

```rust
use deadlock_reader::Reader;

let reader = Reader::attach()?;

// Offsets resolve from the live schema, falling back to the baked table.
let off = reader.offset_of("PlayerDataGlobal_t", "m_iPlayerKills");
println!("from schema: {}", reader.offset_is_from_schema("PlayerDataGlobal_t", "m_iPlayerKills"));

if let Some(live) = reader.live_snapshot()? {
    for p in &live.players {
        println!("team {:?} hero {:?}  {:?}/{:?}/{:?}",
            p.team, p.hero_id, p.kills, p.deaths, p.assists);
    }
}
# Ok::<(), deadlock_reader::Error>(())
```

### CLI

```
dlrs status              attach and report what resolved
dlrs entities [substr]   list live entities, optionally filtered
dlrs players             scoreboard
dlrs player [slot|hero]  full detail for one or all players
dlrs objectives          walkers, guardians, patron, shrines
dlrs schema [substr]     list schema classes and their field offsets
dlrs enums [substr]      list schema enums and their values
dlrs heroes [substr]     hero catalogue (works offline)
dlrs items [substr|kind] item / ability catalogue (works offline)
dlrs probe-schema        brute-force the schema layout offset
dlrs fields              print the baked fallback offset table
dlrs regions             committed RW regions
dlrs steam               local Steam account id
dlrs watch               live scoreboard, refreshed 10x a second
dlrs events              print match events as they happen
dlrs bench               time the read path
```

### Features

* `serde`: derive `Serialize` on the snapshot, schema and layout types. Also enables
  `deadlock-core/serde` and `deadlock-memory/serde`.
* `events`: the `EventTracker` differ and the `Watcher` polling thread.
* `positions`: expose live world coordinates on `PlayerRow`. Off by default because
  everything else this crate reads is information the game already shows you, and live
  positions are not.
* `modifiers`: read every player's modifier list into their row. Off by default on
  measured cost, not on principle: against a live client at build 6683, three idle pawns in
  the Hideout, a snapshot costs ~150 us without it and ~300 us with it, so it **doubles**
  the tick. That is ~50 us per player, putting a twelve-player match near 600 us of
  modifier reads on top of the snapshot, and a hero mid-fight carries more than an idle one.
  Reproduce with `cargo run --release -p deadlock-reader --example bench`, with and without
  the feature. `snapshot::read_modifiers` is available without it for a caller that wants
  one entity rather than all.

`deadlock-memory`'s own `mock` feature gives tests a fake address space to build a `Reader`
over; this crate has no feature of that name.

## Using it as a library

`Reader` is `Send + Sync + 'static` and every read method takes `&self`, so it drops
straight into shared application state with no mutex of your own:

```rust,ignore
struct AppState { reader: Arc<Reader> }

// Tauri
app.manage(AppState { reader: reader.clone() });

#[tauri::command]
fn scoreboard(state: tauri::State<AppState>) -> Option<LiveSnapshot> {
    state.reader.live_snapshot().ok().flatten()
}
```

Enable `features = ["serde"]` and `LiveSnapshot` serialises straight to a frontend.
`examples/poll.rs` is a runnable version of this shape (background poller -> channel ->
consumer) with the Tauri mapping in its docs.

**Cost.** Attaching is the expensive part: about 270 ms, dominated by the 60 MiB image
copy and the schema walk. After that a tick is cheap:

| | |
|---|---|
| `Reader::attach()` | ~270 ms, once |
| `entities()` | ~140 µs |
| `live_snapshot()` | ~135 µs |

Comfortable at 10 Hz, well above the 1-2 Hz a scoreboard needs. Entity names are
memoised by pointer across ticks (`NameCache`), and both the chunk table and each
identity chunk are fetched in a single read, so a warm tick is a handful of syscalls
rather than tens of thousands.

**Errors.** `Error` wraps `deadlock_memory::Error` (attach and read failures) in its `Memory`
variant, with a `From` impl so `?` converts. It implements `std::error::Error + Send + Sync`, so `?` into
`Box<dyn Error>`, `anyhow` or `eyre` works. Library code returns errors rather than
panicking; a failed schema walk degrades to the baked offset table instead of failing
construction.

**Testing without a game.** `deadlock_memory::mock::MockMemory` (the `mock` feature of
`deadlock-memory`) implements `MemoryReader` over a sparse
fake address space, so downstream code can be tested against synthetic game state on any
platform; see `tests/usability.rs`, which drives signature resolution, the entity walk
and `Reader` construction with no game and no Windows.

---

## Confidence: read this before trusting output

This has been run against a live Deadlock client and reads real match data. Constants
come from two independent sources, and the code says which is which.

### Recovered by static extraction and verified byte-for-byte

Each is documented in context in `LAYOUT.md`.

* The three signature patterns, their displacement offsets and instruction lengths.
* The RIP-relative resolution arithmetic
  (`module_base + match_off + instr_len + i32(disp32)`).
* `OpenProcess` access masks (`0x410` for reads, `0x400` for queries).
* Image read chunking (1 MiB, 4 KiB retry).
* The `VirtualQueryEx` region filter, including the 128 MiB cap.
* `HANDLE_INDEX_MASK = 0x7FFF`.
* The whole `(class, field, fallback)` table.
* The Steam `ActiveUser` registry path and the replay CDN URL template.

### Measured against a live client

`SchemaLayout::DEADLOCK` and `EntityLayout::DEADLOCK` were derived by structural search
against a running game (`cargo run --release --example probe`), because the original's
schema walk operates on already-copied buffers and its internal offsets are not
recoverable statically. Both are plain structs, so a game update can be absorbed by
changing constants rather than logic.

Two of these cross-validate against the static extraction:
`CCitadelPlayerController::m_PlayerDataGlobal` at `0x8f0` and
`C_BaseEntity::m_pModifierProp` at `0x348` are what the live schema reports *and* what
the original binary carries as hardcoded fallbacks. Independent agreement between the two
methods.

### Corrections worth knowing about

Three earlier assumptions turned out to be wrong, each caught by checking against the
live client:

* **Entity stride.** The `imul rcx, rax, 0x130` inside the `entity_identity_list`
  signature is *not* `sizeof(CEntityIdentity)`. The real stride is `0x70` (290/512 valid
  slots, versus 44/512 at `0x130`, which is noise), and the chunk table hangs off
  `*entity_system + 0x10`, not off that global.
* **Game state ordinals.** Inferring them from the original binary's string-pool order
  was wrong twice: it missed `Invalid = 0`, and had `WaitForMapToLoad` /
  `WaitingForPlayersToJoin` swapped. The symptom was a live match reporting `PostGame`.
  Values now come from the game's own `EGameState` binding.
* **Scope merge order.** 2637 class names exist in both `client.dll` and `server.dll`
  with different offsets, and `server.dll` was silently winning.

### Field quirks found in a live spectated match

Four things that read plausibly but wrongly, all now handled:

* **`m_iLevel` reads one higher than the game displays.** Observed twice (UI 23 / field
  24, UI 27 / field 28). `PlayerRow::level` is the displayed value; `level_raw` is
  untouched. The behaviour at level 1 has not been observed, so the subtraction
  saturates.
* **Spectators are team 1 and their slot collides.** A spectating client's own
  controller reports `m_unLobbyPlayerSlot == 1`, the same as the first Amber player.
  Slots are suppressed for spectators and `scoreboard()` returns only the twelve real
  players. Teams: 0 Unassigned, 1 Spectator, 2 Amber, 3 Sapphire, 4 Neutral - names read
  from the game's own `C_Team::m_szTeamname`, not hardcoded.
* **`m_bServerPaused` did not flip during a real pause** seen from a spectating client.
  `PauseState` now reads all six pause fields and `is_paused()` ORs `m_bGamePaused`,
  `m_bServerPaused` and `m_nPauseStartTick != 0`, the last being the most direct
  evidence.
* **Objectives were 95% creeps.** The original's label table lumps lane troopers and
  neutral camps in with walkers and the Patron - about 340 creeps against 20 structures.
  `objectives` is now structures only; creeps remain available via `entities()`.

`m_iMidbossKillCount` is a kill count, not an aliveness flag - it correctly went `0 -> 1`
when the Midboss died. Use `midboss_alive()` for presence.

### What a snapshot exposes

`LiveSnapshot` carries match state (id, phase, mode, pause detail), a `MatchClock`,
per-team `TeamStats`, a `PlayerRow` per player, and map structures.

`TeamStats` sums souls, ability points, k/d/a, last hits, denies, hero and objective
damage and healing over each side, plus rejuvenators taken and structures still standing,
because the game exposes no team-level totals. `C_CitadelTeam::m_iScore` is included and
was observed to equal the side's summed kills exactly. `LiveSnapshot::team_stats`,
`soul_lead` and `spent_ability_points` sit on top.

`seconds_until_midboss()` counts down to `m_tNextMidBossSpawnTime`, which shares the
clock's time base. It returns `None` while the Midboss is on the map, since the scheduled
time is meaningless then, and clamps at zero rather than going negative on a stale timer.

`MatchClock` is derived: `m_flMatchClockAtLastUpdate` looks authoritative but was
observed **frozen** across a live match, so elapsed time comes from a live entity's
`m_flSimulationTime` minus `m_flGameStartTime`, with `m_nTotalPausedTicks` removed for
`playing_seconds()`. The raw fields are still exposed.

`PlayerRow` covers the scoreboard plus ability points, kill streak, self-healing,
liveness and respawn time, ultimate trained/cooldown, packed rank, the Rejuvenator
(golden statue) and Rebirth flags, abandon and cheater flags, purchased item ids
(`m_vecUpgrades`) and per-ability upgrade state (`m_vecAbilityUpgradeState`). Item and
ability ids resolve to names via `deadlock-data`.

`m_nUpgradeInfo` packs a tier bitmask into its high 16 bits: bit 0 means the ability is
learned, and each further bit is one point spent. Verified against a live match where a
Yamato had Power Slash at 1 point (`0x3_0001`), Flying Slash at 2 (`0x7_0001`), Crimson
Slash at 0 (`0x1_0001`) and an unlearned ultimate (`0x1`). `AbilityUpgrade` exposes
`unlocked`, `points` and the raw value; the low 16 bits have read `0x0001` on every
sample so far and their meaning is still unknown.

Two caveats found live: the creep-souls breakdown fields (`m_iCreepGold*`,
`m_iFarmBaseline`) read **zero on a spectating client** and are probably not networked;
and bools in `PlayerDataGlobal_t` sit at unaligned byte offsets (`0x80`..`0x83`), so they
must be read as `u8`; a `u32` read there returns three neighbouring fields.

**Chat history is not reachable this way.** The client schema has no chat classes at all
(only `CCitadel_Modifier_UIHudMessage`); chat lives in the Panorama UI layer, outside the
entity and schema systems.

### Who is on screen

`local_player()` is your own controller, but while spectating that sits on the spectator
team with no hero, which is rarely what a HUD wants. So:

| | |
|---|---|
| `local_player()` | your controller, whatever it is |
| `observed_player()` | whoever the camera is following, if anyone |
| `current_player()` | you while playing, the camera target while spectating |

`current_player()` is the one that corresponds to what is actually on screen, and
`perspective` reports which case applied (`Playing` / `Spectating` / `Unknown`).

The chain is `pawn -> m_pObserverServices -> m_hObserverTarget -> pawn -> m_hController`.
Both an observer pawn (spectating) and a normal player pawn (dead, watching a teammate)
carry observer services, so both are checked. While dead, `current_player()` deliberately
stays *you* (your stats are still the subject), with the teammate reachable via
`observed_player()`.

### Positions

World coordinates are behind the non-default `positions` feature. Everything else this
crate reads is information the game already shows you; live positions are not, so they
are opt-in rather than on by default.

### Modes, and telling the Hideout apart

`LiveSnapshot` exposes `match_mode` (Unranked / PrivateLobby / CoopBot / Ranked /
Tutorial / HeroLabs / ...), `game_mode` (Normal / Sandbox / StreetBrawl / ExploreNYC / ...)
and `context`, with helpers `is_hideout()`, `is_match()`, `is_ranked()`, `is_custom()`,
`is_street_brawl()` and `describe()`.

The Hideout needs care: it reports `GameInProgress` with match id `0` and both modes
`Invalid`, which looks identical to a half-initialised match. Detection keys off entity
classes that only exist on the Hideout map (`HIDEOUT_ENTITY_CLASSES`), not off state.

Enum *values* are read from the runtime schema rather than hardcoded (`dlrs enums`
dumps all ~200 of them), with the baked mapping as a fallback. `game_state_schema_name`
carries the game's own enumerator name, and is authoritative if it ever disagrees with
the baked enum.

### What has actually been exercised

Against a live client: all three signatures resolved; the schema walk recovered 3605
classes across 20 scopes plus 200 enums; the entity walk returned correct class names
(3779 entities in a match, 459 in the Hideout); `dlrs players` printed a real scoreboard
(hero, level, net worth, last hits, HP); `dlrs objectives` printed real objective health
(patron 6000/6000, walkers, base guardians, teams 2/3); and Hideout versus match context
was distinguished correctly in both.

Offline: 31 tests pass, including ground-truth cases in `deadlock-memory`'s `tests/rip_resolution.rs` that
reproduce real instruction encodings from the original binary and assert the resolver
lands on addresses confirmed by disassembly.

**Not exercised:** the replay CDN path. The crate builds the URL but has no downloader or
decoder; `deadlock-replay` reads the metadata file. Game Coordinator objects live in
`deadlock-walker`. A full 12-player live match has not been observed, and Ranked / Street Brawl / custom
lobby mode values come from the schema rather than from seeing them live.

**The Linux backend has never run.** It compiles for `x86_64-unknown-linux-musl` and its
parsing and ABI-selection logic are covered by tests using recorded `/proc/<pid>/maps`
fixtures, but no `process_vm_readv` call has ever been made. Treat first contact as
debugging, not as a regression.

### After a Deadlock update

1. `cargo run --release --example probe` rediscovers every layout constant by
   structural search and prints `ok` or `MISMATCH` against what the crate assumes.
2. `dlrs status`: do the three globals resolve, and does the schema walk succeed?
3. `dlrs entities`: are class names sensible?
4. `dlrs players`: does the scoreboard match what is on screen?

If a signature stops matching, that pattern has drifted and needs re-deriving from
`client.dll`. If only the schema fails, the reader keeps working on the baked fallback
table, at reduced coverage.

Constants the game never exposes to the client (the bridge-buff cadence, the tick rate,
the entity class names the snapshot matches by string) cannot be re-derived at runtime.
They live in `Tunables` with sensible defaults, and an application can override them
without waiting for a new release of this crate:

```rust,ignore
let mut reader = Reader::attach()?;
reader.tunables_mut().bridge_buff_period = 240.0;          // patched cadence
reader.tunables_mut().midboss_classes = vec!["C_NPC_MidBoss_V2".into()]; // renamed class
```

---

## The field table

`src/fields.rs` holds the 74 `(class, field)` pairs this crate looks up, each with the
fallback offset used only when a runtime schema lookup misses. It is the source of truth
and is edited by hand.

`schema-fields.json` at the crate root is the same table as JSON, for tools that will not
parse Rust. `tests/schema_fields_json.rs` fails when the two stop agreeing, so a row added
to one has to be added to the other.

Rows marked `inferred: true` are ones whose class is resolved at runtime rather than read
from a literal, so the pairing was read off surrounding context rather than proven.

## Platform support

| | Status |
|---|---|
| Windows | Works; exercised against a live client |
| Linux + Proton | Implemented, compile-verified; the `process_vm_readv` path is untested |
| Linux native | Plumbing done; needs signatures and layouts derived when a native build ships |
| macOS | Not attempted; probably no client to read |

### How the ABI split works

Signatures, fallback offsets and the schema/entity struct layouts depend on how the
**target** was compiled, not on the host. A Linux build reading a Proton-hosted game
needs the *Windows* tables, because Wine maps the real PE and the game code is still
MSVC-compiled x86-64.

So it is a runtime choice, made from the `MZ` / `ELF` magic of the mapped image:

```rust,ignore
let abi = Abi::detect(&image)?;      // Win64 under Proton, SysV for a native build
abi.signatures()                     // Option<&SignatureSet>
abi.fallback_offset(class, field)    // MSVC table, or None
abi.schema_layout()                  // Option<SchemaLayout>
abi.entity_layout()                  // Option<EntityLayout>
```

`Abi::SysV` currently returns `None` from all of them, so a native client is refused with
`Error::UnsupportedAbi` rather than being read with MSVC offsets, which would produce
confident nonsense. `dlrs status` prints the detected ABI.

### Linux

`process_vm_readv(2)` for reads (one syscall, no `PTRACE_ATTACH`, target never stopped),
and `/proc/<pid>/maps` for both modules and regions; that one file replaces
`VirtualQueryEx` *and* the Toolhelp32 snapshot.

Three Proton details are handled: pid discovery goes through `/proc/<pid>/cmdline` rather
than `comm` (truncated to 15 bytes) and prefers the process with a client module mapped,
since a prefix runs several wine processes; module lookup is by basename, because
pressure-vessel's mount namespace makes the paths in `maps` container-relative; and
`[vvar]`/`[vsyscall]` are excluded from the scan set.

**Permissions are the thing that will actually bite users.** Yama's `ptrace_scope` is `1`
on most distributions, which makes reads of a non-child fail `EPERM`. That is detected up
front and reported as `Error::PtraceDenied` with the fix:

```
not permitted to read pid 12345
  /proc/sys/kernel/yama/ptrace_scope is 1 (only descendants may be read).
  Fix one of:
    sudo setcap cap_sys_ptrace+ep <this binary>   # per-binary, preferred
    sudo sysctl -w kernel.yama.ptrace_scope=0     # session-wide, resets on reboot
```

Run the reader on the host, not inside the Proton container, and PID-namespace
differences stop mattering.

### When a native Linux build ships

The plumbing is in place; what remains is data:

| What | Why | How |
|---|---|---|
| 3 AOB signatures | Clang codegen, SysV args (`rdi/rsi/rdx`) not Win64 (`rcx/rdx/r8`) | Re-derive against `client.so`; the real work |
| `SchemaLayout` / `EntityLayout` | Itanium ABI packs structs differently | `cargo run --example probe` re-derives both |
| Fallback offsets | Same | Optional; the runtime schema covers it |

Fill in `Abi::SysV`'s arms in `src/abi.rs` and it works. One ELF wrinkle: `PT_LOAD`
segments are not necessarily contiguous the way PE sections are, so `read_image` should
scan per-segment mappings rather than assuming one span.

### macOS

Likely moot: Valve dropped macOS for CS2 and there is probably no Deadlock client;
check before investing. Under CrossOver it would be `task_for_pid()` +
`mach_vm_read_overwrite()`, where the blocker is entitlements and SIP rather than APIs.
On Apple Silicon the game runs x86-64 under Rosetta 2, so the Windows signatures would
still match.

## Legal and practical notes

Written for interoperability and to understand a program the author runs on their own
machine. It reads memory from a process on the local system; it does not modify the game,
bypass any protection, or transmit anything.

Deadlock is a live competitive game. Valve sets the policy on external readers regardless
of how non-invasive the technique is; that call is theirs, not this README's. Know the
risk before you point this at an account you care about.

## Licence

Licensed under either of the Apache License, Version 2.0 ([`LICENSE-APACHE`](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE-APACHE))
or the MIT license ([`LICENSE-MIT`](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE-MIT)), at your option.

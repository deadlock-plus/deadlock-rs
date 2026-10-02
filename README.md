# deadlock-rs

Rust libraries for reading [Deadlock](https://store.steampowered.com/app/1422450/Deadlock/)
game state. They read live data from a running client, and reference data from the
installed game or [deadlock-api.com](https://deadlock-api.com).

```
cargo run --release -p dlrs -- status
cargo run --release -p dlrs -- players
cargo run --release -p dlrs -- heroes bebop     # works with no game running
```

The libraries are read-only. They never write to the game process, inject code, hook
functions, or call into it.

## Crates

| Crate | What it is | Dependencies |
|---|---|---|
| [`deadlock-core`](crates/deadlock-core) | Shared types: `HeroId`, `Team`, `GameState`, `MatchMode`, `GameMode`, and the `HeroNames` trait | none |
| [`deadlock-memory`](crates/deadlock-memory) | Generic read-only process-memory client: attach, module and region lookup, AOB scan, RIP resolve | `windows-sys` / `libc` |
| [`deadlock-reader`](crates/deadlock-reader) | Read-only reader for a live Deadlock client, driven by the Source 2 runtime schema | `deadlock-memory`, `deadlock-core` |
| [`deadlock-data`](crates/deadlock-data) | Hero, ability and item catalogues from the installed game, a vendored snapshot, or the API | `serde_json`, optional `ureq`, optional `source2` |
| [`deadlock-walker`](crates/deadlock-walker) | Reads live Game Coordinator protobuf objects from the client's heap: party, lobby, account, hero builds, match metadata | `deadlock-memory`, `valveprotos` |
| [`deadlock-replay`](crates/deadlock-replay) | Reads published match metadata and streams Source 2 demo files without unpacking them | `bzip2-rs`, `haste_core`, `valveprotos` |
| [`deadlock-events`](crates/deadlock-events) | Polls the sources above on their own schedules and merges them into one event stream | the above |

`deadlock-core` exists so the other crates do not have to depend on each other. A hero id
means the same thing whether it came from a live client's memory or from a CDN.

## Apps

| App | What it is |
|---|---|
| [`apps/dlrs`](apps/dlrs) | `dlrs`, a command-line inspector for a running client |
| [`apps/overlay`](apps/overlay) | DeadRS Overlay, a Tauri and SvelteKit window that shows the scoreboard and your party, sized for a second monitor |

Both apps use the crates above. Neither is published, so they live here and not under
`crates/`.

## Dependencies outside crates.io

Three dependencies come from git and are pinned to a revision:

* `valveprotos` and `haste_core` from [deadlock-api](https://github.com/deadlock-api/deadlock-api).
  The workspace uses `haste_core` and not `haste`, because `haste` pulls in `reqwest` and
  `tokio`.
* `source2` from [deadlock-plus/source2-rs](https://github.com/deadlock-plus/source2-rs).
  `deadlock-data` uses it behind the `vpk` feature to read the game archive.

`valveprotos` generates its types with `protoc`, so `protoc` must be on your `PATH` to build.

## Offline and online

Everything works with no network, and nothing requires the game to be running.

| | No game, no network | Game installed | Game running | Network |
|---|---|---|---|---|
| Hero id to name | vendored snapshot | adds the game's own localisation, 29 languages | no change | adds heroes released since this crate was built |
| Item or ability id to name | vendored snapshot | adds the game's own localisation | no change | adds items added since this crate was built |
| Live match state | not available | not available | full | not available |

The layers are a bundled snapshot, then the installed game's localisation, then an optional
API refresh. Each layer is optional and improves on the one before. Each sits behind a
feature: `bundled` and `client` are on by default, and `online` is opt-in. Ids, display
names and art merge as separate parts, so a source that carries only some of them cannot
blank the rest. `provenance()` says which source answered for each part.

```rust
use deadlock_data::HeroCatalog;
use deadlock_core::{HeroId, HeroNames};

let mut heroes = HeroCatalog::bundled();          // never fails, no I/O
assert_eq!(heroes.hero_name(HeroId(1)), Some("Infernus"));

let _ = heroes.merge_game_dir(".../Deadlock/game/citadel"); // correct + localised
# #[cfg(feature = "online")]
let _ = deadlock_data::api::fetch_heroes("english").map(|live| heroes.merge(&live));
```

`dlrs` does the same, and finds the game directory from the `client.dll` path the reader
already knows.

## Reading a live client

```rust
use deadlock_reader::Reader;

let reader = Reader::attach()?;                 // ~270 ms, once
if let Some(live) = reader.live_snapshot()? {   // ~135 µs per tick
    println!("{} - {} players", live.describe(), live.scoreboard().count());
}
# Ok::<(), deadlock_reader::Error>(())
```

Field offsets come from the game's own runtime schema, so a game patch does not break the
reader the way a table of fixed offsets would.

`Reader` is `Send + Sync`. It fits in a `tauri::State`, or in an `Arc` behind a polling
thread. See [`crates/deadlock-reader/examples/poll.rs`](crates/deadlock-reader/examples/poll.rs).

The client does not expose a few constants: the bridge-buff cadence, the tick rate, and the
entity class names matched by string. They live in `Tunables` and can be overridden at
runtime, so a patch that retunes one does not force a library update.

```rust
let mut reader = deadlock_reader::Reader::attach()?;
reader.tunables_mut().bridge_buff_period = 240.0;  // Valve changed the cadence
# Ok::<(), deadlock_reader::Error>(())
```

## What the reader exposes

* Match id, phase, mode, and Hideout detection.
* A derived match clock, per-minute economy rates taken over played time, and a Midboss
  respawn countdown.
* Per-team totals: souls, kills, deaths, assists, last hits, denies, hero and objective
  damage, healing, ability points, rejuvenators, and structures standing. Helpers compute
  the soul lead.
* A per-player scoreboard with items, ability upgrade points, ultimate state, Rejuvenator
  and Rebirth buffs, rank, abandon and cheater flags, and each stat split by the modifier
  that produced it.
* Map structures with health and destruction state, and Street Brawl round state.
* Objectives: rift capture progress and cash-in, Urn carry and countdowns, and Midboss
  health.

Hero, item and ability ids resolve to names through `deadlock-data`.

The reader names modifiers (buffs, debuffs, crowd control, backdoor protection, parries)
from the object's own vtable through RTTI. The names are the exact runtime classes, so no
lookup table can go stale. `deadlock-events` builds cross-tick totals on top, such as crowd
control received per player, in seconds taken from the game's tick-exact durations and not
from differencing polls.

Two features are off by default, for different reasons. Live world positions sit behind
`deadlock-reader`'s `positions` feature as a matter of principle, because everything else
here is information the game already shows on your screen. Modifier reads sit behind its
`modifiers` feature because of cost. Against a live client they roughly triple the time a
snapshot takes.

Some data the client never receives, and this workspace does not guess at it. The rift
spawn cadence is one: `m_timeNextKothSpawn` exists on the server rules class and on no
networked class, so there is no next-rift countdown. Bridge-buff timings come from the
schedule and are marked that way.

Chat history is not available. It lives in the Panorama UI layer, outside the entity and
schema systems.

## Game Coordinator objects

Party membership, lobby, account stats, hero builds and match metadata are not in the
entity or schema systems. Across a live client's 3605-class schema, the only field whose
name contains "party" is `CCitadelPlayerController::m_bInPartyChat`, a chat-channel flag.
The client does keep the Game Coordinator's shared objects in its heap, as live C++
protobuf messages. `deadlock-walker` reads them.

The walker never searches for serialised bytes. It works in four steps:

1. It reads the message's vtable from the client's RTTI.
2. It finds heap objects whose first word equals that vtable.
3. It reads each field's offset and has-bit from the protobuf tables compiled into
   `client.dll`.
4. It writes the object out as wire bytes, which `valveprotos` decodes.

Objects can be freed while the walker reads them, so it re-validates its pinned objects on
every poll. A freed object reads as `None`, and the caller retries.

```
dlrs party      # locate and print the party roster
dlrs account    # account stats and hero builds
dlrs events     # match, party and post-game events on one stream
```

### Post-game capture

The client holds a finished match's `CMsgMatchMetaDataContents` for a short time after the
post-game screen opens. `PostGameSource` in `deadlock-events` reads it from the heap. This
gives the full scoreboard, accolades and objectives seconds after the match ends, without
the replay CDN.

* The first read comes 5 s after the phase becomes `PostGame`. Reads repeat every 2 s.
* The first complete copy is emitted as `PostGameEvent::Captured`.
* The source keeps reading until 60 s have passed. Each read that differs from the last
  emitted copy is emitted as `PostGameEvent::Updated`.
* If no complete copy turns up in 60 s, the source emits `PostGameEvent::Missed`. This is
  normal when the player leaves the post-game screen early, because the client only holds
  the match whose screen is open.

`deadlock-events` runs each source on its own thread. A finished match is readable for only
12 to 19 seconds, so a slow heap search in one source must not delay the match source.

## Platform support

| | Status |
|---|---|
| Windows | Works, and is tested against a live client |
| Linux with Proton | Implemented and compiles. The `process_vm_readv` path is untested |
| Linux native | The memory code is done. Signatures need deriving if a native build ships |
| macOS | Not attempted. There is probably no client to read |

`deadlock-core`, `deadlock-data`, `deadlock-memory` and `deadlock-reader` cross-compile to
`x86_64-unknown-linux-musl` and `aarch64-apple-darwin` with no warnings.

`deadlock-data` finds an installed game by itself. It reads Steam's library folders and
scans the usual locations, so the installed-game sources work with Deadlock closed. Set
`DEADLOCK_CITADEL_DIR` to the `citadel` directory to override this. That is the reliable
fix for an unusual Steam layout, because reading Steam's registry key would need `unsafe`
code or another dependency.

The workspace MSRV is 1.88, checked in CI. The TLS stack behind `deadlock-data`'s `online`
feature sets that floor, because its `icu_collections` and `icu_locale_core` crates declare
1.88. The offline dependencies need 1.87 at most, set by `ruzstd`, the pure-Rust zstd
decoder that `source2` uses for a compiled `vdata_c`. All decompressors are pure Rust, so
`#![forbid(unsafe_code)]` holds across the workspace and no C toolchain is needed.

The `online` feature pulls in a TLS stack whose build script needs a C cross-compiler.
Cross-building that feature, and so the CLI which enables it by default, needs a matching
toolchain such as `musl-gcc`. The offline paths have no C dependencies.

## Documentation

Each crate's README covers its own internals. Two more files cover the reader:

* [`crates/deadlock-reader/LAYOUT.md`](crates/deadlock-reader/LAYOUT.md) describes
  Deadlock's memory and schema layout, and where the reader's constants come from.
* [`crates/deadlock-reader/schema-fields.json`](crates/deadlock-reader/schema-fields.json)
  is the class and field table as JSON.

## Licence

Licensed under either of the Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
or the MIT license ([`LICENSE-MIT`](LICENSE-MIT)), at your option.

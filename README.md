# deadlock-rs

Rust libraries for reading [Deadlock](https://store.steampowered.com/app/1422450/Deadlock/)
game state: live from a running client, plus reference data from the installed game or
[deadlock-api.com](https://deadlock-api.com).

```
cargo run --release -p dlrs -- status
cargo run --release -p dlrs -- players
cargo run --release -p dlrs -- heroes bebop     # works with no game running
```

## Crates

| Crate | What it is | Dependencies |
|---|---|---|
| [`deadlock-core`](crates/deadlock-core) | Shared vocabulary: `HeroId`, `Team`, `GameState`, `MatchMode`, `GameMode`, the `HeroNames` trait | none |
| [`deadlock-memory`](crates/deadlock-memory) | Generic read-only process-memory client: attach, module and region lookup, AOB scan, RIP resolve | `windows-sys` / `libc` |
| [`deadlock-reader`](crates/deadlock-reader) | Read-only external reader for a live Deadlock client, driven by the Source 2 runtime schema | `deadlock-memory`, `deadlock-core` |
| [`deadlock-data`](crates/deadlock-data) | Hero, ability and item catalogues from the installed game, a vendored snapshot, or the API | `serde_json`, optional `ureq`, optional `source2` |
| [`deadlock-walker`](crates/deadlock-walker) | Walks live Game Coordinator protobuf objects in the client's heap: party, lobby, account, hero builds | `deadlock-memory`, `valveprotos` |
| [`deadlock-replay`](crates/deadlock-replay) | Reads published match metadata and streams Source 2 demo files without unpacking them | `bzip2-rs`, `haste_core`, `valveprotos` |
| [`deadlock-events`](crates/deadlock-events) | Polls the sources above on their own schedules into one event stream | the above |

`deadlock-core` exists so nothing else has to depend on its siblings. A hero id means
the same thing whether it came out of a live client's memory or off a CDN.

## Apps

| App | What it is |
|---|---|
| [`apps/dlrs`](apps/dlrs) | **`dlrs`** - a command-line inspector for a running client |
| [`apps/overlay`](apps/overlay) | **DeadRS Overlay** - a Tauri + SvelteKit window showing the scoreboard and your party, sized for a second monitor |

Both consume the crates above and neither is published, so they live here rather than
under `crates/`.

## Offline and online

Everything works with no network. Nothing *requires* the game to be running.

| | No game, no network | Game installed | Game running | Network |
|---|---|---|---|---|
| Hero id -> name | vendored snapshot | + the game's own localisation, 29 languages | - | + heroes released since this crate was built |
| Item / ability id -> name | vendored snapshot | + the game's own localisation | - | + items added since this crate was built |
| Live match state | - | - | full | - |

The intended layering is bundled snapshot -> installed game's localisation -> optional
API refresh. Each step is optional and improves on the last, and each is behind a feature
(`bundled` and `client` on by default, `online` opt-in). Ids, display names and art merge
as independent facets, so a source that carries only some of the three cannot blank the
rest; `provenance()` says which source answered for each:

```rust
use deadlock_data::HeroCatalog;
use deadlock_core::{HeroId, HeroNames};

let mut heroes = HeroCatalog::bundled();          // never fails, no I/O
assert_eq!(heroes.hero_name(HeroId(1)), Some("Infernus"));

let _ = heroes.merge_game_dir(".../Deadlock/game/citadel"); // correct + localised
# #[cfg(feature = "online")]
let _ = deadlock_data::api::fetch_heroes("english").map(|live| heroes.merge(&live));
```

`dlrs` does exactly this, and finds the game directory automatically from the
`client.dll` path the reader already knows.

## Reading a live client

```rust
use deadlock_reader::Reader;

let reader = Reader::attach()?;                 // ~270 ms, once
if let Some(live) = reader.live_snapshot()? {   // ~135 µs per tick
    println!("{} - {} players", live.describe(), live.scoreboard().count());
}
# Ok::<(), deadlock_reader::Error>(())
```

Strictly read-only: no `WriteProcessMemory`, no injection, no hooking, no calls into the
target. Field offsets come from the game's own runtime schema, so the reader survives
game patches rather than breaking on each one.

`Reader` is `Send + Sync`, so it drops straight into a `tauri::State` or an `Arc` behind
a polling thread. See [`crates/deadlock-reader/examples/poll.rs`](crates/deadlock-reader/examples/poll.rs).

The few constants the client does not expose (the bridge-buff cadence, the tick rate,
the entity class names matched by string) live in `Tunables` and can be overridden at
runtime, so a game patch that retunes one of them does not force a library update:

```rust
let mut reader = deadlock_reader::Reader::attach()?;
reader.tunables_mut().bridge_buff_period = 240.0;  // Valve changed the cadence
# Ok::<(), deadlock_reader::Error>(())
```

## What is exposed

Match id, phase, mode and Hideout detection; a derived match clock, per-minute economy
rates taken over played time rather than elapsed, and a Midboss respawn countdown;
per-team totals (souls, k/d/a, last hits, denies, hero/objective damage, healing, ability
points, rejuvenators, structures standing) with soul-lead helpers; per-player scoreboard
plus items, per-ability upgrade points, ultimate state, Rejuvenator/Rebirth buffs, rank,
abandon/cheater flags, and per-stat attribution decomposed by the modifier that produced
it; map structures with health and destruction state; Street Brawl round state; and the
objectives - rift capture progress and cash-in, Urn carry and countdowns, Midboss health.
Hero, item and ability ids resolve to names through `deadlock-data`.

Modifiers - buffs, debuffs, crowd control, backdoor protection, parries - are named from
the object's own vtable through RTTI, so they are exact runtime classes rather than a table
that goes stale. `deadlock-events` builds cross-tick totals on top: crowd control received
per player, in seconds taken from the game's own tick-exact durations rather than from
differencing polls.

Two feature gates, for different reasons. Live world positions sit behind
`deadlock-reader`'s `positions` feature on principle - everything else here is information
the game already puts on your screen. Modifier reads sit behind its `modifiers` feature on
cost: measured against a live client they roughly triple what a snapshot takes.

Some things the client is simply never told, and this workspace says so rather than
guessing. The rift's spawn cadence is one: `m_timeNextKothSpawn` exists on the *server*
rules class and on no networked one, so there is no next-rift countdown and none is
invented. Bridge-buff timings are schedule-derived and marked as such.

Chat history is **not** available: it lives in the Panorama UI layer, not the entity or
schema systems, so nothing in the client schema reaches it.

## Party membership

Party membership is **not** in the entity or schema systems. Across a live client's full
3605-class schema the only field whose name even contains "party" is
`CCitadelPlayerController::m_bInPartyChat`, a chat-channel flag. What the client does keep
is the Game Coordinator's shared objects, sitting in its heap as serialised protobuf, and
`deadlock-walker` finds them.

There is no pointer to follow, so the object is located by searching for the local account
id encoded as a protobuf varint. That gives two speeds:

| Phase | Cost |
|---|---|
| Full sweep, cold | ~1.2 s (release) |
| Re-find from a hint | ~0.3 s |
| Pinned re-read | ~2.6 us |

```
dlrs party      # locate and print the party roster
dlrs events     # match and party events on one stream
```

Because a sweep takes longer than the 12-to-19-second window in which a finished match is
still readable, `deadlock-events` runs every source on its own thread; a sweep can never
delay the match source past that window.

## Platform support

| | Status |
|---|---|
| Windows | Works; exercised against a live client |
| Linux + Proton | Implemented, compile-verified; the `process_vm_readv` path is untested |
| Linux native | Plumbing done; needs signatures derived if a native build ships |
| macOS | Not attempted; probably no client to read |

`deadlock-core`, `deadlock-data`, `deadlock-memory` and `deadlock-reader` all cross-compile clean to
`x86_64-unknown-linux-musl` and `aarch64-apple-darwin` with zero warnings.

`deadlock-data` finds an installed game on its own — Steam's library folders, plus a scan
of the usual locations — so the installed-game sources work with Deadlock closed, not just
while it is running. Set `DEADLOCK_CITADEL_DIR` to the `citadel` directory to override
that; it is the reliable answer for an unusual Steam layout, since reading Steam's registry
key would mean `unsafe` code or another dependency.

The workspace MSRV is **1.88**, checked in CI. The floor is set by the TLS stack behind
`deadlock-data`'s `online` feature, whose `icu_collections` and `icu_locale_core` declare
1.88; the offline tree bottoms out at 1.87, from `ruzstd`, the pure-Rust zstd decoder
`source2` needs for a compiled `vdata_c`. The decompressors are pure Rust, so
`#![forbid(unsafe_code)]` holds across the workspace and none needs a C toolchain.

One caveat: `deadlock-data`'s optional `online` feature pulls in a TLS stack whose build
script needs a C cross-compiler, so cross-building *that* feature (and therefore the CLI,
which enables it by default) needs a matching toolchain such as `musl-gcc`. The offline
paths have no C dependencies.

## Documentation

Each crate's README covers its own internals. Beyond those:

* [`crates/deadlock-reader/LAYOUT.md`](crates/deadlock-reader/LAYOUT.md): Deadlock's
  memory and schema layout, and the provenance of the constants the reader depends on
* [`crates/deadlock-reader/schema-fields.json`](crates/deadlock-reader/schema-fields.json):
  the class/field table as machine-readable JSON

## Licence

LGPL-3.0-or-later. See [LICENSE.md](LICENSE.md).

The LGPL is a copyleft licence. Modifications to this library must be released
under the same terms; an application that merely uses it need not be.

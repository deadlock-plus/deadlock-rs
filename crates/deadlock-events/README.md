# deadlock-events

A multi-source event engine for Deadlock. Each source polls on its own schedule; you get
one merged stream.

| Source | Reads | Poll cost | Sensible interval |
|---|---|---|---|
| `ReaderSource` | entity and schema systems | a few hundred microseconds | 50 to 100 ms |
| `PartySource` | Game Coordinator shared objects | 2.6 us pinned, ~0.3 s to re-locate | 1 to 2 s |
| `PostGameSource` | game phase, then the match metadata object | one snapshot per tick; a heap search per attempt | 500 ms |

## Why a thread per source

A cold Game Coordinator sweep measured 1.2 seconds, and a finished match stays readable for
only 12 to 19 seconds. On a shared thread one sweep could swallow the whole window and
lose the final scoreboard, so the cadences have to be independent.

```rust,ignore
let (_engine, rx) = Engine::new()
    .with(ReaderSource::new(Arc::clone(&reader)).every(Duration::from_millis(100)))
    .with(PartySource::new(Arc::clone(&reader), account).every(Duration::from_secs(2)))
    .start()
    .expect("the engine could not spawn its threads");

for note in rx {
    println!("{note:?}");
}
```

Dropping the handle stops every thread.

## Health is per source

"No party is resident" and "the game closed" are different situations. `Health` is
reported per source and only when it changes, so a healthy source is silent. `Health::Idle`
is the ordinary state for solo play and is not an error.

## Post-game capture

`PostGameSource` captures each finished match's `CMsgMatchMetaDataContents` from the
client's heap. When a real match enters `PostGame` it waits 5 s, then reads every 2 s until
the metadata for that match is resident and complete, or 60 s pass. It emits exactly one
`Event::PostGame` per match: `Captured { match_id, metadata }`, or `Missed { match_id,
attempts }` if the deadline passes.

- The match id is remembered from while the match was live, since it can read zero after.
- A flapping phase does not re-emit; a new match id re-arms; a game restart resets.
- Street Brawl (`PostGame` straight to `GameInProgress`) and normal matches (via `End`)
  both work: the window outlives the phase.
- Only the match whose post-game screen is up is resident, so `Missed` is normal when the
  player leaves the screen early.
- Health is `Idle` with no window open, `Ok` while capturing, `Failed` on a poll error.
- Timing is configurable with `PostGameSource::timing`. The policy itself is
  `CapturePolicy`, a pure state machine with an injected clock and fetcher.

## Crowd control taken (`crowd-control` feature)

Nothing in the client totals how long a player spent stunned; that number only exists if
something watches successive snapshots and adds it up. `crowd_control::CrowdControlTracker`
does, and reports it under Companion's own keys:

```rust,ignore
for (key, value) in tracker.player(slot).unwrap().metrics() {
    println!("{key} = {value}"); // cc.received.slow.events = 6
}                                // cc.received.slow.seconds = 26.21875
```

Seconds come from each modifier's own `m_flDuration`, which is tick-exact off
`m_flSimulationTime`, rather than from deltas between polls - so the answer does not depend
on how often you poll, and one sighting of a stun shorter than the poll gap is enough to
count it in full. Off by default: it needs `deadlock-reader/modifiers`, which roughly
triples the cost of a tick.

## Ability landed casts (`landed-casts` feature)

`landed_casts::LandedCastTracker` groups modifier applications into casts and reports, per
ability, `landed_casts`, `enemies_hit`, `max_enemies_in_cast` and
`avg_enemies_per_landed_cast`.

**The grouping is inferred.** Companion's output *shape* was read off its own files; how it
computes those numbers was never traced. This implements the inferred mechanism -
applications sharing an `m_hAbility` within an `m_flCreationTime` window, across distinct
enemy targets - and nothing more. The window width is a tunable, `DEFAULT_CAST_WINDOW`,
whose doc comment spells out what setting it too narrow and too wide each does.

Casts are **grouped** by `m_hAbility`, which distinguishes one ability instance from
another, and **reported** under the modifier's `m_nAbilitySubclassID`, which is a
`CUtlStringToken` of the ability's `scripts/abilities.vdata_c` key - measured against a
live client, where hashing the C++ class name matched 0 of 17 ids and the vdata keys
matched 15 of 15. So `AbilityCastTotals::subclass_id` is an `ItemId` any consumer holding
the ability roster resolves to a display name; this crate takes no dependency on
`deadlock-data` to do it. Keys read `ability.subclass_2335418656.landed_casts`, or
`ability.handle_0x00018005.landed_casts` for the fallback when no subclass id was
readable - both tagged, so the two kinds are never confused.

Ledgers are still keyed by `m_hCaster`, raw: nothing in a `LiveSnapshot` turns an entity
handle into a player row, and modifiers carry no equivalent id for the applier. Off by
default, on the same `deadlock-reader/modifiers` read as `crowd-control`.

## Objective-context buckets (`objective-context` feature)

`objective_context::ObjectiveContextTracker` buckets a player's stats by *where* they
happened rather than only totalling them, which is the shape Companion's own output has:

```rust,ignore
for (key, value) in tracker.player(slot).unwrap().metrics() {
    println!("{key} = {value}"); // midboss.kills = 2
}                                // midboss.time_seconds = 41.5
                                 // open.hero_damage = 18422
```

plus match-level `urn_fights`, `urn_fight_seconds` and `rift_fights`.

**Every rule in it is inferred.** The output shape was read off Companion's own files; how
it computes the bucketing was never traced. The radii (`MIDBOSS_RADIUS`, `WALKER_RADIUS`,
`RIFT_RADIUS`), the attribution rule, the bucket precedence and the definition of "a fight"
are all choices made here, each a named constant whose doc comment says what getting it
wrong does in each direction. None of them has been checked against a live match: the
client this was written against sat in the Hideout, which reports zero objectives.

Three things it is deliberately explicit about:

- It needs `deadlock-reader/positions` at **compile time**. Without coordinates nothing can
  be attributed, and a tracker that answers "everything is `open`" emits Companion-shaped
  keys whose zeros nobody can tell from measurements. So a build without positions does not
  get a tracker.
- A player with no readable position this tick is bucketed **nowhere** - not into `open`.
  What that costs is visible in `unattributed.*` and `unattributable_intervals()`.
- `urn.time_seconds` is **not** a proximity metric. A `LiveSnapshot` carries no Urn
  position at all, so it counts seconds alive while an Urn was on the map, which is a
  strict superset of what the key can mean. The module says so at every level.

Cheaper than the other two derived modules: `positions` is one extra pointer chase per
entity, where `modifiers` re-reads every player's modifier list.

## Durable event timeline (`timeline` and `timeline-jsonl` features)

`EventTracker` produces a stream of changes and then forgets them. Companion keeps its
equivalent stream, one row per event; `timeline::TimelineEvent` is that row and
`timeline::EventSink` is the hole a consumer plugs its storage into.

```rust,ignore
let sink = JsonlSink::open("timeline.jsonl")?;      // append-only, one JSON object per line
let mut recorder = TimelineRecorder::new(sink);

for event in tracker.update(&snap) {                // once per tick
    let _ = recorder.record(&event, snap.clock.playing_seconds());
}
assert!(recorder.status().is_healthy());            // and out of band, whenever you like
```

```json
{"match_id":"38176045","kind":"kill","slot":3,"clock":612.5,"offset_ms":90000,"seq":41}
{"match_id":"38176045","kind":"death","slot":8,"clock":612.5,"offset_ms":90000,"seq":42}
```

Companion's five columns under Companion's names, plus `seq`. What is different, and why:

- **`kind` is an enum that serialises as a string.** A consumer grepping the file wants
  `"kind":"kill"`; a consumer in Rust wants an exhaustive `match`. One field does both.
  There is no `assist` kind - `PlayerRow::assists` is read, but nothing diffs it into an
  event, and a column that is always empty looks like a measurement that came out zero.
- **`clock` and `offset_ms` are two different clocks and the record never conflates them.**
  `clock` is the in-game match clock in seconds, pauses removed; it can be absent, can stand
  still, and restarts at zero with the next match. `offset_ms` is milliseconds since the
  recorder was constructed, from a monotonic `Instant`; it is never absent and never goes
  backwards. **An unreadable match clock is `null`, never `0`**, because `0` reads as "at
  match start". A non-finite reading is `null` too: `NaN` is not a time.
- **`seq` is the recorder's own counter**, the one field with no source in the game. Several
  events routinely land on one tick and share both other clocks, so `(clock, offset_ms)`
  does not order them and `(clock, offset_ms, seq)` does.
- **`match_id` is a JSON string, and 64 bits wide.** `MatchID_t` is a `uint64`; a JSON
  *number* is a double to `jq` and to every browser, which silently rounds anything above
  2^53.
- **No field the events cannot supply.** No `video_path`, no `clip_id`: Companion's timeline
  indexes a video recording and this crate has no recording. The scoreboard attached to
  `MatchEnded` is deliberately dropped - the timeline is an index of when things happened,
  not an archive of state.

### The sink, and what it actually guarantees

`EventSink` is two methods, one defaulted, so a consumer wanting SQLite implements it in a
dozen lines and this crate never picks a database. `write` returns a `Result`, because a
sink that swallows failures makes a full disk look like a quiet match; `TimelineRecorder`
also counts failures in `RecorderStatus`, so a poll loop that deliberately writes
`let _ = recorder.record(..)` can still find out, on its own schedule, the same way `Health`
works.

`JsonlSink` opens with the append flag and never seeks, so **records already on disk are
never rewritten**. What it does *not* claim: it is not atomic per record, and `flush` is a
write to the OS and not an `fsync` - a process crash loses nothing already flushed, a power
cut may. After a failed write it starts the next record on a fresh line, so a torn write
damages exactly one line and the records either side of it still parse. **A reader must
therefore skip lines that do not parse, and tolerate a partial line at the end of the file.**

Flushing defaults to `FlushPolicy::EveryRecord`: at the rate this crate produces events one
`write` syscall per record is not worth trading durability for. `FlushPolicy::every(n)`
batches, `FlushPolicy::Manual` hands the decision over entirely, and dropping the sink
flushes what is buffered as a best-effort backstop.

Two features rather than one: `timeline` is the record and the trait and needs only `serde`,
`timeline-jsonl` adds the file sink and `serde_json`. A consumer writing to a database
should not have to compile a JSON encoder it will never call. Both dependencies are already
in this workspace's lockfile, both pure Rust, both MIT/Apache-2.0.

## Adding your own

`Source` is a name, an interval and a `poll`. Anything implementing it joins the same
stream with the same health reporting.

## Licence

LGPL-3.0-or-later. See [LICENSE.md](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE.md).

The LGPL is a copyleft licence. Modifications to this library must be released
under the same terms; an application that merely uses it need not be.

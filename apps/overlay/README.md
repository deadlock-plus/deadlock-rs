# DeadRS Overlay

A small window showing the live scoreboard and your Steam party, sized to sit on a second
monitor rather than over the game. Tauri + SvelteKit over the `deadlock-rs` crates.

Read-only. Everything it shows is information the game already puts on your screen; it is
collected here so it can sit somewhere you can glance at instead of being paged through.

## Running it

```sh
pnpm install
pnpm tauri dev
```

`pnpm tauri build` produces a bundle. A bare `cargo build -p deadrs-overlay` will compile
but **will not work**: the frontend is embedded at build time by `tauri-build`, so the
window has nothing to load unless the SvelteKit app has been built first. Go through
`pnpm tauri` and that happens for you.

## How it is put together

```
src/                 SvelteKit frontend
  lib/               Icon, PlayerRow, TeamPanel, formatting helpers
  routes/+page.svelte
src-tauri/
  src/model.rs       what the window is sent - plain serialisable views
  src/poll.rs        two poller threads
  src/main.rs        window setup
```

**Two pollers, on two threads**, because the two sources have nothing in common but being
polled. The entity reader answers in microseconds and wants a tight interval. The Game
Coordinator scan usually answers from a pin just as fast, but when a pin dies it can spend a
second sweeping the heap - and sharing a thread would let one of those sweeps eat a dozen
scoreboard frames. They get one each and the window merges whatever arrives.

Both share one supervised `Reader`, so the game is opened once and both recover together
when it restarts.

## What `model.rs` guarantees

The types in `model.rs` are deliberately their own set rather than the reader's types
serialised directly. The window wants strings it can paint, and pinning that shape here
means a change in the reader shows up as a compile error instead of a blank panel.

Two things worth knowing if you touch it:

- **Every counter is flattened to a number.** A field the reader could not read arrives as
  `0` and is indistinguishable from a real zero. That is the right trade for a panel
  refreshed several times a second - a row that blanks out mid-match reads as a bug - and it
  means nothing in a `PlayerView` should be treated as authoritative for anything else.
- **Both teams are always present, Amber first.** The window indexes `teams[0]` and
  `teams[1]` directly, so a side the reader has not managed to read yet still gets a row,
  carrying its own team number rather than a default.

## Tests

`cargo test -p deadrs-overlay` covers `model.rs`, which is the part with logic in it. The
pollers are threads around a supervised reader and are not worth faking a game for.

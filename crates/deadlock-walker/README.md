# deadlock-walker

Reads Game Coordinator protobuf objects out of a running Deadlock client without copying
serialised bytes. It resolves message vtables from the client's RTTI, finds live C++
instances on the heap, derives each message's real field layout from the protobuf tables
compiled into `client.dll`, and walks the object into wire bytes that `valveprotos` decodes.

`GcSession` is the high-level entry point: party, lobby, account, stats and hero builds,
with cheap pinned re-reads for poll loops.

Match metadata (`CMsgMatchMetaDataContents`) is anchored on the match, not the account.
`GcSession::match_metadata(mem, match_id)` returns the resident copy of one match and
`match_metadata_all` lists every one. Only complete objects count (non-zero
`match_info.match_id`, at least one player); an object freed mid-walk is `Ok(None)`, so
retry. The client keeps only the matches the player has opened, so a match that is not
resident is `None`. See `examples/metadata.rs`.

## Platforms

The walker only parses bytes and talks to a `deadlock_memory::MemoryReader`, so it builds on
every target (`cargo check -p deadlock-walker --target <t>`). Reading a live game needs a
memory backend: Windows, and Linux for the Proton/Wine build, where the game is still a PE
image (`client.dll`, MSVC RTTI) inside a Linux process. A native ELF client is not supported.
The examples compile everywhere and exit with a message where no backend exists.

## Live checks

Run in the Hideout with the game open. Use `--release`: a debug sweep is several times slower.

- Reach: `cargo run --release -p deadlock-walker --example kinds`. A signed-in client holds
  a `CSOGameAccountClient` and one `CSOAccountHeroInfo` per hero. Zero hits for those means
  the heap search is not covering the memory they live in (a region cap in the memory
  backend), not that they are absent.
- Solo queue: start a queue alone, then `cargo run --release -p dlrs -- party`. Expected: a
  party with one member, a `queue` line (`Normal / ...`) and `matchmaking for m:ss`, and
  `PartyExt::is_queueing()` true. Cancel the queue and run it again: `not queueing`.
- Polling cost: `cargo run --release -p deadlock-walker --example gc` prints the first sweep
  and the pinned re-read time.

Read-only. Pure Rust, `forbid(unsafe_code)`.

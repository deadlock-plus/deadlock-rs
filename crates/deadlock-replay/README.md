# deadlock-replay

Decoder for Deadlock's post-match replay metadata.

When a match ends, Valve publishes a `CMsgMatchMetaData` for it, bzip2'd, at

```
http://replay{cluster}.valve.net/1422450/{match_id}_{metadata_salt}.meta.bz2
```

It is the only source for several numbers. Weapon accuracy (`shots_hit` / `shots_missed`),
`headshot_kills`, `damage_mitigated` and `heal_prevented` have no live-memory equivalent,
and neither do per-death positions, the damage matrix, or objective first-damage times.

```rust
use deadlock_replay::MatchMetaData;

# fn downloaded() -> Vec<u8> { Vec::new() }
# fn main() -> Result<(), deadlock_replay::Error> {
# let payload = downloaded();
# if payload.is_empty() { return Ok(()); }
let meta = MatchMetaData::from_bz2(&payload)?;
let info = meta.contents()?.match_info.expect("a finished match has match_info");
# Ok(())
# }
```

## No network

This crate decodes bytes it is handed. Fetching them is the caller's job, the same way
`deadlock-data` is given a path rather than going looking for the game's install. Build the
URL with `deadlock_reader::replay_meta_url`; the cluster id, match id and metadata salt all
come off the live lobby.

## The message is a wrapper

`CMsgMatchMetaData` has three fields, and the interesting one — `match_details` — is
declared `bytes`, not a submessage. So the details are a second serialised message,
`CMsgMatchMetaDataContents`, decoded separately by `MatchMetaData::contents`. A caller who
only wants the match id never pays for the rest.

## Field numbers

Transcribed from Deadlock's own `citadel_gcmessages_common.proto`, as published at
<https://github.com/SteamTracking/GameTracking-Deadlock>. Transcribed, not vendored: the
definitions are Valve's.

The proto is full of gaps where fields were removed — `Objective` has no field 3, `Deaths`
no 3 through 8, `Players` no 7, 16 or 24, `MatchInfo` no 7 or 31, `PlayerRankData` no 5,
and both `CMsgMatchMetaDataContents` and `CustomUserStat` start above field 1 — and every
one is transcribed as a gap rather than closed up.

Enumerations are kept as raw numbers with the upstream enumeration named in the doc
comment, so a patch that adds a variant cannot turn a decode into a failure.

## The other kind of replay file

`.dem` files are Source 2 demos, not metadata, and they are large — the 37 local ones run
269 MB to 773 MB. `deadlock_replay::demo` streams them a packet at a time:

```rust
use deadlock_replay::{census, read_file_header, DemoLimits};

# fn main() -> Result<(), deadlock_replay::Error> {
# let path = std::path::Path::new("nonexistent.dem");
# if !path.exists() { return Ok(()); }
let header = read_file_header(path)?;
println!("{:?} on {:?}", header.server_build(), header.map_name);

let c = census(path, DemoLimits::default())?;
println!("{} packets, {} compressed, budget hit: {}", c.packets, c.compressed_packets, c.hit_budget);
# Ok(())
# }
```

Two ceilings, meaning different things. `max_total_bytes` is how much of the *file* may be
read — reaching it is the expected end of a bounded scan, which `census` reports as
`hit_budget` rather than failing on. `max_packet_bytes` is how big any *one* packet may be,
checked against the declared frame size before the payload is read and again against the
declared decompressed length before a byte is produced.

Nothing is ever fully resident: walking all 358,931,388 bytes of `32433914.dem` — 53,842
packets, 53,758 of them compressed — takes 1.5 s and peaks at 5.5 MB of working set, the
largest single packet being 295,893 bytes.

Compressed packets are raw-block snappy, hand-rolled here for the same reasons the protobuf
reader is: no dependency, no C toolchain, `forbid(unsafe_code)` intact. Snappy and not LZ4
is measured, not assumed — every compressed packet in that whole-file walk decodes, and each
to exactly the length its header declares, while the first packet read as LZ4 opens with a
match at offset 47,509 against 13 bytes of output.

### Where it stops, and the ban list

This reads packets. It does not decode what is inside `CDemoPacket` field 3, where the net
messages are bit-packed with a `ubitvar` message type — a separate and much larger job
needing the field serializers from `DEM_SendTables` and an entity decoder on top.

That boundary is exactly where the ban list sits, and the finding matters more than the
feature. Searching the decompressed send tables of all 37 local replays:

- `m_vecBannedHeroes` appears in exactly one file, `98226635.dem` at `citadel_v6670`,
  alongside `m_bAbandon`, `m_bMatchSafeToAbandon` and friends. It is absent from the other
  36, which span `citadel_v5488` to `citadel_v6640`.
- `CCitadelUserMsg` gets **zero** hits in every file, v6670 included.

So the ban list is entity data on the game-rules class, not a user message — which means
naming `CCitadelUserMsgBannedHeroes` as the carrier does not describe these files. `tables_contain` is what measures the claim, and `DemoFileHeader::server_build` is
what says whether a given replay's build could carry the field at all.

## Decompression

Pure Rust, via [`bzip2-rs`](https://github.com/paolobarbolini/bzip2-rs) (MIT/Apache-2.0).
The `libbz2` bindings would need a C toolchain, which the cross-compile job does not have.
`#![forbid(unsafe_code)]` throughout.

`decompress` applies a 64 MiB ceiling as output accumulates; `decompress_capped` takes your
own. Both refuse a payload that does not open with a bzip2 header before decoding anything,
because the common failure is the CDN answering with an error page rather than a stream.

## Features

| Feature | Default | What it adds |
|---|---|---|
| `bzip2` | yes | `decompress`, `decompress_capped` and `MatchMetaData::from_bz2`. Without it the crate decodes already-decompressed bytes only. |

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

Read-only. Pure Rust, `forbid(unsafe_code)`.

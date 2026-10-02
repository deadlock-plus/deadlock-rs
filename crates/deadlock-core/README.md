# deadlock-core

Shared vocabulary for the [`deadlock-rs`](https://github.com/deadlock-plus/deadlock-rs) crates:
`HeroId`, `Team`, `GameState`, `MatchMode`, `GameMode`, `AccountId`/`SteamId`, and the
`HeroNames` trait.

These describe the *game*, independent of where a value came from: a hero id means the
same thing read out of a live client's memory as fetched from an API. That lets
`deadlock-memory` and `deadlock-data` speak the same types without depending on each
other.

Dependency-free apart from optional `serde`.

```rust
use deadlock_core::{HeroId, HeroNames, Team};

assert!(Team::AMBER.is_playing());
assert_eq!(Team::AMBER.opponent(), Some(Team::SAPPHIRE));
assert!(!Team::SPECTATOR.is_playing());
```

Licence: LGPL-3.0-or-later. See [LICENSE.md](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE.md).

The LGPL is a copyleft licence. Modifications to this library must be released
under the same terms; an application that merely uses it need not be.

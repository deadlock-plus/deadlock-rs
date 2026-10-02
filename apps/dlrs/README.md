# dlrs

`dlrs` is a command-line inspector for a running Deadlock client.

```
dlrs status                   attach and report what resolved
dlrs entities [substr]        list live entities, optionally filtered
dlrs players                  scoreboard
dlrs objectives               objective timers, then walkers, guardians, patron, shrines
dlrs modifiers [substr]       buffs, debuffs and crowd control on each player
dlrs account                  your account object, stats and hero builds, from the Game Coordinator
dlrs timeline                 record kills, objectives and phases as a timeline
    --out <path>              append JSONL records to <path>; default is stdout only
    --for <secs>              stop after <secs> and print a summary; default is until ctrl-c
    --every <ms>              poll interval; default 100
    --quiet                   do not print records, only write them; needs --out
dlrs replay <path>            what is inside a local .dem: build, map and a command census
    --all                     scan the whole file instead of the first --cap MiB
    --cap <mib>               how much of the file to read; default 64
    --find <name>             also say whether the serializer tables contain <name>
dlrs party                    your Steam party, from the Game Coordinator
dlrs schema [substr]          list schema classes and field offsets
dlrs enums [substr]           list schema enums and their values
dlrs heroes [substr]          hero id -> name catalogue (works offline)
dlrs player [slot|hero]       full detail for one or all players
dlrs items [substr|kind]      item / ability / weapon names (works offline)
dlrs probe-schema             brute-force the schema layout offset
dlrs fields                   print the baked fallback offset table
dlrs regions                  committed RW regions (the heap search surface)
dlrs steam                    local Steam account id
dlrs watch                    live scoreboard, refreshed 10x a second
dlrs events                   stream kills, purchases and objectives as they happen
dlrs bench                    time a snapshot and report how often values change
```

That list is `commands::COMMANDS`, which is also what `dlrs help` prints and what `main`
dispatches on, flags included. `dlrs <command> --help` prints one command's block.

Read-only: every subcommand only observes the game process.

Not published to crates.io. It is an application over the `deadlock-*` crates, not part of
the library surface.

Licence: LGPL-3.0-or-later. See [LICENSE.md](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE.md).

The LGPL is a copyleft licence. Modifications to this library must be released
under the same terms; an application that merely uses it need not be.

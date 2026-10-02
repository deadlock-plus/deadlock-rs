//! One module per subcommand family.

mod account;
mod bench;
mod catalogs;
mod entities;
mod events;
#[cfg(feature = "metrics")]
mod metrics;
mod modifiers;
mod objectives;
mod party;
mod player_detail;
mod players;
#[cfg(feature = "replay")]
mod replay;
mod schema;
mod status;
mod system;
#[cfg(feature = "timeline")]
mod timeline;

pub use account::account;
pub use bench::bench;
pub use catalogs::{heroes, items};
pub use entities::entities;
pub use events::events;
#[cfg(feature = "metrics")]
pub use metrics::metrics;
pub use modifiers::modifiers;
pub use objectives::objectives;
pub use party::party;
pub use player_detail::player;
pub use players::{players, watch};
#[cfg(feature = "replay")]
pub use replay::replay;
pub use schema::{enums, fields, probe_schema, schema};
pub use status::status;
pub use system::{regions, steam};
#[cfg(feature = "timeline")]
pub use timeline::timeline;

use deadlock_reader::Reader;
use deadlock_reader::snapshot::LiveSnapshot;

/// One subcommand: how it is spelled, what it takes, and what to run.
///
/// The table below is both the dispatch and the help text. They were separate lists, and
/// they had already diverged: `party`, `events` and `bench` were all reachable from the
/// command line and none of them appeared in `dlrs help`, so three commands existed and
/// could only be found by reading the source.
pub struct Command {
    /// What the user types.
    pub name: &'static str,
    /// Argument hint for the help line; empty when the command takes none.
    pub arg: &'static str,
    /// One-line description.
    pub help: &'static str,
    /// The flags this command accepts, as `(spelling, description)`, or empty.
    ///
    /// Listed under the command in `dlrs help`, for the same reason the table exists at
    /// all: a flag that is accepted and undocumented can only be found by reading the
    /// source. Each command that has any owns its own list, and its own tests check that
    /// everything in it parses.
    pub flags: &'static [(&'static str, &'static str)],
    /// Every command is handed the arguments after its own name, including the ones that
    /// ignore them, so a single table can hold every shape.
    pub run: fn(&[&str]) -> i32,
}

/// Every subcommand, in the order `dlrs help` lists them.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "status",
        arg: "",
        help: "attach and report what resolved",
        flags: &[],
        run: |_| status(),
    },
    Command {
        name: "entities",
        arg: "[substr]",
        help: "list live entities, optionally filtered",
        flags: &[],
        run: |a| entities(a.first().copied()),
    },
    Command {
        name: "players",
        arg: "",
        help: "scoreboard",
        flags: &[],
        run: |_| players(),
    },
    Command {
        name: "objectives",
        arg: "",
        help: "objective timers, then walkers, guardians, patron, shrines",
        flags: &[],
        run: |_| objectives(),
    },
    Command {
        name: "modifiers",
        arg: "[substr]",
        help: "buffs, debuffs and crowd control on each player",
        flags: &[],
        run: |a| modifiers(a.first().copied()),
    },
    Command {
        name: "account",
        arg: "",
        help: "your account object, stats and hero builds, from the Game Coordinator",
        flags: &[],
        run: |_| account(),
    },
    #[cfg(feature = "timeline")]
    Command {
        name: "timeline",
        arg: "",
        help: "record kills, objectives and phases as a timeline",
        flags: timeline::FLAGS,
        run: timeline,
    },
    #[cfg(feature = "metrics")]
    Command {
        name: "metrics",
        arg: "",
        help: "record the cross-tick accumulator totals over a match",
        flags: metrics::FLAGS,
        run: metrics,
    },
    #[cfg(feature = "replay")]
    Command {
        name: "replay",
        arg: "<path>",
        help: "what is inside a local .dem: build, map and a command census",
        flags: replay::FLAGS,
        run: replay,
    },
    Command {
        name: "party",
        arg: "",
        help: "your Steam party, from the Game Coordinator",
        flags: &[],
        run: |_| party(),
    },
    Command {
        name: "schema",
        arg: "[substr]",
        help: "list schema classes and field offsets",
        flags: &[],
        run: |a| schema(a.first().copied()),
    },
    Command {
        name: "enums",
        arg: "[substr]",
        help: "list schema enums and their values",
        flags: &[],
        run: |a| enums(a.first().copied()),
    },
    Command {
        name: "heroes",
        arg: "[substr]",
        help: "hero id -> name catalogue (works offline)",
        flags: &[],
        run: |a| heroes(a.first().copied()),
    },
    Command {
        name: "player",
        arg: "[slot|hero]",
        help: "full detail for one or all players",
        flags: &[],
        run: |a| player(a.first().copied()),
    },
    Command {
        name: "items",
        arg: "[substr|kind]",
        help: "item / ability / weapon names (works offline)",
        flags: &[],
        run: |a| items(a.first().copied()),
    },
    Command {
        name: "probe-schema",
        arg: "",
        help: "brute-force the schema layout offset",
        flags: &[],
        run: |_| probe_schema(),
    },
    Command {
        name: "fields",
        arg: "",
        help: "print the baked fallback offset table",
        flags: &[],
        run: |_| fields(),
    },
    Command {
        name: "regions",
        arg: "",
        help: "committed RW regions (channel 2 scan surface)",
        flags: &[],
        run: |_| regions(),
    },
    Command {
        name: "steam",
        arg: "",
        help: "local Steam account id",
        flags: &[],
        run: |_| steam(),
    },
    Command {
        name: "watch",
        arg: "",
        help: "live scoreboard, refreshed 10x a second",
        flags: &[],
        run: |_| watch(),
    },
    Command {
        name: "events",
        arg: "",
        help: "stream kills, purchases and objectives as they happen",
        flags: &[],
        run: |_| events(),
    },
    Command {
        name: "bench",
        arg: "",
        help: "time a snapshot and report how often values change",
        flags: &[],
        run: |_| bench(),
    },
];

/// The command spelled `name`, if there is one.
pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

pub fn usage() {
    outln!("dlrs - read-only Deadlock match reader\n\nUSAGE:");
    for c in COMMANDS {
        usage_of(c);
    }
    outln!("");
}

/// One command's line, with its flags underneath it.
///
/// The flag column is indented to land under the description column, so a command with
/// flags reads as one block rather than as a second table.
pub fn usage_of(c: &Command) {
    let invocation = if c.arg.is_empty() {
        c.name.to_string()
    } else {
        format!("{} {}", c.name, c.arg)
    };
    outln!("  dlrs {invocation:<24} {}", c.help);
    for (spelling, help) in c.flags {
        outln!("      {spelling:<25} {help}");
    }
}

/// Attach to the game, reporting the failure on stderr.
pub(crate) fn attach() -> Option<Reader> {
    match Reader::attach() {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("attach failed: {e}");
            None
        }
    }
}

/// Attach, take one snapshot, and hand it to `f`. Returns a process exit code.
///
/// The three outcomes that are not "here is a snapshot" - no game, no match, a failed
/// read - are the same for every command, and were written out at each of them. One of
/// those copies had drifted: `dlrs player` matched on `Ok(Some(_))` and let its `else`
/// branch report a failed read as "not in a match", so a permissions problem or a
/// half-unmapped page looked like an idle client.
pub(crate) fn with_snapshot(f: impl FnOnce(&Reader, &LiveSnapshot) -> i32) -> i32 {
    let Some(r) = attach() else { return 1 };
    match r.live_snapshot() {
        Ok(Some(s)) => f(&r, &s),
        Ok(None) => {
            outln!("not in a match");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the dispatch *and* the help text, so the two cannot disagree - which
    /// they had: `party`, `events` and `bench` were reachable and undocumented. These
    /// guard the table's own invariants, which is what is left to get wrong.
    #[test]
    fn every_command_is_spelled_once_and_described() {
        let mut seen = std::collections::BTreeSet::new();
        for c in COMMANDS {
            assert!(!c.name.is_empty(), "a command with no name is unreachable");
            assert!(!c.help.is_empty(), "{} has no description", c.name);
            assert!(
                seen.insert(c.name),
                "{} is listed twice; the second is dead, since lookup takes the first",
                c.name
            );
            assert!(
                !c.name.starts_with('-'),
                "{} would be mistaken for a flag",
                c.name
            );
        }
        assert_eq!(seen.len(), COMMANDS.len());
    }

    #[test]
    fn lookup_finds_every_listed_command_and_nothing_else() {
        for c in COMMANDS {
            let found = lookup(c.name).unwrap_or_else(|| panic!("{} not found", c.name));
            assert_eq!(found.name, c.name);
        }
        assert!(lookup("nope").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("help").is_none());
    }

    /// Named individually because these three are the finding: each dispatched, none
    /// appeared in the help.
    #[test]
    fn the_three_previously_undiscoverable_commands_are_listed() {
        for name in ["party", "events", "bench"] {
            assert!(lookup(name).is_some(), "{name} is dispatched but unlisted");
        }
    }

    /// A command taking an argument has to say so in the help, or the argument may as well
    /// not exist. The reverse matters less, but a hint on a command that ignores its
    /// argument is a promise the code does not keep.
    /// `[optional]` and `<required>` are the two shapes a hint may take; anything else
    /// leaves its reader guessing which it is.
    #[test]
    fn argument_hints_say_whether_they_are_required() {
        for c in COMMANDS {
            if c.arg.is_empty() {
                continue;
            }
            let optional = c.arg.starts_with('[') && c.arg.ends_with(']');
            let required = c.arg.starts_with('<') && c.arg.ends_with('>');
            assert!(
                optional || required,
                "{}'s hint {:?} says neither [optional] nor <required>",
                c.name,
                c.arg
            );
        }
    }

    /// A flag with no description is as undiscoverable as one that is not listed at all,
    /// and one not spelled with dashes would be read as a positional argument.
    #[test]
    fn every_listed_flag_is_spelled_and_described() {
        for c in COMMANDS {
            let mut seen = std::collections::BTreeSet::new();
            for (spelling, help) in c.flags {
                assert!(
                    spelling.starts_with("--"),
                    "{}'s flag {spelling:?} is not a flag",
                    c.name
                );
                assert!(
                    !help.is_empty(),
                    "{}'s {spelling} has no description",
                    c.name
                );
                let name = spelling.split_whitespace().next().unwrap();
                assert!(seen.insert(name), "{} lists {name} twice", c.name);
            }
        }
    }

    /// The two feature-gated commands, named individually for the same reason the three
    /// above are: the table's whole job is that a command cannot be reachable and
    /// undocumented, and these are the two that also have flags to be undocumented.
    #[cfg(all(feature = "timeline", feature = "replay"))]
    #[test]
    fn timeline_and_replay_are_listed_with_their_flags() {
        for name in ["timeline", "replay"] {
            let c = lookup(name).unwrap_or_else(|| panic!("{name} is not listed"));
            assert!(!c.flags.is_empty(), "{name} parses flags but lists none");
        }
    }
}

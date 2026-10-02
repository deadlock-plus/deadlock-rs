//! `dlrs`: inspect a running Deadlock client.
//!
//! Read-only. Every subcommand only observes the game process.

#[cfg(any(windows, target_os = "linux"))]
#[macro_use]
mod output;
#[cfg(any(windows, target_os = "linux"))]
mod catalog;
#[cfg(any(windows, target_os = "linux"))]
mod commands;

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("dlrs supports Windows and Linux (including Proton).");
    std::process::exit(2);
}

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = argv.first().copied().unwrap_or("status");
    // Everything after the command's own name. Commands that take nothing are handed it
    // too and ignore it, which is what lets one table hold every shape.
    let rest = argv.get(1..).unwrap_or(&[]);

    // Dispatch straight from the table `usage` prints, so a command cannot be reachable
    // and undocumented. Three of them were.
    let rc = match cmd {
        "help" | "-h" | "--help" => {
            commands::usage();
            0
        }
        name => match commands::lookup(name) {
            // `dlrs <command> --help` prints that command's line and its flags. Handled
            // here rather than in each parser so every command answers it, including the
            // ones with no flags to describe.
            Some(c) if matches!(rest.first(), Some(&"--help" | &"-h")) => {
                commands::usage_of(c);
                0
            }
            Some(c) => (c.run)(rest),
            None => {
                eprintln!("unknown command: {name}\n");
                commands::usage();
                2
            }
        },
    };
    std::process::exit(rc);
}

//! `udoc mangen` hidden subcommand.
//!
//! Emits a roff-formatted man page for `udoc(1)` on stdout. Consumers
//! redirect the output into a location `man` searches (e.g.
//! `/usr/local/share/man/man1/udoc.1`) or pass it through `man -l -`
//! for one-shot rendering.
//!
//! Like the `completions` subcommand, this is `#[command(hide = true)]`
//! at the clap layer so it does not clutter the top-level `--help`
//! output. It is still reachable via `udoc mangen --help`.

use std::io::Write;

use clap::CommandFactory;
use clap_mangen::Man;

/// Run the man-page generator. Writes roff to stdout. Returns an exit
/// code (always 0 today; kept as a `u8` for parity with the other
/// subcommand runners).
pub fn run<C: CommandFactory>() -> u8 {
    let cmd = C::command();
    let man = Man::new(cmd);
    let mut buffer: Vec<u8> = Vec::new();
    if man.render(&mut buffer).is_err() {
        return 1;
    }
    if std::io::stdout().write_all(&buffer).is_err() {
        return 1;
    }
    0
}

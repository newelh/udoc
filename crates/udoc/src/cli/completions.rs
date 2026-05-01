//! `udoc completions <shell>` hidden subcommand.
//!
//! Emits a shell completion script on stdout. Consumers redirect the
//! output into a location their shell sources on startup. Supported
//! shells are whatever `clap_complete::Shell` supports today: bash,
//! zsh, fish, elvish, powershell.
//!
//! The subcommand is `#[command(hide = true)]` at the clap layer so it
//! doesn't clutter the top-level `--help` output; it is still reachable
//! via `udoc completions --help`.

use clap::CommandFactory;
use clap_complete::{generate, Shell};

/// Run the completion generator. Writes to stdout. Returns an exit
/// code (always 0 today; kept as a `u8` for parity with the other
/// subcommand runners).
pub fn run<C: CommandFactory>(shell: Shell) -> u8 {
    let mut cmd = C::command();
    let bin_name = cmd.get_name().to_string();
    generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
    0
}

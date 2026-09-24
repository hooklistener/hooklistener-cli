//! `completions` command: shell completion scripts.

use clap::{CommandFactory, ValueEnum};
use std::io;

use crate::cli::Cli;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell", alias = "power-shell")]
    PowerShell,
    Elvish,
}

/// Generate the completion script for `shell` into `out`.
///
/// The script is rendered into memory first so that a single `write_all`
/// carries it to `out`; a closed pipe then surfaces as one `io::Error`
/// instead of a panic inside `clap_complete`.
pub fn write_completions(shell: CompletionShell, out: &mut dyn io::Write) -> io::Result<()> {
    use clap_complete::generate;
    use clap_complete::shells::{Bash, Elvish, Fish, PowerShell, Zsh};

    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();
    let mut buf: Vec<u8> = Vec::new();

    match shell {
        CompletionShell::Bash => generate(Bash, &mut command, bin_name, &mut buf),
        CompletionShell::Zsh => generate(Zsh, &mut command, bin_name, &mut buf),
        CompletionShell::Fish => generate(Fish, &mut command, bin_name, &mut buf),
        CompletionShell::PowerShell => generate(PowerShell, &mut command, bin_name, &mut buf),
        CompletionShell::Elvish => generate(Elvish, &mut command, bin_name, &mut buf),
    }

    out.write_all(&buf).and_then(|_| out.flush())
}

/// Write the completion script for `shell` to `out`, treating a closed pipe
/// (for example `completions bash | head -1`) as success.
pub fn print_completions(shell: CompletionShell, out: &mut dyn io::Write) -> io::Result<()> {
    match write_completions(shell, out) {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

use anyhow::Result;
use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs};

pub fn run(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    clap_complete::generate(args.shell, &mut cmd, bin_name, &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::Shell;

    fn render(shell: Shell) -> String {
        let mut cmd = Cli::command();
        let mut buf: Vec<u8> = Vec::new();
        clap_complete::generate(shell, &mut cmd, "clever-project", &mut buf);
        String::from_utf8(buf).expect("completion output must be valid UTF-8")
    }

    #[test]
    fn every_shell_emits_non_empty_script() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            let out = render(shell);
            assert!(
                out.len() > 100,
                "completion script for {shell:?} is suspiciously short ({} bytes)",
                out.len()
            );
        }
    }

    #[test]
    fn script_mentions_every_subcommand() {
        let bash = render(Shell::Bash);
        for sub in [
            "read",
            "apply",
            "delete",
            "check",
            "status",
            "init",
            "unlock",
            "completions",
        ] {
            assert!(
                bash.contains(sub),
                "bash completion script missing subcommand `{sub}`"
            );
        }
    }
}

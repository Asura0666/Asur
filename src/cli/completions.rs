use anyhow::Result;
use clap::{Args, CommandFactory};
use clap_complete::{Shell, generate};

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: Shell,
}

pub fn run(args: CompletionsArgs) -> Result<()> {
    let mut cmd = super::Cli::command();
    generate(args.shell, &mut cmd, "asur", &mut std::io::stdout());
    Ok(())
}

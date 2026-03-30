use std::path::PathBuf;

use clap::{Parser, Subcommand};
use egopulse::config::Config;
use egopulse::error::EgoPulseError;
use egopulse::logging::init_logging;
use egopulse::runtime;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(name = "egopulse", version = VERSION, about = "EgoPulse runtime foundation")]
struct Cli {
    /// Explicit config file path (absolute or relative)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send a single prompt to the configured OpenAI-compatible endpoint.
    Ask { prompt: String },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    init_logging(&config.log_level)?;

    match cli.command {
        Command::Ask { prompt } => match runtime::ask(config, &prompt).await {
            Ok(response) => {
                println!("assistant: {response}");
                Ok(())
            }
            Err(EgoPulseError::ShutdownRequested) => Ok(()),
            Err(error) => Err(error),
        },
    }
}

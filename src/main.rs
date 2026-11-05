//! eco: dictate on your phone, transcribe on your computer.
//!
//! An OpenAI-compatible speech-to-text server (`POST /v1/audio/transcriptions`)
//! running a local transcribe.cpp model, plus the commands that set it up.

mod config;
mod init;
mod network;
mod pair;
mod server;
mod service;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Dictate on your phone, transcribe on your computer.")]
#[command(after_help = "Get started:\n  eco init\n  eco service install\n  eco pair")]
struct Cli {
    /// Config file [default: $XDG_CONFIG_HOME/eco/config.toml or ~/.config/eco/config.toml]
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a config: a new API key, a model, and your home network
    Init {
        /// Replace an existing config, including its API key
        #[arg(long)]
        force: bool,
        /// Answer other devices on every network, not only at home
        #[arg(long)]
        no_home_check: bool,
        /// Use this GGUF model instead of finding or downloading one
        #[arg(long, value_name = "PATH")]
        model: Option<PathBuf>,
    },
    /// Show the endpoint and API key for your phone, with QR codes
    Pair {
        /// Print the values without QR codes
        #[arg(long)]
        no_qr: bool,
    },
    /// Run eco in the background and start it at login
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Run the server in the foreground
    Serve,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install and start the service, or repair and restart it
    Install,
    /// Stop and remove the service
    Uninstall,
    /// Restart the service, e.g. after editing the config
    Restart,
    /// Show whether eco is running and answering
    Status,
}

fn main() {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(config::default_path);
    let result = match cli.command {
        Command::Init { force, no_home_check, model } => {
            init::run(&config_path, init::Options { force, no_home_check, model })
        }
        Command::Pair { no_qr } => pair::run(&config_path, !no_qr),
        Command::Service { action: ServiceAction::Install } => service::install(&config_path),
        Command::Service { action: ServiceAction::Uninstall } => service::uninstall(),
        Command::Service { action: ServiceAction::Restart } => service::restart(),
        Command::Service { action: ServiceAction::Status } => std::process::exit(service::status(&config_path)),
        Command::Serve => config::Config::load(&config_path).and_then(server::run),
    };
    if let Err(message) = result {
        eprintln!("eco: {message}");
        std::process::exit(1);
    }
}

/// Prints one `label  value` line, aligned the same way in every command.
fn print_row(label: &str, value: impl std::fmt::Display) {
    println!("  {label:<14}{value}");
}

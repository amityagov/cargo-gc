use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-gc",
    version,
    about = "Manage and clean Rust project target directories"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Recursively register projects (depth counts directories below PATH).
    Scan {
        path: Option<PathBuf>,
        #[arg(long)]
        depth: Option<usize>,
    },
    /// Register a Cargo project or its workspace root.
    Add { path: Option<PathBuf> },
    /// List registered projects and local target sizes.
    List,
    /// Remove safe, standard local target directories.
    Clean {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Unregister a project without deleting its files.
    Remove { path: Option<PathBuf> },
}

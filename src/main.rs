mod cleanup;
mod cli;
mod projects;
mod registry;
mod sizes;

use clap::Parser;
use cli::{Cli, Command};
use registry::RegistryStore;
use std::io;
use std::path::PathBuf;

fn main() {
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "gc") {
        args.remove(1);
    }
    let cli = Cli::parse_from(args);
    if let Err(error) = run(cli.command) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> io::Result<()> {
    let store = RegistryStore::new()?;
    match command {
        Command::Scan { path, depth } => {
            let root = std::fs::canonicalize(path.unwrap_or_else(|| PathBuf::from(".")))?;
            println!("Scanning {}...", root.display());
            let projects = projects::discover(&root, depth)?;
            let mut added = 0;
            let mut skipped = 0;
            store.update(|registry| {
                for project in projects {
                    if registry.insert(&project)? {
                        println!("+ {}", project.display());
                        added += 1;
                    } else {
                        println!("= {} (already registered)", project.display());
                        skipped += 1;
                    }
                }
                Ok(())
            })?;
            println!("\nAdded: {added}\nSkipped: {skipped}");
            Ok(())
        }
        Command::Add { path } => {
            let project = projects::canonical_project(&path.unwrap_or_else(|| PathBuf::from(".")))?;
            let added = store.update(|registry| registry.insert(&project))?;
            if added {
                println!("+ {}\nAdded: 1", project.display());
            } else {
                println!("= {} (already registered)", project.display());
            }
            Ok(())
        }
        Command::List => sizes::list(&store.read()?),
        Command::Clean { dry_run, yes } => cleanup::clean(&store.read()?, dry_run, yes),
        Command::Remove { path } => {
            let path = path.unwrap_or_else(|| PathBuf::from("."));
            let requested = projects::normalize_path(&path)?;
            let removed = store.update(|registry| {
                if registry.remove(&requested) {
                    return Ok(true);
                }
                if let Ok(root) = projects::canonical_project(&path) {
                    return Ok(registry.remove(&root));
                }
                Ok(false)
            })?;
            println!(
                "{} {}",
                if removed {
                    "Removed"
                } else {
                    "Not registered:"
                },
                requested.display()
            );
            Ok(())
        }
    }
}

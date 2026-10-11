use crate::projects::canonical_project;
use crate::registry::Registry;
use crate::sizes::{human_size, size_of_open_dir};
use cap_fs_ext::DirExt;
#[cfg(windows)]
use cap_std::fs::FileTypeExt;
use cap_std::{ambient_authority, fs::Dir};
use same_file::Handle;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

struct Plan {
    root: PathBuf,
    directory: Dir,
    size: u64,
}

fn target_config_is_nonstandard(root: &Path) -> io::Result<bool> {
    for name in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ] {
        if std::env::var_os(name).is_some() {
            return Ok(true);
        }
    }
    let mut directories = BTreeSet::new();
    for start in [root.to_path_buf(), std::env::current_dir()?] {
        for ancestor in start.ancestors() {
            directories.insert(ancestor.join(".cargo"));
        }
    }
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cargo")))
    {
        directories.insert(home);
    }
    for directory in directories {
        for name in ["config", "config.toml"] {
            if config_is_nonstandard(&directory.join(name))? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn config_is_nonstandard(path: &Path) -> io::Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("cannot inspect {}: {error}", path.display()),
            ));
        }
    };
    let value: toml::Value = toml::from_str(&text).map_err(|error| {
        io::Error::other(format!("cannot validate {}: {error}", path.display()))
    })?;
    // Config includes may refer to other files. Fail closed for this MVP.
    Ok(value.get("include").is_some()
        || value.get("build").is_some_and(|build| {
            build.get("target-dir").is_some() || build.get("build-dir").is_some()
        }))
}

fn identity(directory: &Dir) -> io::Result<Handle> {
    Handle::from_file(directory.try_clone()?.into_std_file())
}

fn prepare(root: &Path) -> io::Result<Option<Plan>> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(io::Error::other("project is not a real directory"));
        }
        Ok(_) => {}
    }
    if !root.is_absolute() || fs::canonicalize(root)? != root {
        return Err(io::Error::other("project path changed or is not canonical"));
    }
    if root.parent().is_none()
        || dirs::home_dir()
            .and_then(|home| fs::canonicalize(home).ok())
            .is_some_and(|home| home == root)
    {
        return Err(io::Error::other(
            "refusing to clean a filesystem or home root",
        ));
    }
    let directory = Dir::open_ambient_dir(root, ambient_authority())?;
    if identity(&directory)? != Handle::from_path(root)? || fs::canonicalize(root)? != root {
        return Err(io::Error::other("project changed while opening it"));
    }
    validate_project(root, &directory)?;
    let metadata = match directory.symlink_metadata("target") {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("target is not a real directory"));
    }
    let size = size_of_open_dir(&directory.open_dir_nofollow("target")?)?;
    Ok(Some(Plan {
        root: root.into(),
        directory,
        size,
    }))
}

fn validate_project(root: &Path, directory: &Dir) -> io::Result<()> {
    if identity(directory)? != Handle::from_path(root)? || fs::canonicalize(root)? != root {
        return Err(io::Error::other("project path changed"));
    }
    let manifest = directory.symlink_metadata("Cargo.toml")?;
    if !manifest.is_file() || manifest.file_type().is_symlink() {
        return Err(io::Error::other("project manifest is not a real file"));
    }
    if canonical_project(root)? != root {
        return Err(io::Error::other(
            "registered path is no longer the Cargo workspace root",
        ));
    }
    if target_config_is_nonstandard(root)? {
        return Err(io::Error::other(
            "non-standard target configuration; skipped",
        ));
    }
    Ok(())
}

fn execute(plan: &Plan) -> io::Result<()> {
    validate_project(&plan.root, &plan.directory)?;
    let metadata = plan.directory.symlink_metadata("target")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("target changed or is a symlink"));
    }
    // Resolve only the fixed child of the retained project handle. Capability
    // traversal and removal cannot escape it if path components are swapped.
    remove_target(&plan.directory)
}

#[cfg(not(windows))]
fn remove_target(directory: &Dir) -> io::Result<()> {
    directory.remove_dir_all("target")
}

// cap-std's Windows remove_dir_all releases the target handle before recursively
// deleting by its absolute path. Keep directories pinned while traversing;
// after releasing a child, remove only that single entry, never traverse it again.
#[cfg(windows)]
fn remove_target(directory: &Dir) -> io::Result<()> {
    remove_child(directory, Path::new("target"))
}

#[cfg(windows)]
fn remove_child(parent: &Dir, name: &Path) -> io::Result<()> {
    let directory = parent.open_dir_nofollow(name)?;
    for entry in directory.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = directory.symlink_metadata(&name)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            remove_child(&directory, Path::new(&name))?;
        } else if metadata.file_type().is_symlink_dir() {
            directory.remove_dir(&name)?;
        } else {
            directory.remove_file(&name)?;
        }
    }
    drop(directory);
    parent.remove_dir(name)
}

fn confirm(input: &mut impl BufRead) -> io::Result<bool> {
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub fn clean(registry: &Registry, dry_run: bool, yes: bool) -> io::Result<()> {
    let mut plans = Vec::new();
    let mut total = 0u64;
    let mut errors = 0;
    println!("Cleanup plan:");
    for project in &registry.projects {
        let root = Path::new(&project.path);
        match prepare(root) {
            Ok(Some(plan)) => {
                total = total
                    .checked_add(plan.size)
                    .ok_or_else(|| io::Error::other("total size overflow"))?;
                println!("  {}  {}", root.display(), human_size(plan.size));
                plans.push(plan);
            }
            Ok(None) => println!("  {}  0 B (project or target missing)", root.display()),
            Err(error) => {
                errors += 1;
                eprintln!("warning: {}: {error}", root.display());
            }
        }
    }
    println!("Total: {}", human_size(total));
    if dry_run || plans.is_empty() {
        println!("Cleaned: 0, errors: {errors}");
        return result(errors);
    }
    if !yes {
        if !io::stdin().is_terminal() {
            return Err(io::Error::other(
                "refusing to clean without --yes in a non-interactive environment",
            ));
        }
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        if !confirm(&mut io::stdin().lock())? {
            println!("Cancelled.\nCleaned: 0, errors: {errors}");
            return result(errors);
        }
    }
    let mut cleaned = 0;
    for plan in plans {
        match execute(&plan) {
            Ok(()) => {
                cleaned += 1;
                println!("cleaned {}", plan.root.display());
            }
            Err(error) => {
                errors += 1;
                eprintln!("error: {}: {error}", plan.root.display());
            }
        }
    }
    println!("Cleaned: {cleaned}, errors: {errors}");
    result(errors)
}

fn result(errors: usize) -> io::Result<()> {
    if errors > 0 {
        Err(io::Error::other("some projects could not be cleaned"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::tests::fixture;

    #[test]
    fn confirmation_and_cancellation() {
        for answer in ["", "\n", "n\n", "no\n"] {
            assert!(!confirm(&mut answer.as_bytes()).unwrap());
        }
        for answer in ["y\n", "YES\n"] {
            assert!(confirm(&mut answer.as_bytes()).unwrap());
        }
    }

    #[test]
    fn invalid_config_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let config = temporary.path().join("config.toml");
        assert!(!config_is_nonstandard(&config).unwrap());
        fs::write(&config, "[build]\ntarget-dir='elsewhere'").unwrap();
        assert!(config_is_nonstandard(&config).unwrap());
        fs::write(&config, "[broken").unwrap();
        assert!(config_is_nonstandard(&config).is_err());
    }

    #[test]
    fn filesystem_root_is_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(temporary.path()).unwrap();
        let root = canonical.ancestors().last().unwrap();
        assert!(prepare(root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn target_swap_is_rejected_and_nested_links_are_not_followed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        fixture(&root, "project");
        fs::create_dir(root.join("target")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), "keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("target/link")).unwrap();
        let plan = prepare(&root).unwrap().unwrap();
        fs::rename(root.join("target"), root.join("old-target")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("target")).unwrap();
        assert!(execute(&plan).is_err());
        assert!(outside.path().join("keep").exists());
        fs::remove_file(root.join("target")).unwrap();
        fs::rename(root.join("old-target"), root.join("target")).unwrap();
        execute(&prepare(&root).unwrap().unwrap()).unwrap();
        assert!(!root.join("target").exists());
        assert!(outside.path().join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn project_path_swapped_after_planning_cannot_delete_outside() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap().join("project");
        fixture(&root, "project");
        fs::create_dir(root.join("target")).unwrap();
        let plan = prepare(&root).unwrap().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("target")).unwrap();
        fs::write(outside.path().join("target/keep"), "keep").unwrap();
        fs::rename(&root, root.with_file_name("moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        assert!(execute(&plan).is_err());
        assert!(outside.path().join("target/keep").exists());
    }
}

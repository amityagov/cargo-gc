use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

pub fn canonical_project(path: &Path) -> io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    let manifest = if path.is_dir() {
        path.join("Cargo.toml")
    } else {
        path
    };
    if manifest.file_name().is_none_or(|name| name != "Cargo.toml") || !manifest.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a Cargo project", manifest.display()),
        ));
    }
    // Cargo validates workspace membership, exclusions and package.workspace
    // without fetching dependencies or creating a lockfile.
    let output = Command::new("cargo")
        .args([
            "locate-project",
            "--workspace",
            "--message-format",
            "plain",
            "--color",
            "never",
            "--manifest-path",
        ])
        .arg(&manifest)
        .current_dir(manifest.parent().unwrap())
        .output()
        .map_err(|error| io::Error::new(error.kind(), format!("cannot run Cargo: {error}")))?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid Cargo project {}: {}",
                manifest.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let root_manifest = String::from_utf8(output.stdout)
        .map_err(|_| io::Error::other("Cargo returned a non-UTF-8 path"))?;
    let root_manifest = Path::new(root_manifest.trim_end_matches(['\r', '\n']));
    fs::canonicalize(
        root_manifest
            .parent()
            .ok_or_else(|| io::Error::other("Cargo returned an invalid manifest path"))?,
    )
}

pub fn discover(root: &Path, depth: Option<usize>) -> io::Result<BTreeSet<PathBuf>> {
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scan path is not a directory",
        ));
    }
    let mut projects = BTreeSet::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .max_depth(depth.map_or(usize::MAX, |depth| depth.saturating_add(1)))
        .into_iter()
        .filter_entry(|entry| {
            !entry.file_type().is_dir()
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "node_modules")
                )
        })
    {
        match entry {
            Ok(entry) if entry.file_type().is_file() && entry.file_name() == "Cargo.toml" => {
                match canonical_project(entry.path()) {
                    Ok(project) => {
                        projects.insert(project);
                    }
                    // Manifests without build targets or a usable workspace are
                    // not independently usable projects. Quietly skip them during
                    // discovery while keeping explicit add strict.
                    Err(error) if is_nonproject_manifest(&error) => {}
                    Err(error) => eprintln!("warning: {error}"),
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("warning: cannot scan: {error}"),
        }
    }
    Ok(projects)
}

fn is_nonproject_manifest(error: &io::Error) -> bool {
    // locate-project has no structured error output; match only these specific
    // Cargo diagnostics so unrelated manifest and filesystem errors stay visible.
    if error.kind() != io::ErrorKind::InvalidData {
        return false;
    }
    let diagnostic = error.to_string();
    diagnostic.contains("current package believes it's in a workspace when it's not")
        || diagnostic
            .lines()
            .any(|line| line.trim() == "no targets specified in the manifest")
        || (diagnostic.contains("error inheriting ")
            && diagnostic
                .lines()
                .any(|line| line.trim() == "failed to find a workspace root"))
        || (diagnostic.contains("the cargo feature `")
            && diagnostic.contains("requires a nightly version of Cargo"))
}

pub fn normalize_path(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => return Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(name) => {
                normalized.push(name);
                // Resolve existing components before a later `..` can remove
                // them. Missing components still support lexical normalization.
                match fs::canonicalize(&normalized) {
                    Ok(canonical) => normalized = canonical,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn fixture(path: &Path, name: &str) {
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("src/lib.rs"), "").unwrap();
        fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n"),
        )
        .unwrap();
    }

    #[test]
    fn registration_and_invalid_manifests() {
        let temporary = tempfile::tempdir().unwrap();
        fixture(temporary.path(), "ordinary");
        assert_eq!(
            canonical_project(temporary.path()).unwrap(),
            fs::canonicalize(temporary.path()).unwrap()
        );
        for text in ["", "invalid = [", "[package]\nname=123"] {
            fs::write(temporary.path().join("Cargo.toml"), text).unwrap();
            assert!(canonical_project(temporary.path()).is_err(), "{text}");
        }
        assert!(canonical_project(&temporary.path().join("missing")).is_err());
    }

    #[test]
    fn workspace_members_and_exclusions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        fixture(&root.join("member"), "member");
        fixture(&root.join("excluded"), "excluded");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=['member']\nexclude=['excluded']\nresolver='3'\n",
        )
        .unwrap();
        assert_eq!(canonical_project(&root.join("member")).unwrap(), root);
        assert_eq!(
            canonical_project(&root.join("excluded")).unwrap(),
            root.join("excluded")
        );
        fixture(&root.join("other"), "other");
        fs::write(
            root.join("other/Cargo.toml"),
            "[package]\nname='other'\nversion='0.1.0'\nworkspace='../workspace'\n",
        )
        .unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        fs::write(
            root.join("workspace/Cargo.toml"),
            "[workspace]\nmembers=['../other']\n",
        )
        .unwrap();
        assert_eq!(
            canonical_project(&root.join("other")).unwrap(),
            root.join("workspace")
        );
    }

    #[test]
    fn scan_depth_ignored_directories_and_invalid_project() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        fixture(&root.join("one"), "one");
        fixture(&root.join("nested/two"), "two");
        for ignored in ["target", ".git", "node_modules"] {
            fixture(&root.join(ignored).join("ignored"), "ignored");
        }
        fs::create_dir(root.join("broken")).unwrap();
        fs::write(root.join("broken/Cargo.toml"), "[broken").unwrap();
        assert!(discover(&root, Some(0)).unwrap().is_empty());
        assert_eq!(discover(&root, Some(1)).unwrap().len(), 1);
        assert_eq!(discover(&root, None).unwrap().len(), 2);
        assert_eq!(discover(&root, Some(usize::MAX)).unwrap().len(), 2);
    }

    #[test]
    fn missing_path_is_normalized() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        assert_eq!(
            normalize_path(&temporary.path().join("gone/../missing")).unwrap(),
            root.join("missing")
        );
    }
}

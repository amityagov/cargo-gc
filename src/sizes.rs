use crate::registry::Registry;
use cap_fs_ext::DirExt;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use std::fs;
use std::io;
use std::path::Path;

pub fn directory_size(path: &Path) -> io::Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("target is not a real directory"));
    }
    // Do not follow the final component even if swapped after the metadata check.
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("target has no name"))?;
    let parent = Dir::open_ambient_dir(parent, ambient_authority())?;
    size_of_open_dir(&parent.open_dir_nofollow(name)?)
}

pub fn size_of_open_dir(directory: &Dir) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in directory.entries()? {
        let entry = entry?;
        let metadata = directory.symlink_metadata(entry.file_name())?;
        if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| io::Error::other("target size overflow"))?;
        } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
            total = total
                .checked_add(size_of_open_dir(
                    &directory.open_dir_nofollow(entry.file_name())?,
                )?)
                .ok_or_else(|| io::Error::other("target size overflow"))?;
        }
    }
    Ok(total)
}

pub fn human_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn list(registry: &Registry) -> io::Result<()> {
    println!("{:<20} {:>12}  PATH", "PROJECT", "TARGET SIZE");
    let mut total = 0u64;
    let mut errors = 0;
    for project in &registry.projects {
        let path = Path::new(&project.path);
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("?");
        let size = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                match fs::canonicalize(path) {
                    Ok(canonical) if canonical == path => directory_size(&path.join("target")),
                    Ok(_) => Err(io::Error::other(
                        "project path changed or contains a symlink",
                    )),
                    Err(error) => Err(error),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                println!("{name:<20} {:>12}  {} (missing)", "missing", path.display());
                continue;
            }
            Ok(_) => Err(io::Error::other("project is not a directory")),
            Err(error) => Err(error),
        };
        match size {
            Ok(size) => {
                total = total
                    .checked_add(size)
                    .ok_or_else(|| io::Error::other("total size overflow"))?;
                println!("{name:<20} {:>12}  {}", human_size(size), path.display());
            }
            Err(error) => {
                errors += 1;
                println!("{name:<20} {:>12}  {} ({error})", "error", path.display());
            }
        }
    }
    println!(
        "\n{} projects, {} {}",
        registry.projects.len(),
        human_size(total),
        if errors == 0 {
            "total"
        } else {
            "known total (incomplete)"
        }
    );
    if errors > 0 {
        Err(io::Error::other(
            "some project sizes could not be determined",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_missing_nested_and_non_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        assert_eq!(directory_size(&target).unwrap(), 0);
        fs::create_dir_all(target.join("nested")).unwrap();
        fs::write(target.join("one"), [0; 1024]).unwrap();
        fs::write(target.join("nested/two"), [0; 1024]).unwrap();
        assert_eq!(directory_size(&target).unwrap(), 2048);
        assert_eq!(human_size(2048), "2.0 KiB");
        assert!(directory_size(&target.join("one")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_counted() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("data"), [0; 2048]).unwrap();
        std::os::unix::fs::symlink(outside.path(), temporary.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("data"),
            temporary.path().join("file-link"),
        )
        .unwrap();
        assert_eq!(directory_size(temporary.path()).unwrap(), 0);
        assert!(directory_size(&temporary.path().join("link")).is_err());
        std::os::unix::fs::symlink(
            outside.path().join("missing"),
            temporary.path().join("broken"),
        )
        .unwrap();
        assert!(directory_size(&temporary.path().join("broken")).is_err());
    }
}

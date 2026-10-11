use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<Project>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub path: String,
}

impl Registry {
    pub fn insert(&mut self, path: &Path) -> io::Result<bool> {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "project path is not valid UTF-8",
            )
        })?;
        if self.projects.iter().any(|project| project.path == path) {
            return Ok(false);
        }
        self.projects.push(Project { path: path.into() });
        Ok(true)
    }

    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.projects.len();
        self.projects
            .retain(|project| Path::new(&project.path) != path);
        self.projects.len() != before
    }
}

pub struct RegistryStore {
    pub path: PathBuf,
}

impl RegistryStore {
    pub fn new() -> io::Result<Self> {
        let base = if cfg!(target_os = "linux") {
            std::env::var_os("XDG_STATE_HOME")
                .filter(|value| !value.is_empty() && Path::new(value).is_absolute())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|path| path.join(".local/state")))
        } else {
            dirs::data_local_dir()
        };
        let base = base.ok_or_else(|| io::Error::other("cannot determine user data directory"))?;
        Ok(Self {
            path: base.join("cargo-gc/registry.toml"),
        })
    }

    fn with_lock<T>(&self, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        fs::create_dir_all(
            self.path
                .parent()
                .ok_or_else(|| io::Error::other("invalid registry path"))?,
        )?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        FileExt::lock_exclusive(&lock)?;
        // Dropping the handle releases the lock on both normal and error paths.
        operation()
    }

    fn load(&self) -> io::Result<Registry> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registry {
                    version: 1,
                    projects: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let registry: Registry = toml::from_str(&text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid registry {}: {error}", self.path.display()),
            )
        })?;
        if registry.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported registry version {}", registry.version),
            ));
        }
        let mut seen = BTreeSet::new();
        for project in &registry.projects {
            if !Path::new(&project.path).is_absolute() || !seen.insert(&project.path) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "registry contains a relative or duplicate project path",
                ));
            }
        }
        Ok(registry)
    }

    pub fn read(&self) -> io::Result<Registry> {
        self.with_lock(|| self.load())
    }

    pub fn update<T>(
        &self,
        operation: impl FnOnce(&mut Registry) -> io::Result<T>,
    ) -> io::Result<T> {
        self.with_lock(|| {
            let mut registry = self.load()?;
            let result = operation(&mut registry)?;
            let text = toml::to_string_pretty(&registry).map_err(io::Error::other)?;
            let mut temporary = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
            temporary.write_all(text.as_bytes())?;
            temporary.as_file().sync_all()?;
            // Atomic replacement of an existing file, including on Windows.
            temporary.persist(&self.path).map_err(|error| error.error)?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn round_trip_idempotence_and_concurrent_updates() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(RegistryStore {
            path: temporary.path().join("state/registry.toml"),
        });
        assert!(store.read().unwrap().projects.is_empty());
        let root = fs::canonicalize(temporary.path()).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let store = store.clone();
                let path = root.join(format!("project-{index}"));
                std::thread::spawn(move || {
                    store
                        .update(|registry| {
                            assert!(registry.insert(&path)?);
                            assert!(!registry.insert(&path)?);
                            Ok(())
                        })
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.read().unwrap().projects.len(), 8);
    }

    #[test]
    fn invalid_registry_is_never_overwritten() {
        let temporary = tempfile::tempdir().unwrap();
        let store = RegistryStore {
            path: temporary.path().join("registry.toml"),
        };
        for text in [
            "",
            "[broken",
            "version = 2",
            "version = 1\nunknown = true",
            "version = 1\n[[projects]]\npath = 'relative'",
        ] {
            fs::write(&store.path, text).unwrap();
            assert!(store.update(|_| Ok(())).is_err());
            assert_eq!(fs::read_to_string(&store.path).unwrap(), text);
        }
    }
}

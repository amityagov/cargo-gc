#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

struct Sandbox {
    _temporary: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        Self {
            _temporary: temporary,
            root,
            home,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-gc"));
        command
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env(
                "RUSTUP_HOME",
                std::env::var_os("RUSTUP_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| dirs::home_dir().unwrap().join(".rustup")),
            )
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("CARGO_HOME", self.home.join(".cargo"))
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("CARGO_BUILD_BUILD_DIR");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn registry_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home
                .join("Library/Application Support/cargo-gc/registry.toml")
        } else {
            self.home.join("state/cargo-gc/registry.toml")
        }
    }

    fn project(&self, name: &str) -> PathBuf {
        let root = self.root.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n"),
        )
        .unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/artifact"), [0; 1024]).unwrap();
        root
    }

    fn add(&self, path: &Path) {
        success(self.command().arg("add").arg(path).output().unwrap());
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn add_list_dry_run_noninteractive_clean_and_remove() {
    let sandbox = Sandbox::new();
    let one = sandbox.project("one");
    let two = sandbox.project("two");
    // Default path, repeated registration, and Cargo's inserted `gc` argument.
    success(
        sandbox
            .command()
            .current_dir(&one)
            .arg("add")
            .output()
            .unwrap(),
    );
    assert!(
        success(
            sandbox
                .command()
                .args(["gc", "add"])
                .arg(&one)
                .output()
                .unwrap()
        )
        .contains("already registered")
    );
    sandbox.add(&two);
    assert!(success(sandbox.run(&["list"])).contains("2 projects, 2.0 KiB total"));
    assert!(success(sandbox.run(&["clean", "--dry-run"])).contains("Total: 2.0 KiB"));
    assert!(one.join("target/artifact").exists());
    let refused = sandbox.run(&["clean"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("non-interactive"));
    assert!(one.join("target/artifact").exists());
    assert!(success(sandbox.run(&["clean", "--yes"])).contains("Cleaned: 2, errors: 0"));
    assert!(!one.join("target").exists());
    assert!(!two.join("target").exists());
    assert!(one.join("Cargo.toml").exists());
    success(
        sandbox
            .command()
            .current_dir(&one)
            .arg("remove")
            .output()
            .unwrap(),
    );
    assert!(success(sandbox.run(&["list"])).contains("1 projects"));
    assert!(one.join("Cargo.toml").exists());
}

#[test]
fn missing_project_can_be_removed_with_normalized_path() {
    let sandbox = Sandbox::new();
    let project = sandbox.project("gone");
    sandbox.add(&project);
    fs::rename(&project, sandbox.root.join("moved")).unwrap();
    assert!(success(sandbox.run(&["list"])).contains("missing"));
    success(sandbox.run(&["remove", "./gone/../gone"]));
    assert!(success(sandbox.run(&["list"])).contains("0 projects"));
    assert!(sandbox.root.join("moved/target/artifact").exists());
}

#[test]
fn unsafe_project_is_counted_but_other_projects_are_cleaned() {
    let sandbox = Sandbox::new();
    let bad = sandbox.project("bad");
    let good = sandbox.project("good");
    sandbox.add(&bad);
    sandbox.add(&good);
    let outside = sandbox.root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "keep").unwrap();
    fs::rename(bad.join("target"), bad.join("saved-target")).unwrap();
    std::os::unix::fs::symlink(&outside, bad.join("target")).unwrap();
    let output = sandbox.run(&["clean", "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Cleaned: 1, errors: 1"));
    assert!(outside.join("keep").exists());
    assert!(!good.join("target").exists());
    assert!(bad.join("saved-target/artifact").exists());
}

#[test]
fn forged_registry_and_changed_manifest_cannot_authorize_cleanup() {
    let sandbox = Sandbox::new();
    let project = sandbox.project("project");
    sandbox.add(&project);
    fs::write(project.join("Cargo.toml"), "[not-a-project]\nvalue=1").unwrap();
    assert!(!sandbox.run(&["clean", "--yes"]).status.success());
    assert!(project.join("target/artifact").exists());
    let fake = sandbox.root.join("fake");
    fs::create_dir_all(fake.join("target")).unwrap();
    fs::write(fake.join("target/keep"), "keep").unwrap();
    fs::write(
        sandbox.registry_path(),
        format!(
            "version=1\n[[projects]]\npath={:?}\n",
            fake.to_str().unwrap()
        ),
    )
    .unwrap();
    assert!(!sandbox.run(&["clean", "--yes"]).status.success());
    assert!(fake.join("target/keep").exists());
}

#[test]
fn inherited_global_local_and_environment_configuration_is_skipped() {
    let sandbox = Sandbox::new();
    let project = sandbox.project("project");
    sandbox.add(&project);
    for directory in [
        project.join(".cargo"),
        sandbox.root.join(".cargo"),
        sandbox.home.join(".cargo"),
    ] {
        fs::create_dir_all(&directory).unwrap();
        let config = directory.join("config.toml");
        fs::write(&config, "[build]\ntarget-dir='elsewhere'\n").unwrap();
        assert!(!sandbox.run(&["clean", "--yes"]).status.success());
        assert!(project.join("target/artifact").exists());
        fs::remove_file(config).unwrap();
    }
    for name in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        assert!(
            !sandbox
                .command()
                .env(name, sandbox.root.join("external"))
                .args(["clean", "--yes"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(project.join("target/artifact").exists());
    }
}

#[test]
fn workspace_scan_deduplicates_and_member_remove_works() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let member = sandbox.project("member");
    fs::rename(&member, workspace.join("member")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers=['member']\nresolver='3'\n",
    )
    .unwrap();
    assert!(success(sandbox.run(&["scan", "workspace"])).contains("Added: 1"));
    assert!(success(sandbox.run(&["scan", "workspace"])).contains("Skipped: 1"));
    success(sandbox.run(&["remove", "workspace/member"]));
    assert!(success(sandbox.run(&["list"])).contains("0 projects"));
    assert!(workspace.join("member/target/artifact").exists());
}

#[test]
fn scan_quietly_skips_nested_nonmember_fixtures_but_warns_for_invalid_manifests() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let member = sandbox.project("member");
    fs::rename(&member, workspace.join("member")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers=['member']\nresolver='3'\n",
    )
    .unwrap();
    let fixture = sandbox.project("fixture");
    let nested = workspace.join("member/test-fixture/assets/fake_sysroot/alloc");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    fs::rename(&fixture, &nested).unwrap();

    let output = sandbox.run(&["scan", "workspace"]);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(success(output).contains("Added: 1"));
    assert!(success(sandbox.run(&["list"])).contains("1 projects"));

    // Explicitly adding the fixture still explains why Cargo rejects it.
    let output = sandbox.command().arg("add").arg(&nested).output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("current package believes it's in a workspace when it's not")
    );

    let broken = workspace.join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join("Cargo.toml"), "[broken").unwrap();
    let output = sandbox.run(&["scan", "workspace"]);
    let warnings = String::from_utf8_lossy(&output.stderr);
    assert!(warnings.contains("warning:"));
    assert!(warnings.contains("broken/Cargo.toml"));
    assert!(!warnings.contains("fake_sysroot"));
    assert!(success(output).contains("Skipped: 1"));
    assert!(nested.join("target/artifact").exists());
}

#[test]
fn scan_quietly_skips_manifests_without_build_targets() {
    let sandbox = Sandbox::new();
    let valid = sandbox.project("valid");
    let fixture = sandbox.root.join("tests/testsuite/cargo_add/example/out");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();

    let output = sandbox.run(&["scan"]);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(success(output).contains("Added: 1"));
    assert!(success(sandbox.run(&["list"])).contains("1 projects"));
    assert!(valid.join("target/artifact").exists());

    let output = sandbox.command().arg("add").arg(&fixture).output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no targets specified in the manifest")
    );
}

#[test]
fn scan_quietly_skips_workspace_inheritance_without_a_workspace_root() {
    let sandbox = Sandbox::new();
    sandbox.project("valid");
    let orphan = sandbox.project("engine_tirocks");
    fs::write(
        orphan.join("Cargo.toml"),
        "[package]\nname='engine_tirocks'\nversion='0.1.0'\n[dependencies]\napi_version.workspace=true\n",
    ).unwrap();

    let output = sandbox.run(&["scan"]);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(success(output).contains("Added: 1"));
    let listed = success(sandbox.run(&["list"]));
    assert!(listed.contains("1 projects"));
    assert!(!listed.contains("engine_tirocks"));

    let output = sandbox.command().arg("add").arg(&orphan).output().unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("error inheriting `api_version`"));
    assert!(diagnostic.contains("failed to find a workspace root"));
    assert!(orphan.join("target/artifact").exists());
}

#[test]
fn scan_quietly_skips_manifests_requiring_nightly_cargo() {
    let sandbox = Sandbox::new();
    sandbox.project("valid");
    let nightly = sandbox.project("nightly_only");
    fs::write(
        nightly.join("Cargo.toml"),
        "cargo-features=['path-bases']\n[package]\nname='nightly_only'\nversion='0.1.0'\n",
    )
    .unwrap();

    let output = sandbox
        .command()
        .env("RUSTUP_TOOLCHAIN", "stable")
        .arg("scan")
        .output()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(success(output).contains("Added: 1"));
    let listed = success(sandbox.run(&["list"]));
    assert!(listed.contains("1 projects"));
    assert!(!listed.contains("nightly_only"));

    let output = sandbox
        .command()
        .env("RUSTUP_TOOLCHAIN", "stable")
        .arg("add")
        .arg(&nightly)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires a nightly version of Cargo")
    );
    assert!(nightly.join("target/artifact").exists());
}

#[test]
fn concurrent_processes_preserve_updates_and_corruption_is_preserved() {
    let sandbox = Sandbox::new();
    let processes: Vec<_> = (0..6)
        .map(|index| {
            let project = sandbox.project(&format!("project{index}"));
            sandbox
                .command()
                .arg("add")
                .arg(project)
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut process in processes {
        assert!(process.wait().unwrap().success());
    }
    assert!(success(sandbox.run(&["list"])).contains("6 projects"));
    fs::write(sandbox.registry_path(), "version=999\n").unwrap();
    assert!(!sandbox.run(&["remove", "project0"]).status.success());
    assert_eq!(
        fs::read_to_string(sandbox.registry_path()).unwrap(),
        "version=999\n"
    );
}

#[test]
fn actual_cargo_subcommand_help_and_version() {
    let sandbox = Sandbox::new();
    let mut command = sandbox.command();
    command.env(
        "PATH",
        std::env::join_paths(
            std::iter::once(
                PathBuf::from(env!("CARGO_BIN_EXE_cargo-gc"))
                    .parent()
                    .unwrap()
                    .to_path_buf(),
            )
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap(),
    );
    // Reuse the exact isolated environment for the real Cargo invocation.
    let mut cargo = Command::new("cargo");
    cargo.current_dir(&sandbox.root).args(["gc", "--help"]);
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => {
                cargo.env(name, value);
            }
            None => {
                cargo.env_remove(name);
            }
        }
    }
    assert!(success(cargo.output().unwrap()).contains("Manage and clean"));
    assert!(success(sandbox.run(&["--version"])).contains("cargo-gc 0.1.0"));
}

#[test]
fn permission_errors_are_explicit_and_do_not_delete_other_data() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let project = sandbox.project("project");
    sandbox.add(&project);
    let target = project.join("target");
    let original = fs::metadata(&target).unwrap().permissions();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o0)).unwrap();
    // Root can bypass permission bits; verify this scenario only when enforced.
    if fs::read_dir(&target).is_err() {
        let listed = sandbox.run(&["list"]);
        assert!(!listed.status.success());
        assert!(String::from_utf8_lossy(&listed.stdout).contains("incomplete"));
        assert!(!sandbox.run(&["clean", "--yes"]).status.success());
    }
    fs::set_permissions(&target, original).unwrap();
    assert!(target.join("artifact").exists());
}

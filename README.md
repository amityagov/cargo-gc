# cargo-gc

Register local Rust projects and centrally clean their standard `target/` directories.
Requires Rust/Cargo with edition 2024 support. Cargo must be available on `PATH`
for manifest validation and workspace discovery.

## Install

```sh
cargo install --path .
cargo gc --help
```

Both `cargo gc` and `cargo-gc` support the same commands:

```sh
cargo gc scan ~/projects
cargo gc scan . --depth 5
cargo gc add                 # current project/workspace
cargo gc add ~/projects/app
cargo gc list
cargo gc clean --dry-run
cargo gc clean               # interactive confirmation
cargo gc clean --yes         # suitable for scripts
cargo gc remove              # unregister current project/workspace
cargo gc remove ~/projects/app
```

Scan defaults to the current directory, skips `.git`, `target`, `node_modules`
and symlinks, and continues with warnings for invalid or inaccessible projects.
Manifests with no build targets, nested packages that are not members of their
enclosing workspace (commonly test fixtures), and packages inheriting workspace
settings without a workspace root are quietly skipped by scan. Manifests using
Cargo features that require nightly are also skipped when the current Cargo
cannot parse them. Explicit `add` still reports the Cargo error for these packages.
Depth counts project directories below the scan root; `--depth 0` checks only
the root. Workspace members register the workspace root, with Cargo respecting
workspace exclusions and explicit `package.workspace` references.
Add and scan are idempotent. Remove changes only the registry and supports saved
paths to missing projects. List retains missing entries and recalculates sizes
without following symlinks; access errors produce an explicitly incomplete total.

## Storage

The versioned TOML registry lives at:

- Linux: `$XDG_STATE_HOME/cargo-gc/registry.toml`, defaulting to
  `~/.local/state/cargo-gc/registry.toml`. Relative/empty XDG settings are ignored.
- macOS: `~/Library/Application Support/cargo-gc/registry.toml`.
- Windows: the user's local application data directory, under `cargo-gc`.

Updates use a separate file lock and an atomic replacement file. Invalid or
incompatible registries cause an error and are preserved. Sizes are not cached.
Paths must be valid UTF-8 for TOML storage.

## Cleanup limits and safety

Only a real, immediate `target/` child of a canonical registered project or
workspace root is eligible. Cleanup validates the Cargo project, refuses home
and filesystem roots and symlink targets, and retains a directory handle to
contain deletion when paths change. Nested symlinks are removed as links without
following them. Each project is revalidated before deletion.

Projects with target/build directory overrides are conservatively skipped,
including `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`,
local/ancestor/current-directory/global Cargo config, and config includes.
Unreadable or invalid config also prevents deletion. Even an explicit local
override is skipped in this MVP. Missing projects or targets are harmless skips.

Dry runs never prompt or delete. Noninteractive cleanup requires `--yes`.
Cleanup continues after per-project failures, reports the error count, and
returns a nonzero exit status for partial failures (including during a dry run).
List also returns nonzero when sizes cannot be determined. Scan reports warnings
while registering valid projects. No Cargo caches, shared/external targets,
scheduling, background services, or age-based cleanup are supported.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy -- -D warnings
```

Tests use temporary fixtures; CLI tests isolate the home directory and registry.

# Contributing

This guide covers setting up a local development environment for contributing to Eidetica.

## Prerequisites

Eidetica uses [Nix](https://nixos.org/) for reproducible development environments. Install Nix with flakes enabled, or use the [Determinate Systems installer](https://github.com/DeterminateSystems/nix-installer) which enables flakes by default.
The Nix flake provides pinned versions of all development tools: Rust toolchain, cargo-nextest, mdbook, formatters, and more.

If you want to skip Nix, a standard Rust toolchain should be sufficient.
The main project is structured as a Cargo workspace.

## Command Runner

[just](https://github.com/casey/just) provides convenient commands for common workflows.
Commands wrap cargo, nix, and other tools as needed.

```bash
just   # See all available commands
```

### Common Commands

| Command                | Description                                     |
| ---------------------- | ----------------------------------------------- |
| `just dev`             | Fast local tests and Clippy (no separate build) |
| `just build`           | Build all targets explicitly                    |
| `just test`            | Run tests with cargo nextest                    |
| `just lint`            | Linting (clippy, audit, etc.)                   |
| `just fmt`             | Multi-language formatting                       |
| `just ci`              | Full local check-only pipeline                  |
| `just ci nix`          | Push-CI graph: checks and all integrations      |
| `just nix test sqlite` | Hermetic SQLite check only                      |
| `just nix lint clippy` | Hermetic Clippy check only                      |
| `just nix doc links`   | Offline documentation link check                |

### Parallel Buck2 build

Buck2 builds the library, CLI, examples, unit/integration tests, and book doctests
with native Rust rules alongside Cargo. See [Buck2 build](buck2.md) for the tool
setup, commands, dependency regeneration, and Nix packaging limitations.

### Testing

| Command         | Description                            |
| --------------- | -------------------------------------- |
| `just test`     | Unit and integration tests via nextest |
| `just test doc` | Code examples in `///` doc comments    |
| `just doc test` | Code examples in mdbook documentation  |

## Nix Commands

Direct Nix commands are available when needed:

| Command                    | Description                                            |
| -------------------------- | ------------------------------------------------------ |
| `nix develop`              | Enter the development shell                            |
| `nix build`                | Build the default package                              |
| `nix flake check`          | Run all CI checks                                      |
| `nix build .#test.default` | Run default tests (sqlite)                             |
| `nix build .#test.all`     | Run all tests including all backends                   |
| `nix build .#lint.default` | Run fast lints (clippy, deny, statix...)               |
| `nix run .#test`           | Interactive test runner (override with `TEST_BACKEND`) |
| `nix run .#fix`            | Auto-fix linting issues and format code                |
| `nix run .#bench`          | Run benchmarks interactively                           |
| `nix run .#coverage`       | Run coverage interactively                             |

Packages are organized into groups: `test`, `doc`, `lint`, `coverage`, `sanitize`.
Each group supports `.#<group>.default` (fast), `.#<group>.all` (all), and `.#<group>.<name>` (specific).
See [CI/Build Infrastructure](ci.md) for details.

Binary caching via a [binary cache](https://cache.eidetica.dev) speeds up builds by providing pre-built dependencies.

## Benchmarks

Criterion benchmarks live in `crates/lib/benches/`. Run them with `nix run .#bench`, or a
single target with `cargo bench --bench backend_benchmarks`. CI runs `cargo bench --workspace`
weekly and uploads the results to Bencher.

Sample size is set per harness — 30 for `backend_benchmarks`, 50 for `benchmarks` — and
`--sample-size N` overrides it:

```bash
cargo bench --bench backend_benchmarks -- --sample-size 60
```

Two groups build a large tree in every iteration's setup, so they fall back to a reduced
sample size of 10 when the flag is absent: `large_tree_operations` and `get_tree_from_tips`.
Each announces that on stderr. Results are noisy at that size — pass `--sample-size` when a
number needs to be trusted, and note that the tracked history for those two groups was
collected at 10 samples, so it is not comparable with a larger run.

## Development Workflow

1. Enter the dev shell: `nix develop` or use direnv.
2. Make changes and run `just dev` for SQLite tests and Clippy. Nextest builds the test binaries, and Clippy checks all targets, so a separate `just build` is not needed for routine validation.
3. During iteration, run a focused test with `just test <filter>`; use `just test service` for service-backend changes. These use Cargo's incremental cache in the current worktree.
4. Run `just ci` for the full local check-only pipeline (format, lint, API docs, tests, doc tests and book links). Run `just fix` separately when fixes are needed; `just ci` does not edit the tree.
5. Use `just ci nix` for the reproducible push-CI graph before delivery: all checks and integration tests, including the real daemon smoke test. The release binary is built as an integration dependency; there is no separate binary-build pass.

## CI Integration

The same checks that run locally also run in CI. See [CI/Build Infrastructure](ci.md) for details on the CI systems.

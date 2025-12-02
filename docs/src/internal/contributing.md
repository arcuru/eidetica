# Contributing

This guide covers setting up a local development environment for contributing to Eidetica.

## Prerequisites

Eidetica uses [Nix](https://nixos.org/) for reproducible development environments. Install Nix with flakes enabled, or use the [Determinate Systems installer](https://github.com/DeterminateSystems/nix-installer) which enables flakes by default.
The Nix flake provides pinned versions of all development tools: Rust toolchain, cargo-nextest, mdbook, formatters, and more.

If you want to skip Nix, a standard Rust toolchain should be sufficient.
The main project is structured as a Cargo workspace.

## Task Runner

[Taskfile](https://taskfile.dev/) provides convenient commands for common workflows.
Tasks wrap cargo, nix, and other tools as needed.

```bash
task --list   # See all available tasks
```

### Common Commands

| Command         | Description                  |
| --------------- | ---------------------------- |
| `task build`    | Fast incremental build       |
| `task test`     | Run tests with cargo nextest |
| `task clippy`   | Strict linting               |
| `task fmt`      | Multi-language formatting    |
| `task ci:local` | Full local CI pipeline       |
| `task ci:nix`   | Nix CI pipeline              |

### Testing

| Command          | Description                            |
| ---------------- | -------------------------------------- |
| `task test`      | Unit and integration tests via nextest |
| `task test:doc`  | Code examples in `///` doc comments    |
| `task book:test` | Code examples in mdbook documentation  |

## Nix Commands

Direct Nix commands are available when needed:

| Command           | Description                 |
| ----------------- | --------------------------- |
| `nix develop`     | Enter the development shell |
| `nix build`       | Build the default package   |
| `nix flake check` | Run all CI checks           |

Binary caching via [Cachix](https://eidetica.cachix.org) speeds up builds by providing pre-built dependencies.

## Development Workflow

1. Enter the dev shell: `nix develop` or use direnv
2. Make changes
3. Build: `task build`
4. Test: `task test`
5. Lint: `task clippy`
6. Format: `task fmt`
7. Run full CI locally before pushing: `task ci:local`

## CI Integration

The same checks that run locally also run in CI. See [CI/Build Infrastructure](ci.md) for details on the CI systems.

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

| Command       | Description                   |
| ------------- | ----------------------------- |
| `just build`  | Fast incremental build        |
| `just test`   | Run tests with cargo nextest  |
| `just lint`   | Linting (clippy, audit, etc.) |
| `just fmt`    | Multi-language formatting     |
| `just ci`     | Full local CI pipeline        |
| `just ci nix` | Nix CI pipeline               |

### Testing

| Command         | Description                            |
| --------------- | -------------------------------------- |
| `just test`     | Unit and integration tests via nextest |
| `just test doc` | Code examples in `///` doc comments    |
| `just doc test` | Code examples in mdbook documentation  |

## Nix Commands

Direct Nix commands are available when needed:

| Command                    | Description                              |
| -------------------------- | ---------------------------------------- |
| `nix develop`              | Enter the development shell              |
| `nix build`                | Build the default package                |
| `nix flake check`          | Run all CI checks                        |
| `nix build .#test.default` | Run default tests (sqlite)               |
| `nix build .#test.all`     | Run all tests including all backends     |
| `nix build .#lint.default` | Run fast lints (clippy, deny, statix...) |

Packages are organized into groups: `test`, `doc`, `lint`, `coverage`, `sanitize`.
Each group supports `.#<group>.default` (fast), `.#<group>.all` (all), and `.#<group>.<name>` (specific).
See [CI/Build Infrastructure](ci.md) for details.

Binary caching via [Cachix](https://eidetica.cachix.org) speeds up builds by providing pre-built dependencies.

## Development Workflow

1. Enter the dev shell: `nix develop` or use direnv
2. Make changes
3. Build: `just build`
4. Test: `just test`
5. Lint: `just lint`
6. Format: `just fmt`
7. Run full CI locally before pushing: `just ci`

## CI Integration

The same checks that run locally also run in CI. See [CI/Build Infrastructure](ci.md) for details on the CI systems.

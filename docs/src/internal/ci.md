# CI/Build Infrastructure

Eidetica uses a CI/build system with GitHub Actions, Forgejo CI, and Nix flakes.

The philosophy for CI is that compute resources are cheap and developer resources (my time) are expensive. CI is used for comprehensive testing, status reporting, security checks, dependency updates, documentation generation, and releasing. In the current setup some of these are run multiple times on several platforms to ensure compatibility and reliability across different environments.

Fuzz / simulation testing are planned for the future.

## CI Systems

### GitHub Actions

The primary CI runs on GitHub with these workflows:

- **[rust.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/rust.yml)**: Main Rust CI pipeline (format, clippy, build, test, doc tests, book tests)
- **[nix.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/nix.yml)**: Nix-based CI that mostly runs the same tests but inside the Nix sandbox
- **[security.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/security.yml)**: Weekly vulnerability scanning and dependency review
- **[coverage.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/coverage.yml)**: Multi-backend code coverage tracking via Codecov
- **[deploy-docs.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/deploy-docs.yml)**: Documentation deployment to GitHub Pages
- **[release-plz.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/release-plz.yml)**: Automated releases and crates.io publishing

### Forgejo CI

A dedicated Forgejo runner provides CI redundancy on [Codeberg](https://codeberg.org/arcuru/eidetica). The Forgejo workflows mirror the testing in the GitHub Actions setup with minor adaptations for the Forgejo environment.

## Nix Flake

The Nix flake defines reproducible builds and CI checks that run identically locally and in CI:

- `nix build` - Build the default package
- `nix flake check` - Run all CI checks (audit, clippy, doc, test, etc.)

Binary caching via [Cachix](https://eidetica.cachix.org) speeds up builds by providing pre-built dependencies.

## Nix Package Groups

The Nix flake organizes packages into groups with a consistent pattern:

- `.#<group>.default` — runs a sensible default subset
- `.#<group>.all` — runs ALL items in the group
- `.#<group>.<name>` — runs a specific item

| Group         | Default                   | All               |
| ------------- | ------------------------- | ----------------- |
| `test`        | sqlite                    | all backends      |
| `doc`         | api + test + book-test    | includes book     |
| `lint`        | clippy + deny + statix... | includes udeps    |
| `coverage`    | sqlite                    | all backends      |
| `sanitize`    | asan + lsan               | includes miri     |
| `integration` | all                       | nixos + container |
| `eval`        | all                       | nixos + hm        |

## Code Coverage

Code coverage runs against all storage backends to ensure comprehensive test coverage:

| Backend    | CI  | Taskfile                 | Nix                             |
| ---------- | --- | ------------------------ | ------------------------------- |
| SQLite     | ✓   | `task coverage`          | `nix build .#coverage.sqlite`   |
| InMemory   | ✓   | `task coverage:inmemory` | `nix build .#coverage.inmemory` |
| PostgreSQL | ✓   | `task coverage:postgres` | `nix build .#coverage.postgres` |
| All        | —   | `task coverage:all`      | `nix build .#coverage.all`      |

### Local Coverage Commands

```bash
task coverage           # SQLite backend (default)
task coverage:inmemory  # InMemory backend
task coverage:postgres  # PostgreSQL (starts a container automatically)
task coverage:all       # All backends, merged into coverage/lcov.info
```

The `coverage:all` task runs all three backends sequentially and merges the LCOV reports using `lcov`. Individual reports are saved as `coverage/lcov-{backend}.info`.

### CI Coverage

GitHub Actions runs coverage for each backend in parallel and uploads to [Codecov](https://codecov.io) with backend-specific flags. Codecov automatically merges the reports server-side, providing both combined coverage metrics and per-backend breakdowns.

For local development setup, see [Contributing](contributing.md).

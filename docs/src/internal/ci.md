# CI/Build Infrastructure

Eidetica uses a CI/build system with GitHub Actions, Forgejo CI, and Nix flakes.

The philosophy for CI is that compute resources are cheap and developer resources (my time) are expensive. CI is used for comprehensive testing, status reporting, security checks, dependency updates, documentation generation, and releasing. In the current setup some of these are run multiple times on several platforms to ensure compatibility and reliability across different environments.

Fuzz / simulation testing are planned for the future.

## CI Systems

### GitHub Actions

The primary CI runs on GitHub with these workflows:

- **[tests.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/tests.yml)**: Main test pipeline (format, build, test, doc tests, book tests, minimal features)
- **[lint.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/lint.yml)**: Linting checks (clippy, deny, formatting)
- **[doc.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/doc.yml)**: Documentation building and testing
- **[sanitizers.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/sanitizers.yml)**: Memory and thread sanitizer checks
- **[benchmarks.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/benchmarks.yml)**: Performance benchmarks
- **[container.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/container.yml)**: Container image building
- **[artifacts.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/artifacts.yml)**: Build artifact management
- **[coverage.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/coverage.yml)**: Multi-backend code coverage tracking via Codecov
- **[deploy-docs.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/deploy-docs.yml)**: Documentation deployment to GitHub Pages
- **[release-plz.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/release-plz.yml)**: Automated releases and crates.io publishing
- **[renovate.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/renovate.yml)**: Dependency update automation
- **[codeberg.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/codeberg.yml)**: Codeberg mirror sync

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

| Group         | Default                       | All                         |
| ------------- | ----------------------------- | --------------------------- |
| `test`        | sqlite                        | all backends                |
| `doc`         | api + test + booktest + links | includes book               |
| `lint`        | all except udeps, minversions | includes udeps, minversions |
| `coverage`    | sqlite                        | all backends                |
| `sanitize`    | asan + lsan                   | includes miri               |
| `integration` | all                           | nixos + container           |
| `eval`        | all                           | nixos + hm                  |

## Interactive Runners

Interactive runners execute commands with live output, accepting additional arguments:

| Command                   | Description                                              |
| ------------------------- | -------------------------------------------------------- |
| `nix run`                 | Run the eidetica binary                                  |
| `nix run .#fix`           | Auto-fix linting issues and format code                  |
| `nix run .#bench`         | Run benchmarks interactively                             |
| `nix run .#coverage`      | Run coverage interactively                               |
| `nix run .#test`          | Run tests (no backend set, override with `TEST_BACKEND`) |
| `nix run .#test-sqlite`   | Run tests with sqlite backend                            |
| `nix run .#test-inmemory` | Run tests with inmemory backend                          |
| `nix run .#test-postgres` | Run tests with postgres backend (Linux)                  |
| `nix run .#test-all`      | Run tests with all backends sequentially                 |

## Code Coverage

Code coverage runs against all storage backends to ensure comprehensive test coverage:

| Backend    | CI  | just                     | Nix                             |
| ---------- | --- | ------------------------ | ------------------------------- |
| SQLite     | ✓   | `just coverage`          | `nix build .#coverage.sqlite`   |
| InMemory   | ✓   | `just coverage inmemory` | `nix build .#coverage.inmemory` |
| PostgreSQL | ✓   | `just coverage postgres` | `nix build .#coverage.postgres` |
| All        | —   | `just coverage all`      | `nix build .#coverage.all`      |

### Local Coverage Commands

```bash
just coverage            # SQLite backend (default)
just coverage inmemory   # InMemory backend
just coverage postgres   # PostgreSQL (starts a container automatically)
just coverage all        # All backends, merged into coverage/lcov.info
```

The `coverage all` command runs all three backends sequentially and merges the LCOV reports using `lcov`. Individual reports are saved as `coverage/lcov-{backend}.info`.

### CI Coverage

GitHub Actions runs coverage for each backend in parallel and uploads to [Codecov](https://codecov.io) with backend-specific flags. Codecov automatically merges the reports server-side, providing both combined coverage metrics and per-backend breakdowns.

For local development setup, see [Contributing](contributing.md).

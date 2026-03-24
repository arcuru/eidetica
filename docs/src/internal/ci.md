# CI/Build Infrastructure

Eidetica uses a CI/build system with GitHub Actions, Forgejo CI, and Nix flakes.

The philosophy for CI is that compute resources are cheap and developer resources (my time) are expensive. CI is used for comprehensive testing, status reporting, security checks, dependency updates, documentation generation, and releasing. In the current setup some of these are run multiple times on several platforms to ensure compatibility and reliability across different environments.

Fuzz / simulation testing are planned for the future.

## CI Systems

### GitHub Actions

The primary CI runs on GitHub with these workflows:

- **[ci.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/ci.yml)**: Main CI pipeline (lint, test, integration tests, docs)
- **[release.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/release.yml)**: Release pipeline (build, publish container images, create manifests)
- **[sanitizers.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/sanitizers.yml)**: Memory and thread sanitizer checks
- **[benchmarks.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/benchmarks.yml)**: Performance benchmarks
- **[coverage.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/coverage.yml)**: Multi-backend code coverage tracking via Codecov
- **[deploy-docs.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/deploy-docs.yml)**: Documentation deployment to GitHub Pages
- **[release-plz.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/release-plz.yml)**: Automated releases and crates.io publishing
- **[codeberg.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/codeberg.yml)**: Codeberg mirror sync (main branch only)
- **[security-audit.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/security-audit.yml)**: Daily advisory scanning
- **[cargo-update.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/cargo-update.yml)**: Monthly cargo dependency updates
- **[flake-update.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/flake-update.yml)**: Monthly Nix flake input updates
- **[actions-update.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/actions-update.yml)**: Monthly GitHub Actions version updates
- **[dependency-hold.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/dependency-hold.yml)**: Merge gate for dependency PRs
- **[update-hold.yml](https://github.com/arcuru/eidetica/blob/main/.github/workflows/update-hold.yml)**: Automatic hold expiry

### Forgejo CI

A dedicated Forgejo runner provides CI redundancy on [Codeberg](https://codeberg.org/arcuru/eidetica). The Forgejo workflows mirror the testing in the GitHub Actions setup with minor adaptations for the Forgejo environment.

## Nix Flake

The Nix flake defines reproducible builds and CI checks that run identically locally and in CI:

- `nix build` - Build the default package
- `nix flake check` - Run all CI checks (audit, clippy, doc, test, etc.)

Binary caching via a [nix binary cache](https://wiki.nixos.org/wiki/Binary_Cache) speeds up builds by providing pre-built dependencies. It is located at:

- URL - `https://cache.eidetica.dev`
- Public Key - `cache.eidetica.dev-1:eND5gRJlbnool3ZLCWT2H8kkygWS8JcsU76HYXbWPBI=`

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

## Dependency Management

Dependencies are managed through purpose-built GitHub Actions workflows rather than third-party tools. Three categories of dependencies are updated monthly, each with a mandatory hold period before merge. A daily security audit catches advisory database updates between dependency cycles.

### Security Audits

The [`security-audit.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/security-audit.yml) workflow runs daily at 06:00 UTC. It executes `cargo deny check advisories` against the RustSec Advisory Database.

- On failure: creates or updates a GitHub issue with the `security` label containing the full advisory output
- On success: auto-closes any existing security advisory issue

The `deny.toml` configuration contains an ignore list for known advisories from transitive dependencies (e.g., unmaintained crates pulled in by iroh). The CI lint target (`nix build .#lint.deny`) checks `bans licenses sources` but not `advisories` — advisories are excluded from regular CI because new advisories appear without code changes and would cause flaky builds. The dedicated daily workflow fills this gap.

### Update Workflows

Three monthly update workflows run on the 1st of each month:

| Workflow                                                                                                  | Branch                | What it updates                     |
| --------------------------------------------------------------------------------------------------------- | --------------------- | ----------------------------------- |
| [`cargo-update.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/cargo-update.yml)     | `deps/cargo-update`   | `Cargo.lock` via `cargo update`     |
| [`flake-update.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/flake-update.yml)     | `deps/flake-update`   | `flake.lock` via `nix flake update` |
| [`actions-update.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/actions-update.yml) | `deps/actions-update` | SHA pins in workflow files          |

Each workflow:

1. Sets up the `deps/` branch (creates new or rebases existing onto main)
2. Runs the update command
3. Checks if anything changed (exits cleanly if not)
4. Commits and pushes with `--force-with-lease` (preserving any existing user commits)
5. Creates a PR (or updates the existing one) with `dependencies` and `on-hold` labels
6. Embeds a `<!-- hold-until: YYYY-MM-DD -->` comment in the PR body; CI handles verification

Shared branch setup and commit/PR logic is extracted into composite actions at `.github/actions/`.

The flake update workflow generates an input diff summary with GitHub compare links for each changed flake input (nixpkgs, crane, fenix, etc.).

The actions update workflow parses `uses:` lines across all `.github/workflows/*.yml` and `.forgejo/workflows/*.yml` files, queries the GitHub API for the latest release of each action, and updates SHA pins in-place. It respects the Forgejo constraint: `actions/checkout` in `.forgejo/` stays at v4.

All three workflows support `workflow_dispatch` for manual triggering with a configurable `hold_days` parameter (default: 7).

### Hold Mechanism

Dependency update PRs are gated by a mandatory hold period to allow time for upstream issues to surface before merging.

The hold system has two components:

**[`dependency-hold.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/dependency-hold.yml)** — A PR status check that runs on every pull request to `main`. For branches that start with `deps/`, it queries the GitHub API for the PR's current labels and fails if the `on-hold` label is present, blocking merge. For all other branches, it passes immediately. Live label queries (rather than event payload data) ensure manual re-runs always see current state. This job name (`Dependency Hold`) is configured as a required status check in branch protection.

**[`update-hold.yml`](https://github.com/arcuru/eidetica/blob/main/.github/workflows/update-hold.yml)** — A daily workflow (07:00 UTC) that scans open PRs with the `on-hold` label. It parses the `<!-- hold-until: YYYY-MM-DD -->` comment from each PR body. When today's date meets or exceeds the hold date, it removes the `on-hold` label, creates a passing `Dependency Hold` check run on the PR's HEAD commit, and posts a comment.

```text
Day 0:  cargo-update.yml creates PR with on-hold label
        dependency-hold.yml → FAIL (on-hold label present, merge blocked)

Day 7:  update-hold.yml removes on-hold label + creates passing check run
        Dependency Hold check → PASS (merge allowed)
```

### Required Setup

The hold mechanism requires repository configuration:

- An `on-hold` label in GitHub repo settings
- A `dependencies` label (for categorization)
- `Dependency Hold` added as a required status check in branch protection for `main` (displayed as `Deps: Hold Gate / Dependency Hold` in the checks UI)

## Secret Management

Secrets are scoped to [GitHub Environments](https://docs.github.com/en/actions/deployment/targeting-different-environments/using-environments-for-deployment) with branch restrictions rather than stored as repo-level secrets. This limits the blast radius of a compromised workflow or action.

### Environments

| Environment    | Secrets                                                                                                                    | Purpose                                          |
| -------------- | -------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| `publish`      | `NIX_CACHE_ACCESS_KEY_ID`, `NIX_CACHE_SECRET_ACCESS_KEY`, `NIX_CACHE_SIGNING_KEY`, `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | Release builds, cache push, container publishing |
| `release`      | `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`                                                                                    | Container manifest creation                      |
| `automation`   | `PAT_TOKEN`                                                                                                                | PR creation with elevated permissions            |
| `mirror`       | `GIT_SSH_PRIVATE_KEY`                                                                                                      | Codeberg mirror sync                             |
| _(repo-level)_ | `CODECOV_TOKEN`, `BENCHER_API_TOKEN`                                                                                       | Low-risk upload-only tokens                      |

All environments are restricted to the `main` branch. Low-risk tokens that can only upload metrics or coverage data remain at the repo level.

### Cache Push Isolation

Only the release workflow (`release.yml`) has cache push credentials. CI, coverage, and sanitizer workflows are fully secretless — they read from the public cache but never write to it. This means the Nix cache signing key exists in a single environment (`publish`), and all cache contents are built from source by the release job.

### Release Build Integrity

The release workflow builds published artifacts (binary and container image) from source using `--option substitute false`, bypassing the Nix binary cache entirely. This prevents a cache poisoning attack from affecting published artifacts. The from-source results are pushed to the cache afterward, along with CI debug artifacts (lint, test, doc targets), so subsequent CI runs and developer builds benefit from caching.

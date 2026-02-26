# Eidetica Development Commands
# Run `just` to see available recipes

alias b := build

[private]
default:
    @just --list

# =============================================================================
# Development Workflows
# =============================================================================

# Quick development feedback (build + test + lint)
dev:
    just build
    just test
    just lint clippy

# Run automatic fixes (clippy fix + nix fixes + format)
fix:
    cargo clippy --workspace --fix --allow-dirty --all-targets --all-features --allow-no-vcs -- -D warnings
    statix fix .
    deadnix --edit .
    just fmt

# =============================================================================
# Building
# =============================================================================

# Build the project (debug or release)
build mode='debug':
    cargo build --workspace --all-targets --all-features {{ if mode == "release" { "--release" } else { "" } }} --quiet

# Run local dev server with persistent cache
serve port='3000' mode='debug' backend='sqlite':
    #!/usr/bin/env bash
    set -e

    # PRJ_SPEC-compliant cache directory
    cache_base="${PRJ_CACHE_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}}/eidetica"
    data_dir="${cache_base}/serve-{{ port }}"
    mkdir -p "$data_dir"

    # Build binary
    if [ "{{ mode }}" = "release" ]; then
        cargo build --bin eidetica --release
        bin="target/release/eidetica"
    else
        cargo build --bin eidetica
        bin="target/debug/eidetica"
    fi

    echo "Data directory: $data_dir"
    echo "Starting eidetica on port {{ port }}..."

    # Open browser if graphical display available (backgrounded, ignore failures)
    if [ -n "$DISPLAY" ] || [ -n "$WAYLAND_DISPLAY" ]; then
        (sleep 1 && xdg-open "http://localhost:{{ port }}" >/dev/null 2>&1) &
    fi

    # Run server
    "$bin" serve --port {{ port }} --data-dir "$data_dir" --backend {{ backend }}

# Clean serve cache for a specific port
serve-clean port='3000':
    #!/usr/bin/env bash
    cache_base="${PRJ_CACHE_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}}/eidetica"
    data_dir="${cache_base}/serve-{{ port }}"
    if [ -d "$data_dir" ]; then
        rm -rf "$data_dir"
        echo "Removed $data_dir"
    else
        echo "No cache found at $data_dir"
    fi

# =============================================================================
# Testing
# =============================================================================

# Run tests: [filter], sqlite, postgres, all-backends, doc, full, ignored, minimal, todo
test *args:
    #!/usr/bin/env bash
    set -e
    args="{{ args }}"

    # No args: run main tests (sqlite backend)
    if [ -z "$args" ]; then
        TEST_BACKEND=sqlite cargo nextest run --workspace --all-features --no-fail-fast --status-level fail
        exit 0
    fi

    # Parse first word
    first="${args%% *}"
    rest="${args#* }"
    [ "$first" = "$rest" ] && rest=""

    case "$first" in
        sqlite)
            TEST_BACKEND=sqlite cargo nextest run --workspace --all-features --no-fail-fast --status-level fail $rest
            ;;
        inmemory)
            TEST_BACKEND=inmemory cargo nextest run --workspace --all-features --no-fail-fast --status-level fail $rest
            ;;
        postgres)
            CONTAINER_NAME="eidetica-test-postgres"
            DB_NAME="eidetica_test"
            DB_PORT="54321"
            docker rm --force "$CONTAINER_NAME" 2>/dev/null || true
            trap "docker rm --force $CONTAINER_NAME 2>/dev/null || true" EXIT
            echo "Starting PostgreSQL container..."
            docker run --detach --name "$CONTAINER_NAME" \
                --env POSTGRES_DB="$DB_NAME" \
                --env POSTGRES_HOST_AUTH_METHOD=trust \
                --publish "$DB_PORT":5432 \
                postgres:16-alpine
            echo "Waiting for PostgreSQL to be ready..."
            for i in $(seq 1 30); do
                if docker exec "$CONTAINER_NAME" pg_isready --username postgres --dbname "$DB_NAME" >/dev/null 2>&1; then
                    echo "PostgreSQL is ready!"
                    break
                fi
                if [ "$i" -eq 30 ]; then
                    echo "Timed out waiting for PostgreSQL"
                    exit 1
                fi
                sleep 1
            done
            echo "Running tests against PostgreSQL..."
            TEST_BACKEND=postgres \
            TEST_POSTGRES_URL="postgres://postgres@localhost:$DB_PORT/$DB_NAME" \
            cargo nextest run --workspace --all-features --no-fail-fast --status-level fail $rest
            ;;
        all-backends)
            echo "=== Testing SQLite backend ==="
            just test
            echo "=== Testing InMemory backend ==="
            just test inmemory
            echo "=== Testing PostgreSQL backend ==="
            just test postgres
            ;;
        doc)
            cargo test --doc --workspace --all-features --quiet
            ;;
        full)
            just test
            just test doc
            just doc test
            ;;
        ignored)
            cargo nextest run --workspace --all-features --no-fail-fast --status-level fail --run-ignored all
            ;;
        minimal)
            cargo build --package eidetica --no-default-features
            cargo test --package eidetica --no-default-features --features testing
            ;;
        todo)
            cd examples/todo && ./test.sh
            ;;
        *)
            # Treat as test filter
            cargo nextest run --workspace --all-features --no-fail-fast --status-level fail $args
            ;;
    esac

# =============================================================================
# Linting (Static Analysis)
# =============================================================================

# Run linter(s): clippy, audit, typos, statix, deadnix, shellcheck, yamllint, actionlint, hadolint, markdownlint, gitleaks, udeps, min-versions, all
lint +tools='clippy audit typos statix deadnix shellcheck yamllint actionlint hadolint markdownlint gitleaks':
    #!/usr/bin/env bash
    set -e
    for tool in {{ tools }}; do
        case "$tool" in
            clippy)
                echo "=== Running clippy ==="
                cargo clippy --workspace --all-targets --all-features -- -D warnings
                ;;
            audit)
                echo "=== Running audit (cargo-deny) ==="
                cargo deny check
                ;;
            typos)
                echo "=== Running typos ==="
                typos --config .config/typos.toml
                ;;
            statix)
                echo "=== Running statix ==="
                statix check .
                ;;
            deadnix)
                echo "=== Running deadnix ==="
                deadnix --fail .
                ;;
            shellcheck)
                echo "=== Running shellcheck ==="
                find . -name "*.sh" -type f -exec shellcheck {} +
                ;;
            yamllint)
                echo "=== Running yamllint ==="
                find . \( -name "*.yml" -o -name "*.yaml" \) -type f -exec yamllint -c .config/yamllint.yaml {} +
                ;;
            actionlint)
                echo "=== Running actionlint ==="
                actionlint
                ;;
            hadolint)
                echo "=== Running hadolint ==="
                hadolint Dockerfile
                ;;
            markdownlint)
                echo "=== Running markdownlint ==="
                find . -name "*.md" -not -path "./target/*" -type f -exec markdownlint --config .config/markdownlint.yaml {} +
                ;;
            gitleaks)
                echo "=== Running gitleaks ==="
                gitleaks detect --source . --no-git --verbose --config .config/gitleaks.toml
                ;;
            udeps)
                echo "=== Running udeps ==="
                cargo-nightly udeps --workspace --all-targets --all-features
                ;;
            min-versions)
                echo "=== Checking minimum dependency versions ==="
                cargo-nightly update -Z minimal-versions
                cargo build --workspace --all-targets --all-features --quiet
                cargo nextest run --workspace --all-features --status-level fail
                ;;
            all)
                just lint clippy audit typos statix deadnix shellcheck yamllint actionlint hadolint markdownlint gitleaks udeps min-versions
                ;;
            *)
                echo "Unknown linter: $tool"
                echo "Options: clippy, audit, typos, statix, deadnix, shellcheck, yamllint, actionlint, hadolint, markdownlint, gitleaks, udeps, min-versions, all"
                exit 1
                ;;
        esac
    done

# =============================================================================
# Formatting
# =============================================================================

# Run formatters: (default), check
fmt mode='':
    #!/usr/bin/env bash
    set -e
    case "{{ mode }}" in
        check)
            cargo fmt --all -- --check
            alejandra . --check --quiet
            prettier --check . --log-level warn
            typos --config .config/typos.toml
            ;;
        *)
            cargo fmt --all
            alejandra . --quiet
            prettier --write . --log-level warn
            typos --write-changes --config .config/typos.toml
            ;;
    esac

# =============================================================================
# Sanitizers (Dynamic Analysis)
# =============================================================================

# Run sanitizer(s): miri, careful, asan, tsan, lsan, all
sanitize *targets:
    #!/usr/bin/env bash
    set -e
    if [ -z "{{ targets }}" ]; then
        echo "Available sanitizers:"
        echo "  just sanitize miri     - Miri: Stacked Borrows, UB detection"
        echo "  just sanitize careful  - cargo-careful: extra std debug assertions"
        echo "  just sanitize asan     - AddressSanitizer: memory errors, use-after-free"
        echo "  just sanitize tsan     - ThreadSanitizer: data races"
        echo "  just sanitize lsan     - LeakSanitizer: memory leaks"
        echo ""
        echo "  just sanitize all      - Run all sanitizers except miri"
        echo ""
        echo "Multiple: just sanitize miri asan"
        exit 0
    fi
    for target in {{ targets }}; do
        case "$target" in
            miri)
                echo "=== Running Miri ==="
                cargo-nightly miri test --workspace --all-features
                ;;
            careful)
                echo "=== Running cargo-careful ==="
                cargo-nightly careful test --workspace --all-features
                ;;
            asan)
                echo "=== Running AddressSanitizer ==="
                RUSTFLAGS="-Zsanitizer=address" cargo-nightly test --workspace --all-features --lib --bins --tests --examples --target x86_64-unknown-linux-gnu
                ;;
            tsan)
                echo "=== Running ThreadSanitizer ==="
                CARGO_TARGET_DIR=target/tsan \
                RUSTFLAGS="-Zsanitizer=thread -Zsanitizer-memory-track-origins=1" \
                TSAN_OPTIONS="suppressions=$(pwd)/.config/tsan" \
                cargo-nightly test -Zbuild-std --workspace --all-features --lib --bins --tests --examples --target x86_64-unknown-linux-gnu
                ;;

            lsan)
                echo "=== Running LeakSanitizer ==="
                RUSTFLAGS="-Zsanitizer=leak" cargo-nightly test --workspace --all-features --lib --bins --tests --examples --target x86_64-unknown-linux-gnu
                ;;
            all)
                just sanitize careful asan tsan lsan
                ;;
            *)
                echo "Unknown sanitizer: $target"
                echo "Options: miri, careful, asan, tsan, lsan, all"
                exit 1
                ;;
        esac
    done

# =============================================================================
# Documentation
# =============================================================================

# Build documentation: api, api-full, book, serve, test, clean, links, links-online, stats
doc action='api':
    #!/usr/bin/env bash
    set -e
    case "{{ action }}" in
        api)
            cargo doc --workspace --all-features --no-deps --quiet
            ;;
        api-full)
            cargo doc --workspace --all-features --quiet
            ;;
        book)
            cargo doc --package eidetica --all-features --no-deps --quiet
            ln --symbolic --force --no-dereference ../../target/doc docs/src/rustdoc
            mdbook build docs
            ;;
        serve)
            just doc book
            mdbook serve docs --open
            ;;
        test)
            just doc links
            rm --force target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
            cargo build --package eidetica
            RUST_LOG=error mdbook test docs --library-path target/debug/deps
            ;;
        clean)
            mdbook clean docs
            rm --recursive --force docs/src/rustdoc
            ;;
        links)
            just doc book
            # --exclude-path: don't check files in these directories
            # --exclude 'rustdoc/': don't check links pointing to rustdoc (may not be built)
            # Exclude 404.html: mdbook generates <base href="/"> which lychee can't resolve offline
            lychee --offline --exclude-path 'rustdoc' --exclude-path '404.html' --exclude 'rustdoc/' docs/book
            ;;
        links-online)
            just doc book
            # --exclude-path: don't check files in these directories
            # --exclude 'rustdoc/': don't check links pointing to rustdoc (may not be built)
            lychee --exclude-path 'rustdoc' --exclude-path 'fonts' --exclude 'rustdoc/' docs/book
            ;;
        stats)
            tested=$(grep --recursive '```rust$' docs/src | wc --lines)
            total=$(grep --recursive '```rust' docs/src | wc --lines)
            echo "${tested}/${total} Code Blocks tested"
            ;;
        *)
            echo "Unknown action: {{ action }}"
            echo "Options: api, api-full, book, serve, test, clean, links, links-online, stats"
            exit 1
            ;;
    esac

# =============================================================================
# Coverage
# =============================================================================

# Generate coverage data for a backend: sqlite, inmemory, postgres, all
coverage backend='sqlite':
    #!/usr/bin/env bash
    set -e
    case "{{ backend }}" in
        sqlite)
            TEST_BACKEND=sqlite cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm
            ;;
        inmemory)
            TEST_BACKEND=inmemory cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm
            ;;
        postgres)
            CONTAINER_NAME="eidetica-coverage-postgres"
            DB_NAME="eidetica_test"
            DB_PORT="54322"
            docker rm --force "$CONTAINER_NAME" 2>/dev/null || true
            trap "docker rm --force $CONTAINER_NAME 2>/dev/null || true" EXIT
            echo "Starting PostgreSQL container..."
            docker run --detach --name "$CONTAINER_NAME" \
                --env POSTGRES_DB="$DB_NAME" \
                --env POSTGRES_HOST_AUTH_METHOD=trust \
                --publish "$DB_PORT":5432 \
                postgres:16-alpine
            echo "Waiting for PostgreSQL to be ready..."
            for i in $(seq 1 30); do
                if docker exec "$CONTAINER_NAME" pg_isready --username postgres --dbname "$DB_NAME" >/dev/null 2>&1; then
                    echo "PostgreSQL is ready!"
                    break
                fi
                if [ "$i" -eq 30 ]; then
                    echo "Timed out waiting for PostgreSQL"
                    exit 1
                fi
                sleep 1
            done
            echo "Running coverage against PostgreSQL..."
            TEST_BACKEND=postgres \
            TEST_POSTGRES_URL="postgres://postgres@localhost:$DB_PORT/$DB_NAME" \
            cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm
            ;;
        all)
            just coverage sqlite
            mv coverage/lcov.info coverage/lcov-sqlite.info
            just coverage inmemory
            mv coverage/lcov.info coverage/lcov-inmemory.info
            just coverage postgres
            mv coverage/lcov.info coverage/lcov-postgres.info
            echo "Merging coverage reports..."
            lcov --add-tracefile coverage/lcov-sqlite.info --add-tracefile coverage/lcov-inmemory.info --add-tracefile coverage/lcov-postgres.info --output-file coverage/lcov.info
            echo "Merged coverage report: coverage/lcov.info"
            ;;
        ignored)
            cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm --ignored --no-fail-fast
            ;;
        *)
            echo "Unknown backend: {{ backend }}"
            echo "Options: sqlite, inmemory, postgres, all, ignored"
            exit 1
            ;;
    esac

# =============================================================================
# CI
# =============================================================================

# Run CI locally: local (default), full (containers), nix
ci mode='local':
    #!/usr/bin/env bash
    set -e
    case "{{ mode }}" in
        local)
            just fix
            just lint
            just doc
            just build
            just test full
            ;;
        full)
            act --workflows .github/workflows/rust.yml
            ;;
        nix)
            just nix full
            ;;
        *)
            echo "Unknown mode: {{ mode }}"
            echo "Options: local, full, nix"
            exit 1
            ;;
    esac

# =============================================================================
# Nix
# =============================================================================

# Nix commands: build, check, test, test-all, bench, integration, full
nix action='check':
    #!/usr/bin/env bash
    set -e
    case "{{ action }}" in
        build)
            nix build
            ;;
        check)
            nix-fast-build --no-link --skip-cached ${CI:+--no-nom}
            ;;
        test)
            nix run .#test
            ;;
        test-all)
            # Force re-run all hermetic test backends
            nix build .#test.sqlite .#test.inmemory .#test.minimal --rebuild --print-build-logs --no-link
            ;;
        bench)
            # Force re-run hermetic benchmarks (rebuild even if cached)
            nix build .#bench --rebuild --print-build-logs --no-link
            ;;
        integration)
            nix build .#integration.nixos .#integration.container --print-build-logs --no-link
            ;;
        full)
            just nix check
            nix build .#eidetica.bin --no-link
            just nix integration
            ;;
        *)
            echo "Unknown action: {{ action }}"
            echo "Options: build, check, test, test-all, bench, integration, full"
            exit 1
            ;;
    esac

# =============================================================================
# Container
# =============================================================================

# Build container image: docker, nix
container type='docker':
    #!/usr/bin/env bash
    set -e
    case "{{ type }}" in
        docker)
            docker build --tag eidetica:dev .
            ;;
        nix)
            nix build .#eidetica.image
            docker load < ./result
            ;;
        *)
            echo "Unknown type: {{ type }}"
            echo "Options: docker, nix"
            exit 1
            ;;
    esac

# =============================================================================
# Benchmarks
# =============================================================================

# Run benchmarks and open HTML report
bench:
    TEST_BACKEND=sqlite cargo bench --workspace
    xdg-open target/criterion/report/index.html 2>/dev/null || true

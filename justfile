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

# Run automatic fixes (clippy fix + format)
fix:
    cargo clippy --workspace --fix --allow-dirty --all-targets --all-features --allow-no-vcs -- -D warnings
    just fmt

# =============================================================================
# Building
# =============================================================================

# Build the project (debug or release)
build mode='debug':
    cargo build --workspace --all-targets --all-features {{ if mode == "release" { "--release" } else { "" } }} --quiet

# =============================================================================
# Testing
# =============================================================================

# Run tests: [filter], sqlite, postgres, all-backends, doc, full, ignored, minimal, todo
test *args:
    #!/usr/bin/env bash
    set -e
    args="{{ args }}"

    # No args: run main tests (inmemory backend)
    if [ -z "$args" ]; then
        cargo nextest run --workspace --all-features --no-fail-fast --status-level fail
        exit 0
    fi

    # Parse first word
    first="${args%% *}"
    rest="${args#* }"
    [ "$first" = "$rest" ] && rest=""

    case "$first" in
        sqlite)
            TEST_BACKEND=sqlite cargo nextest run --workspace --features sqlite --no-fail-fast --status-level fail $rest
            ;;
        postgres)
            CONTAINER_NAME="eidetica-test-postgres"
            DB_NAME="eidetica_test"
            DB_PORT="54321"
            docker rm -f "$CONTAINER_NAME" 2>/dev/null || true
            trap "docker rm -f $CONTAINER_NAME 2>/dev/null || true" EXIT
            echo "Starting PostgreSQL container..."
            docker run -d --name "$CONTAINER_NAME" \
                -e POSTGRES_DB="$DB_NAME" \
                -e POSTGRES_HOST_AUTH_METHOD=trust \
                -p "$DB_PORT":5432 \
                postgres:16-alpine
            echo "Waiting for PostgreSQL to be ready..."
            for i in $(seq 1 30); do
                if docker exec "$CONTAINER_NAME" pg_isready -U postgres -d "$DB_NAME" >/dev/null 2>&1; then
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
            cargo nextest run --workspace --features postgres --no-fail-fast --status-level fail $rest
            ;;
        all-backends)
            echo "=== Testing InMemory backend ==="
            just test
            echo "=== Testing SQLite backend ==="
            just test sqlite
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
            cargo build -p eidetica --no-default-features
            cargo test -p eidetica --no-default-features
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

# Run linter(s): clippy, audit, udeps, min-versions, all
lint +tools='clippy audit':
    #!/usr/bin/env bash
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
            udeps)
                echo "=== Running udeps ==="
                cargo udeps --workspace --all-targets
                ;;
            min-versions)
                echo "=== Checking minimum dependency versions ==="
                cargo update -Z minimal-versions
                cargo build --workspace --all-targets --all-features --quiet
                cargo nextest run --workspace --all-features --status-level fail
                ;;
            all)
                just lint clippy audit udeps min-versions
                ;;
            *)
                echo "Unknown linter: $tool"
                echo "Options: clippy, audit, udeps, min-versions, all"
                exit 1
                ;;
        esac
    done

# =============================================================================
# Formatting
# =============================================================================

# Run all formatters
fmt:
    cargo fmt --all
    alejandra . --quiet
    prettier --write . --log-level warn

# =============================================================================
# Sanitizers (Dynamic Analysis)
# =============================================================================

# Run sanitizer(s): miri, careful, asan, tsan, lsan, all
sanitize *targets:
    #!/usr/bin/env bash
    if [ -z "{{ targets }}" ]; then
        echo "Available sanitizers:"
        echo "  just sanitize miri     - Miri: Stacked Borrows, UB detection"
        echo "  just sanitize careful  - cargo-careful: extra std debug assertions"
        echo "  just sanitize asan     - AddressSanitizer: memory errors, use-after-free"
        echo "  just sanitize tsan     - ThreadSanitizer: data races"
        echo "  just sanitize lsan     - LeakSanitizer: memory leaks"
        echo ""
        echo "  just sanitize all      - Run ALL sanitizers (slow, ~30+ min)"
        echo ""
        echo "Multiple: just sanitize miri asan"
        exit 0
    fi
    for target in {{ targets }}; do
        case "$target" in
            miri)
                echo "=== Running Miri ==="
                cargo miri test --workspace --all-features
                ;;
            careful)
                echo "=== Running cargo-careful ==="
                cargo careful test --workspace --all-features
                ;;
            asan)
                echo "=== Running AddressSanitizer ==="
                RUSTFLAGS="-Zsanitizer=address" cargo test --workspace --all-features --target x86_64-unknown-linux-gnu
                ;;
            tsan)
                echo "=== Running ThreadSanitizer ==="
                RUSTFLAGS="-Zsanitizer=thread" cargo test --workspace --all-features --target x86_64-unknown-linux-gnu
                ;;
            lsan)
                echo "=== Running LeakSanitizer ==="
                RUSTFLAGS="-Zsanitizer=leak" cargo test --workspace --all-features --target x86_64-unknown-linux-gnu
                ;;
            all)
                just sanitize miri careful asan tsan lsan
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
    case "{{ action }}" in
        api)
            cargo doc --workspace --all-features --no-deps --quiet
            ;;
        api-full)
            cargo doc --workspace --all-features --quiet
            ;;
        book)
            cargo doc -p eidetica --all-features --no-deps --quiet
            ln -sfn ../../target/doc docs/src/rustdoc
            mdbook build docs
            ;;
        serve)
            just doc book
            mdbook serve docs --open
            ;;
        test)
            just doc links
            rm -f target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
            cargo build -p eidetica
            RUST_LOG=error mdbook test docs -L target/debug/deps
            ;;
        clean)
            mdbook clean docs
            rm -rf docs/src/rustdoc
            ;;
        links)
            just doc book
            lychee --offline --exclude-path 'rustdoc' docs/book
            ;;
        links-online)
            just doc book
            lychee --exclude-path 'rustdoc' --exclude-path 'fonts' docs/book
            ;;
        stats)
            tested=$(grep -r '```rust$' docs/src | wc -l)
            total=$(grep -r '```rust' docs/src | wc -l)
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

# Generate coverage data for a backend: inmemory, sqlite, postgres, all
coverage backend='inmemory':
    #!/usr/bin/env bash
    case "{{ backend }}" in
        inmemory)
            cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm
            ;;
        sqlite)
            TEST_BACKEND=sqlite cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm
            ;;
        postgres)
            CONTAINER_NAME="eidetica-coverage-postgres"
            DB_NAME="eidetica_test"
            DB_PORT="54322"
            docker rm -f "$CONTAINER_NAME" 2>/dev/null || true
            trap "docker rm -f $CONTAINER_NAME 2>/dev/null || true" EXIT
            echo "Starting PostgreSQL container..."
            docker run -d --name "$CONTAINER_NAME" \
                -e POSTGRES_DB="$DB_NAME" \
                -e POSTGRES_HOST_AUTH_METHOD=trust \
                -p "$DB_PORT":5432 \
                postgres:16-alpine
            echo "Waiting for PostgreSQL to be ready..."
            for i in $(seq 1 30); do
                if docker exec "$CONTAINER_NAME" pg_isready -U postgres -d "$DB_NAME" >/dev/null 2>&1; then
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
            just coverage inmemory
            mv coverage/lcov.info coverage/lcov-inmemory.info
            just coverage sqlite
            mv coverage/lcov.info coverage/lcov-sqlite.info
            just coverage postgres
            mv coverage/lcov.info coverage/lcov-postgres.info
            echo "Merging coverage reports..."
            lcov -a coverage/lcov-inmemory.info -a coverage/lcov-sqlite.info -a coverage/lcov-postgres.info -o coverage/lcov.info
            echo "Merged coverage report: coverage/lcov.info"
            ;;
        ignored)
            cargo tarpaulin --workspace --skip-clean --all-features --output-dir coverage --out lcov --engine llvm --ignored --no-fail-fast
            ;;
        *)
            echo "Unknown backend: {{ backend }}"
            echo "Options: inmemory, sqlite, postgres, all, ignored"
            exit 1
            ;;
    esac

# =============================================================================
# CI
# =============================================================================

# Run CI locally: local (default), full (containers), nix
ci mode='local':
    #!/usr/bin/env bash
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
            just nix build
            just nix check
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

# Nix commands: build, check, integration, full
nix action='check':
    #!/usr/bin/env bash
    case "{{ action }}" in
        build)
            nix build
            ;;
        check)
            nix-fast-build --no-link
            ;;
        integration)
            nix build .#integration-nixos --no-link
            nix build .#integration-container --no-link
            ;;
        full)
            just nix check
            just nix integration
            ;;
        *)
            echo "Unknown action: {{ action }}"
            echo "Options: build, check, integration, full"
            exit 1
            ;;
    esac

# =============================================================================
# Container
# =============================================================================

# Build container image: docker, nix
container type='docker':
    #!/usr/bin/env bash
    case "{{ type }}" in
        docker)
            docker build -t eidetica:dev .
            ;;
        nix)
            nix build .#eidetica-image
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
    cargo bench --workspace
    xdg-open target/criterion/report/index.html 2>/dev/null || true

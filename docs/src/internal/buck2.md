# Buck2 build

Buck2 is an independent, parallel Rust build: its rules invoke `rustc`/`rustdoc` and native C tools directly, not `cargo build` or `cargo test`. Cargo remains the source of workspace manifests and the lockfile, and is still the Nix/CI build. The generated `third-party/BUCK` graph comes from Reindeer; checked-in `BUCK` files in `crates/` and `examples/` own the first-party targets. The current platform is Linux x86_64; other targets need their own native-toolchain and fixup verification.

## Tools and commands

Obtain tools with Nix, outside **or** inside `nix develop`:

```sh
nix shell nixpkgs#buck2 nixpkgs#rustc nixpkgs#clang nixpkgs#lld nixpkgs#binutils
./toolchains/buck2-local build //crates/lib:eidetica //crates/bin:eidetica //examples/chat:chat //examples/todo:todo
./toolchains/buck2-local test //crates/lib:unit //crates/lib:it //crates/bin:unit //crates/bin:reset '//crates/book-tests:book[doc]'
./toolchains/buck2-local run //crates/bin:eidetica -- --help
```

`buck2-local` supplies absolute C compiler and archiver paths from `PATH`: Buck's bundled demo toolchain otherwise gives build-script shims bare command names that cannot be executed by their `execve` wrapper. It does not invoke Cargo. The CLI binary and examples use the current workspace version (and author/description where Clap requires them) in their BUCK `env`; update those values when the Cargo workspace version changes. `//crates/lib:eidetica_testing` is a private test-only variant; the CLI and examples depend on the production `//crates/lib:eidetica`, which does **not** enable `testing`.

To refresh dependency rules after changing Cargo manifests/lockfile, install Reindeer and Cargo with Nix, then run both generators from the repository root:

```sh
nix shell nixpkgs#reindeer nixpkgs#cargo nixpkgs#rustc
reindeer buckify
reindeer -c reindeer-tests.toml buckify
```

The normal-dependency graph uses the root Cargo workspace. Reindeer excludes dev-only dependencies, so `third-party/tests/Cargo.toml` defines a separate small workspace containing `ctor` and `tempfile`, without changing the production Cargo members; its lockfile and BUCK graph are checked in separately. Review generated changes and each newly reachable build script: a warning from the entire lockfile is not a reason to enable every script. Package-specific fixups under `third-party/fixups/` and `third-party/tests/fixups/` document the required generated cfg, OUT_DIR file, or native archive. In particular, `ring` and bundled SQLite require native C/assembly compilation **and** link flags; skipping their scripts can appear to compile libraries yet fail at executable link time.

Reindeer-generated `http_archive` targets fetch verified crates from crates.io at Buck build time. This is suitable for a developer build, **not** a hermetic Nix derivation: Nix sandbox builds need vendored/prefetched archives before Buck can replace Cargo in a Nix package. Running Buck inside `nix develop` uses the same checked-in graph but is not itself a hermetic package build. The Nix flake and its full CI gate deliberately remain Cargo-based.

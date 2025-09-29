# Eidetica Documentation

This directory contains the mdbook-based documentation for Eidetica. Documentation is built and hosted by GitHub Pages on every commit to the main branch.

View the documentation online: https://arcuru.github.io/eidetica/

## Building and Testing

- **Build docs**: `task book` or `mdbook build docs`
- **Serve docs**: `task book:serve` or `mdbook serve docs --open`
- **Test code examples**: `task book:test`

## Code Examples in Documentation

### Compilable Examples

The documentation includes compilable examples using the actual Eidetica API. This provides:

- Examples that stay current with API changes
- Full Rust compiler validation
- Accurate usage patterns for library consumers

### How It Works

The documentation testing system works as follows:

1. Examples use `extern crate eidetica;` to access the real library
2. `task book:test` builds eidetica with all features enabled
3. mdbook uses `-L target/debug/deps` to find compiled dependencies
4. Build process ensures only one eidetica configuration exists

### Build Process

```bash
task book:test
├── rm -f target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
├── cargo build -p eidetica --features full    # Builds single eidetica configuration
└── mdbook test docs -L target/debug/deps      # Tests examples against built library
```

This approach builds the eidetica library with a consistent feature set, avoiding "multiple candidates" errors from workspace builds.

### Testing Strategy

- Code blocks marked with ` ```rust ` are compiled and validated
- Code blocks marked with ` ```rust,ignore ` are shown but not tested

This allows testing critical examples while showing complex scenarios for illustration.

### Integration Points

Documentation examples are validated in:

- Local development via `task test`
- GitHub Actions CI for all pull requests
- Nix flake checks
- Documentation deployment

## Writing Documentation Examples

### Tested Examples

Template for examples that should be compiled and validated (it might be best to view these unrendered):

```rust
# extern crate eidetica;
# use eidetica::{backend::database::InMemory, Instance};
#
# fn create_database() -> eidetica::Result<()> {
// Create an in-memory database
let database = InMemory::new();
let _db = Instance::new(Box::new(database));

// Your example code here
# Ok(())
# }
```

### Illustration Examples

Template for examples that demonstrate concepts but can't be tested due to external requirements:

<!-- Code block ignored: Requires network connectivity for peer synchronization -->

```rust,ignore
use eidetica::{Instance, backend::database::InMemory};

// Start a sync server
let mut sync = instance.sync();
sync.start_server("0.0.0.0:8080").await?;

// Connect from another instance on a different machine
let mut client_sync = client_instance.sync();
client_sync.sync_with_peer("server.example.com:8080", None).await?;

// Data automatically synchronizes between peers
```

### Guidelines

1. **Test core API usage examples** - Make examples testable whenever possible
2. **Hide setup code in tested examples** - Use `#` prefix for imports, extern crate, function signatures, and boilerplate that aren't relevant to the concept being taught
3. **Focus on concepts** - Show the important API calls and configuration patterns, hiding irrelevant scaffolding
4. **Add explanations for ignored blocks** - Always use `<!-- Code block ignored: reason -->` comments before ` ```rust,ignore ` blocks
5. **Keep tested examples simple** - One concept per example, minimal external dependencies
6. **Use proper error handling** - Return `eidetica::Result<()>` and handle errors appropriately
7. **Follow CLAUDE.md guidelines** - See ../CLAUDE.md for detailed documentation standards

### Testing Changes

- Test all examples: `task book:test`
- Test during development: `task test` (includes book tests)
- Local preview: `task book:serve`
- CI validation: Examples tested automatically in pull requests

### Troubleshooting

If book tests fail:

1. Check imports use valid module paths from the eidetica crate
2. Verify examples have proper `extern crate` declarations
3. Run `task book:test` separately to isolate issues
4. Some features may require specific feature flags

Tested examples use the real API directly, ensuring documentation stays accurate as the codebase evolves.

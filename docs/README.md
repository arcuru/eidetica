# Eidetica Documentation

This directory contains the mdbook-based documentation for Eidetica. Documentation is built and hosted by GitHub Pages on every commit to the main branch.

View the documentation online: https://arcuru.github.io/eidetica/

## Building and Testing

- **Build docs**: `just doc book` or `mdbook build docs`
- **Serve docs**: `just doc serve` or `mdbook serve docs --open`
- **Test code examples**: `just doc test`

## Code Examples in Documentation

### Compilable Examples

The documentation includes compilable examples using the actual Eidetica API. This provides:

- Examples that stay current with API changes
- Full Rust compiler validation
- Accurate usage patterns for library consumers

### How It Works

The documentation testing system works as follows:

1. Examples use `extern crate eidetica;` to access the real library
2. `just doc test` builds eidetica with all features enabled
3. mdbook uses `-L target/debug/deps` to find compiled dependencies
4. Build process ensures only one eidetica configuration exists

### Build Process

```bash
just doc test
├── rm -f target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
├── cargo build -p eidetica                    # Builds single eidetica configuration
└── mdbook test docs -L target/debug/deps      # Tests examples against built library
```

### Testing Strategy

- Code blocks marked with ` ```rust ` are compiled and validated
- Code blocks marked with ` ```rust,ignore ` are shown but not tested

This allows testing critical examples while showing complex scenarios for illustration.

### Integration Points

Documentation examples are validated in:

- Local development via `just test`
- GitHub Actions CI for all pull requests
- Nix flake checks
- Documentation deployment

## Writing Documentation Examples

### Tested Examples

Template for examples that should be compiled and validated (it might be best to view these unrendered):

```rust
# extern crate eidetica;
# use eidetica::{backend::database::Sqlite, Instance};
#
# fn create_database() -> eidetica::Result<()> {
// Create an in-memory SQLite database
let database = Sqlite::in_memory()?;
let _db = Instance::new(Box::new(database));

// Your example code here
# Ok(())
# }
```

### Illustration Examples

Template for examples that demonstrate concepts but can't be tested due to external requirements:

<!-- Code block ignored: Requires network connectivity for peer synchronization -->

```rust,ignore
use eidetica::{Instance, backend::database::Sqlite};

// Start a sync server
let sync = instance.sync().unwrap();
sync.register_transport("http", HttpTransport::builder().bind("0.0.0.0:8080")).await?;
sync.accept_connections().await?;

// Connect from another instance on a different machine
let client_sync = client_instance.sync().unwrap();
client_sync.register_transport("http", HttpTransport::builder()).await?;
client_sync.sync_with_peer(&Address::http("server.example.com:8080"), None).await?;

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

## Writing Style

### Describe Current State Only

Documentation should describe what the system does now, not how it got there:

- **Good**: "The sync system uses a command pattern for async operations"
- **Bad**: "The sync system has been updated to use a simpler command pattern"

### Avoid Change Language

Never use words that imply historical changes:

- ❌ "simplified", "improved", "changed", "updated", "now", "new"
- ❌ "Before X, the system had no way to..."
- ❌ "This was added to solve..."

### Be Direct

State what components do and how they work:

- **Good**: "BackgroundSync uses direct sync tree access for peer data"
- **Bad**: "BackgroundSync now uses simplified direct access instead of commands"

### Focus on Behavior

Document the current architecture, APIs, and behavior patterns without historical context. Readers don't need to know what the system used to do—they need to know what it does.

### Testing Changes

- Test all examples: `just doc test`
- Test during development: `just test` (includes book tests)
- Local preview: `just doc serve`
- CI validation: Examples tested automatically in pull requests

### Troubleshooting

If book tests fail:

1. Check imports use valid module paths from the eidetica crate
2. Verify examples have proper `extern crate` declarations
3. Run `just doc test` separately to isolate issues
4. Some features may require specific feature flags

Tested examples use the real API directly, ensuring documentation stays accurate as the codebase evolves.

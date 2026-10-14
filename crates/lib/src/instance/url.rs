//! Parser for eidetica connection URLs.
//!
//! Eidetica has one connection-string entry point that dispatches across all
//! supported backends and the daemon socket:
//!
//! - `sqlite://./app.db` — embedded eidetica with the sqlite backend; URL is
//!   handed through to `sqlx::sqlite` unchanged after the scheme check, so
//!   any sqlx-accepted form works (relative path, `?mode=rwc&journal_mode=WAL`,
//!   etc.).
//! - `postgres://user:pwd@host/db` — embedded eidetica with the postgres
//!   backend; URL is handed through to `sqlx::postgres` unchanged.
//! - `unix:///absolute/socket/path` — thin client to a running eidetica
//!   daemon. Path must be absolute; query strings and fragments are
//!   rejected.
//! - `memory://` — ephemeral in-process backend.
//! - `memory:///absolute/path/snapshot.json` — in-process backend with a
//!   JSON snapshot file (load-on-start, snapshot on `Instance::flush` /
//!   best-effort on `Drop`).
//!
//! Schemes are matched case-insensitively per RFC 3986 (`SQLITE://`,
//! `Sqlite://`, and `sqlite://` are all accepted). The scheme portion of
//! the URL passed through to sqlx is normalised to lowercase so backends
//! that demand a lowercase scheme don't reject it.
//!
//! `sqlite:` and `postgres:`/`postgresql:` also accept the single-colon
//! URI form (`sqlite:file::memory:?cache=shared`, `sqlite:./app.db`) that
//! sqlx natively understands. This is required for sqlx's in-memory URLs:
//! the `://` variant triggers URL authority parsing in sqlx and rejects
//! `:memory:` as a port number. `unix:` and `memory:` keep the strict
//! `://` requirement so their typo hints still fire on `unix:/run/sock`.
//!
//! Schemes that aren't recognised return [`InstanceError::UnsupportedScheme`]
//! with a typo hint when possible (`mysql://` → suggests `postgres://`).
//! URLs missing the `scheme://` separator return [`InstanceError::InvalidUrl`]
//! with a hint guessing at `sqlite://`.

use std::path::PathBuf;

use crate::Result;

use super::errors::InstanceError;

/// Parsed connection URL.
///
/// Schemes that delegate to sqlx (`sqlite`, `postgres`) carry the original
/// URL string and let sqlx do the detailed parsing — this keeps eidetica from
/// shadowing sqlx's `?param` surface and matches the user-facing expectation
/// of "the URL after the scheme prefix means whatever sqlx says it means."
#[derive(Debug, Clone)]
pub(crate) enum ConnectionUrl {
    /// `sqlite://...` — full URL handed through to sqlx
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    Sqlite { url: String },
    /// `postgres://...` — full URL handed through to sqlx
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    Postgres { url: String },
    /// `unix:///path/to/sock` — absolute socket path.
    #[cfg_attr(not(all(unix, feature = "service")), allow(dead_code))]
    Unix { socket_path: PathBuf },
    /// `memory://` or `memory:///path/to/snapshot.json`.
    Memory { snapshot_path: Option<PathBuf> },
}

/// Parse an eidetica connection URL.
///
/// Returns a structured [`ConnectionUrl`] that the `Instance` dispatcher can
/// switch on. Errors are [`InstanceError::InvalidUrl`] for malformed input
/// (with a hint where possible) and [`InstanceError::UnsupportedScheme`] for
/// recognised-but-typoed schemes (e.g. `mysql://`).
pub(crate) fn parse(url: &str) -> Result<ConnectionUrl> {
    if url.is_empty() {
        return Err(InstanceError::InvalidUrl {
            url: String::new(),
            reason: "URL is empty; expected something like `sqlite://./app.db`, \
                     `postgres://user@host/db`, `unix:///run/eidetica/sock`, or `memory://`"
                .into(),
        }
        .into());
    }

    let Some((scheme_raw, rest, sep)) = split_scheme(url) else {
        return Err(missing_scheme_error(url));
    };

    // RFC 3986: schemes are case-insensitive. Normalise to lowercase and
    // rebuild so sqlx sees a lowercase prefix. The `rest` is left
    // untouched (paths and query strings are case-sensitive). Preserve
    // whichever separator the caller used — `sqlite:` and `sqlite://`
    // aren't interchangeable for sqlx's in-memory URLs.
    let scheme = scheme_raw.to_ascii_lowercase();
    let normalised = format!("{scheme}{sep}{rest}");

    match scheme.as_str() {
        "sqlite" => Ok(ConnectionUrl::Sqlite { url: normalised }),
        "postgres" | "postgresql" => Ok(ConnectionUrl::Postgres { url: normalised }),
        "unix" => parse_unix(url, rest),
        "memory" => parse_memory(url, rest),
        // Common typos / unrelated DB URLs people might paste in.
        "mysql" | "mariadb" => unsupported(scheme, Some("postgres")),
        "tcp" | "http" | "https" | "ws" | "wss" => unsupported(scheme, Some("unix")),
        "file" => unsupported(scheme, Some("sqlite")),
        _ => unsupported(scheme, None),
    }
}

/// Split `url` into `(scheme, rest, separator)`.
///
/// Prefers `scheme://...` (the standard form for all four backends). Falls
/// back to single-colon `scheme:...` only for sqlx-backed schemes — sqlx
/// accepts both prefixes natively and needs the single-colon form for its
/// in-memory URLs (`sqlite:file::memory:?cache=shared`). `unix:` and
/// `memory:` intentionally don't get the fallback so their slash-typo
/// hints (`unix:/run/sock`) keep firing.
fn split_scheme(url: &str) -> Option<(&str, &str, &'static str)> {
    if let Some((s, r)) = url.split_once("://") {
        return Some((s, r, "://"));
    }
    let (s, r) = url.split_once(':')?;
    matches!(
        s.to_ascii_lowercase().as_str(),
        "sqlite" | "postgres" | "postgresql"
    )
    .then_some((s, r, ":"))
}

fn missing_scheme_error(url: &str) -> crate::Error {
    // Common typo: `unix:/path` (single slash) or just a bare path.
    // Match case-insensitively so `UNIX:/path` still gets the hint.
    let lower = url.to_ascii_lowercase();
    let hint = if let Some(stripped) = lower.strip_prefix("unix:") {
        format!(
            "`unix://` requires two slashes plus an absolute path; did you mean `unix://{stripped}`?"
        )
    } else if url.starts_with('/') || url.starts_with("./") || url.ends_with(".db") {
        format!("missing scheme; did you mean `sqlite://{url}`?")
    } else {
        "URL is missing the `scheme://` separator (expected `sqlite://`, `postgres://`, \
         `unix://`, or `memory://`)"
            .to_string()
    };
    InstanceError::InvalidUrl {
        url: url.to_string(),
        reason: hint,
    }
    .into()
}

fn unsupported(scheme: String, suggested: Option<&'static str>) -> Result<ConnectionUrl> {
    Err(InstanceError::UnsupportedScheme { scheme, suggested }.into())
}

/// Shared validation for the absolute-path schemes (`unix://`, `memory://`).
///
/// Rejects query strings and fragments — both schemes describe a local path
/// and have no use for HTTP-style decorations.
fn reject_query_or_fragment(scheme: &'static str, original: &str, rest: &str) -> Result<()> {
    if rest.contains('?') {
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: format!("`{scheme}://` does not support query strings"),
        }
        .into());
    }
    if rest.contains('#') {
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: format!("`{scheme}://` does not support fragments"),
        }
        .into());
    }
    Ok(())
}

fn parse_unix(original: &str, rest: &str) -> Result<ConnectionUrl> {
    // `unix://` only describes a local absolute path to a Unix socket file.
    // No hostname, no query, no fragment.
    if rest.is_empty() {
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: "`unix://` requires an absolute socket path (e.g. \
                     `unix:///run/eidetica/service.sock`)"
                .into(),
        }
        .into());
    }
    if !rest.starts_with('/') {
        // Ambiguous: `unix://run/sock` could be either a slash typo (meant
        // `unix:///run/sock`) or an RFC-3986-style attempt at an authority
        // (meant `unix:///sock`, treating `run` as a hostname). Unix
        // sockets have no hostname concept — the kernel identifies them
        // by filesystem path — so we surface both interpretations and
        // explain rather than guess.
        let forgot_slash_hint = format!("unix:///{rest}");
        let dropped_host_hint = match rest.split_once('/') {
            Some((_, after)) if !after.is_empty() => format!("unix:///{after}"),
            _ => "unix:///path/to/sock".to_string(),
        };
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: format!(
                "`unix://` path must be absolute (start with `/`); got `{rest}`. \
                 Two common causes: (1) a slash typo — if you meant the socket file \
                 at `/{rest}`, use `{forgot_slash_hint}`; (2) treating `unix://` like \
                 an HTTP URL with an authority — Unix sockets have no hostname (the \
                 kernel identifies them by filesystem path only), so drop the host \
                 segment and use `{dropped_host_hint}` instead."
            ),
        }
        .into());
    }
    reject_query_or_fragment("unix", original, rest)?;
    Ok(ConnectionUrl::Unix {
        socket_path: PathBuf::from(rest),
    })
}

fn parse_memory(original: &str, rest: &str) -> Result<ConnectionUrl> {
    // `memory://` (no path) → ephemeral.
    // `memory:///absolute/path.json` → load-on-start + snapshot target.
    if rest.is_empty() {
        return Ok(ConnectionUrl::Memory {
            snapshot_path: None,
        });
    }
    if !rest.starts_with('/') {
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: format!(
                "`memory://` snapshot path must be absolute (start with `/`); got `{rest}`. \
                 Use `memory://` for ephemeral state, or `memory:///{rest}` for a snapshot path."
            ),
        }
        .into());
    }
    // `memory:///` and `memory:////...` aren't usable snapshot targets —
    // the path resolves to `/` (a directory) and the I/O failure later
    // would be cryptic. Reject up front.
    if rest == "/" || rest.starts_with("//") {
        return Err(InstanceError::InvalidUrl {
            url: original.to_string(),
            reason: "`memory://` snapshot path must name a file (e.g. \
                     `memory:///var/lib/eidetica/snap.json`); got an empty or root path. \
                     Use `memory://` for an ephemeral in-memory instance with no snapshot."
                .into(),
        }
        .into());
    }
    reject_query_or_fragment("memory", original, rest)?;
    Ok(ConnectionUrl::Memory {
        snapshot_path: Some(PathBuf::from(rest)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sqlite() {
        match parse("sqlite://./app.db").unwrap() {
            ConnectionUrl::Sqlite { url } => assert_eq!(url, "sqlite://./app.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn parses_sqlite_single_colon_uri_form() {
        // sqlx's native in-memory URL uses single-colon URI form because the
        // `://` variant triggers URL authority parsing in sqlx and rejects
        // `:memory:` as an invalid port. We pass these through unchanged.
        match parse("sqlite:file::memory:?cache=shared").unwrap() {
            ConnectionUrl::Sqlite { url } => {
                assert_eq!(url, "sqlite:file::memory:?cache=shared")
            }
            other => panic!("expected sqlite, got {other:?}"),
        }
        match parse("sqlite:./app.db").unwrap() {
            ConnectionUrl::Sqlite { url } => assert_eq!(url, "sqlite:./app.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
        match parse("postgres:user@host/db").unwrap() {
            ConnectionUrl::Postgres { url } => assert_eq!(url, "postgres:user@host/db"),
            other => panic!("expected postgres, got {other:?}"),
        }
        // Case-insensitive scheme even on the single-colon form.
        match parse("SQLITE:file::memory:?cache=shared").unwrap() {
            ConnectionUrl::Sqlite { url } => {
                assert_eq!(url, "sqlite:file::memory:?cache=shared")
            }
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn parses_postgres_and_postgresql_aliases() {
        match parse("postgres://u@h/db").unwrap() {
            ConnectionUrl::Postgres { url } => assert_eq!(url, "postgres://u@h/db"),
            other => panic!("expected postgres, got {other:?}"),
        }
        match parse("postgresql://u@h/db").unwrap() {
            ConnectionUrl::Postgres { url } => assert_eq!(url, "postgresql://u@h/db"),
            other => panic!("expected postgres, got {other:?}"),
        }
    }

    #[test]
    fn scheme_is_case_insensitive() {
        // Upper- and mixed-case scheme names should match and be
        // normalised to lowercase in the URL handed through to sqlx.
        match parse("SQLITE://./app.db").unwrap() {
            ConnectionUrl::Sqlite { url } => assert_eq!(url, "sqlite://./app.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
        match parse("Postgres://u@h/db").unwrap() {
            ConnectionUrl::Postgres { url } => assert_eq!(url, "postgres://u@h/db"),
            other => panic!("expected postgres, got {other:?}"),
        }
        match parse("UNIX:///run/sock").unwrap() {
            ConnectionUrl::Unix { socket_path } => {
                assert_eq!(socket_path, PathBuf::from("/run/sock"));
            }
            other => panic!("expected unix, got {other:?}"),
        }
        match parse("Memory://").unwrap() {
            ConnectionUrl::Memory { snapshot_path } => assert!(snapshot_path.is_none()),
            other => panic!("expected memory, got {other:?}"),
        }
    }

    #[test]
    fn path_case_is_preserved_when_scheme_is_lowercased() {
        // The scheme is case-insensitive (RFC 3986) but paths, hostnames,
        // query strings, and filenames are case-sensitive on most
        // filesystems and on the wire. Only the scheme prefix should
        // change when we normalise.
        match parse("SQLITE://./MyApp.DB").unwrap() {
            ConnectionUrl::Sqlite { url } => assert_eq!(url, "sqlite://./MyApp.DB"),
            other => panic!("expected sqlite, got {other:?}"),
        }
        // Single-colon URI form: same rule — only the scheme prefix
        // changes, the rest passes through verbatim.
        match parse("SQLITE:file:Mixed-Case.db?Cache=Shared").unwrap() {
            ConnectionUrl::Sqlite { url } => {
                assert_eq!(url, "sqlite:file:Mixed-Case.db?Cache=Shared")
            }
            other => panic!("expected sqlite, got {other:?}"),
        }
        match parse("Postgres://User:Pass@Host.Example/MyDB").unwrap() {
            ConnectionUrl::Postgres { url } => {
                assert_eq!(url, "postgres://User:Pass@Host.Example/MyDB")
            }
            other => panic!("expected postgres, got {other:?}"),
        }
        match parse("UNIX:///Run/MyDaemon.SOCK").unwrap() {
            ConnectionUrl::Unix { socket_path } => {
                assert_eq!(socket_path, PathBuf::from("/Run/MyDaemon.SOCK"));
            }
            other => panic!("expected unix, got {other:?}"),
        }
        match parse("MEMORY:///Var/Lib/MyApp/Snap.JSON").unwrap() {
            ConnectionUrl::Memory { snapshot_path } => {
                assert_eq!(
                    snapshot_path,
                    Some(PathBuf::from("/Var/Lib/MyApp/Snap.JSON"))
                );
            }
            other => panic!("expected memory, got {other:?}"),
        }
    }

    #[test]
    fn parses_unix_absolute_path() {
        match parse("unix:///run/eidetica/sock").unwrap() {
            ConnectionUrl::Unix { socket_path } => {
                assert_eq!(socket_path, PathBuf::from("/run/eidetica/sock"));
            }
            other => panic!("expected unix, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unix_relative_path() {
        // The hint should surface both interpretations: the slash-typo
        // form (`unix:///run/sock`) and the dropped-host form
        // (`unix:///sock`). Either could be what the user meant.
        let err = parse("unix://run/sock").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unix:///run/sock"), "{msg}");
        assert!(msg.contains("unix:///sock"), "{msg}");
        assert!(msg.contains("no hostname"), "{msg}");
    }

    #[test]
    fn rejects_unix_relative_no_subpath() {
        // `unix://host` has no `/` after the host, so the dropped-host
        // suggestion falls back to a generic placeholder.
        let err = parse("unix://host").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unix:///host"), "{msg}");
        assert!(msg.contains("/path/to/sock"), "{msg}");
    }

    #[test]
    fn rejects_unix_empty_path() {
        let err = parse("unix://").unwrap_err();
        assert!(format!("{err}").contains("requires an absolute"), "{err}");
    }

    #[test]
    fn rejects_unix_query_and_fragment() {
        assert!(parse("unix:///s?foo=bar").is_err());
        assert!(parse("unix:///s#frag").is_err());
    }

    #[test]
    fn parses_memory_ephemeral() {
        match parse("memory://").unwrap() {
            ConnectionUrl::Memory { snapshot_path } => assert!(snapshot_path.is_none()),
            other => panic!("expected memory, got {other:?}"),
        }
    }

    #[test]
    fn parses_memory_with_snapshot_path() {
        match parse("memory:///var/lib/snap.json").unwrap() {
            ConnectionUrl::Memory { snapshot_path } => {
                assert_eq!(snapshot_path, Some(PathBuf::from("/var/lib/snap.json")));
            }
            other => panic!("expected memory, got {other:?}"),
        }
    }

    #[test]
    fn rejects_memory_relative_snapshot() {
        let err = parse("memory://./snap.json").unwrap_err();
        assert!(format!("{err}").contains("absolute"), "{err}");
    }

    #[test]
    fn rejects_memory_root_snapshot() {
        // `memory:///` resolves to `/` — a directory, not a snapshot file.
        let err = parse("memory:///").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must name a file"), "{msg}");
        // The double-slash form is a common mistake; reject the same way.
        let err = parse("memory:////etc/passwd").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must name a file"), "{msg}");
    }

    #[test]
    fn empty_url_errors_with_hint() {
        let err = parse("").unwrap_err();
        assert!(format!("{err}").contains("sqlite://"), "{err}");
    }

    #[test]
    fn missing_scheme_hints_at_sqlite() {
        let err = parse("./app.db").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sqlite://"), "{msg}");
    }

    #[test]
    fn unix_single_slash_hints_at_double() {
        let err = parse("unix:/run/sock").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unix://"), "{msg}");
    }

    #[test]
    fn unix_single_slash_uppercase_still_hints() {
        // The missing-scheme hint should match `UNIX:` case-insensitively.
        let err = parse("UNIX:/run/sock").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unix://"), "{msg}");
    }

    #[test]
    fn mysql_suggests_postgres() {
        let err = parse("mysql://u@h/db").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("postgres"), "{msg}");
    }

    #[test]
    fn file_suggests_sqlite() {
        let err = parse("file:///app.db").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sqlite"), "{msg}");
    }

    #[test]
    fn tcp_suggests_unix() {
        let err = parse("tcp://host:1234").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unix"), "{msg}");
    }
}

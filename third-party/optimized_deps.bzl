"""Match Cargo's optimized third-party dev dependencies without optimizing first-party code."""

load("@prelude//rust:cargo_package.bzl", upstream_cargo = "cargo")

def _rust_library(**kwargs):
    kwargs["rustc_flags"] = ["-Copt-level=3"] + kwargs.get("rustc_flags", [])
    upstream_cargo.rust_library(**kwargs)

cargo = struct(
    rust_binary = upstream_cargo.rust_binary,
    rust_library = _rust_library,
)

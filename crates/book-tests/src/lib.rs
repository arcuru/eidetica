#![allow(non_snake_case)]
// This crate exists solely to test code examples in the mdbook documentation.
//
// The build.rs auto-discovers all .md files under docs/src/ and generates
// modules with `#[doc = include_str!("...")]` for each one. Running
// `cargo test --doc -p eidetica-book-tests` compiles and runs every
// fenced Rust code block through cargo's normal dependency resolution,
// avoiding the rlib collision issues that plague `mdbook test -L`.

include!(concat!(env!("OUT_DIR"), "/book_doctests.rs"));

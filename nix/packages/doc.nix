# Documentation packages
{
  craneLib,
  debugArgs,
  pkgs,
  lib,
}: let
  # Source filtered to include only Rust/cargo files + docs directory
  # This prevents rebuilds when unrelated files (like nix/) change
  docsFilter = path: _type: builtins.match ".*/docs/.*" path != null;
  docsOrCargo = path: type:
    !(type == "directory" && builtins.baseNameOf path == ".config")
    && ((docsFilter path type) || (craneLib.filterCargoSources path type));
  docSrc = lib.cleanSourceWith {
    src = ../..;
    filter = docsOrCargo;
  };

  # TODO: --workspace docs are broken: eidetica-bin's binary is also named "eidetica", so its docs overwrite the library's in the output directory
  doc-api = craneLib.cargoDoc (debugArgs
    // {
      cargoDocExtraArgs = "-p eidetica --all-features --no-deps";
    });

  # API docs with dev banner for deployed documentation
  # Uses docSrc to include docs/rustdoc-header.html (filtered out by cleanCargoSource)
  doc-api-dev = craneLib.cargoDoc (debugArgs
    // {
      pname = "doc-api-dev";
      src = docSrc;
      cargoDocExtraArgs = "-p eidetica --all-features --no-deps";
      RUSTDOCFLAGS = "--html-in-header docs/rustdoc-header.html";
    });

  # Full docs including dependencies (slow, ~20min uncached)
  doc-api-full = craneLib.cargoDoc (debugArgs
    // {
      pname = "doc-full";
      cargoDocExtraArgs = "--workspace --all-features";
    });

  # Combined site: mdbook + rustdoc
  mkSite = api:
    pkgs.runCommand "doc-site" {} ''
      cp -r ${doc-book} $out
      chmod -R u+w $out
      mkdir -p $out/rustdoc
      cp -r ${api}/share/doc/* $out/rustdoc/
    '';

  doc-site = mkSite doc-api;
  doc-site-dev = mkSite doc-api-dev;

  # Common lychee args for link checking
  # --exclude-path: skip directories with internal cross-references that break outside their original context
  #   rustdoc: internal links (help.html → index.html) are valid in cargo doc output but break in our site layout
  #   404.html: contains <base href="/"> which lychee can't resolve offline
  #   fonts: binary woff2 files
  # mdbook → rustdoc cross-links are still checked since they're in the mdbook HTML, not excluded paths
  lycheeArgs = "--exclude-path 'rustdoc' --exclude-path '404.html' --exclude-path 'fonts'";

  doc-links =
    pkgs.runCommand "doc-links" {
      nativeBuildInputs = [pkgs.lychee pkgs.cacert];
    } ''
      lychee --offline ${lycheeArgs} ${doc-site}
      mkdir -p $out
      echo "Link check passed" > $out/result
    '';

  doc-links-online-runner = pkgs.writeShellApplication {
    name = "doc-links-online";
    runtimeInputs = [pkgs.lychee pkgs.cacert];
    text = ''
      lychee ${lycheeArgs} ${doc-site}
    '';
  };

  doc-book =
    pkgs.runCommand "book" {
      nativeBuildInputs = [pkgs.mdbook pkgs.mdbook-mermaid];
      src = docSrc;
    } ''
      cd $src
      mdbook build docs -d $out
    '';

  # All doc tests: both Rust doc comments and mdbook code examples.
  # Uses docSrc (includes docs/ directory) so the book-tests crate can
  # find the markdown files. The book-tests crate auto-discovers all
  # markdown files in docs/src/ and tests their fenced code blocks through
  # cargo's dependency resolution, avoiding E0464 "multiple rlib candidates"
  # errors that plague `mdbook test -L`.
  doc-test = craneLib.cargoTest (debugArgs
    // {
      src = docSrc;
      cargoTestExtraArgs = "--doc --workspace --all-features";
    });
in {
  builds = {
    api = doc-api;
    api-dev = doc-api-dev;
    api-full = doc-api-full;
    site = doc-site;
    site-dev = doc-site-dev;
    links = doc-links;
    test = doc-test;
    book = doc-book;
  };

  runners = {
    links-online = doc-links-online-runner;
  };

  # Fast doc checks for CI
  defaults = {
    api = doc-api;
    test = doc-test;
    links = doc-links;
  };
}

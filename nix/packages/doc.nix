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
    (docsFilter path type) || (craneLib.filterCargoSources path type);
  docSrc = lib.cleanSourceWith {
    src = ../..;
    filter = docsOrCargo;
  };

  doc-api = craneLib.cargoDoc (debugArgs
    // {
      cargoDocExtraArgs = "--workspace --all-features --no-deps";
    });

  # Full docs including dependencies (slow, ~20min uncached)
  doc-api-full = craneLib.cargoDoc (debugArgs
    // {
      pname = "doc-full";
      cargoDocExtraArgs = "--workspace --all-features";
    });

  doc-links =
    pkgs.runCommand "doc-links" {
      nativeBuildInputs = [pkgs.mdbook pkgs.mdbook-mermaid pkgs.lychee];
      src = docSrc;
    } ''
      cd $src
      mdbook build docs -d $TMPDIR/book
      lychee --offline --exclude-path 'rustdoc' $TMPDIR/book
      mkdir -p $out
      echo "Link check passed" > $out/result
    '';

  doc-book =
    pkgs.runCommand "book" {
      nativeBuildInputs = [pkgs.mdbook pkgs.mdbook-mermaid];
      src = docSrc;
    } ''
      cd $src
      mdbook build docs -d $out
    '';

  doc-book-test = craneLib.mkCargoDerivation (debugArgs
    // {
      pname = "book-test";
      src = docSrc;
      nativeBuildInputs = debugArgs.nativeBuildInputs ++ [pkgs.mdbook];
      buildPhaseCargoCommand = ''
        rm -f target/debug/deps/libeidetica-*.rlib target/debug/deps/libeidetica-*.rmeta
        cargo build -p eidetica
      '';
      doCheck = true;
      checkPhase = ''
        runHook preCheck
        cd docs
        mdbook test . -L ../target/debug/deps
        runHook postCheck
      '';
      doInstallCargoArtifacts = false;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        echo "Documentation examples tested successfully" > $out/result
        runHook postInstall
      '';
    });

  doc-test = craneLib.cargoTest (debugArgs
    // {
      cargoTestExtraArgs = "--doc --workspace --all-features";
    });

  # Fast doc checks for CI (api docs + doc tests + book tests)
  docFast = {
    api = doc-api;
    test = doc-test;
    book-test = doc-book-test;
  };

  # All doc packages
  docPackages = {
    api = doc-api;
    api-full = doc-api-full;
    links = doc-links;
    book = doc-book;
    book-test = doc-book-test;
    test = doc-test;
  };
in {
  inherit doc-api doc-api-full doc-links doc-book doc-book-test doc-test docFast docPackages;
}

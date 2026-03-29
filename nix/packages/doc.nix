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
      nativeBuildInputs = [pkgs.lychee];
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
    booktest = doc-book-test;
  };

  runners = {
    links-online = doc-links-online-runner;
  };

  # Fast doc checks for CI
  defaults = {
    api = doc-api;
    test = doc-test;
    booktest = doc-book-test;
    links = doc-links;
  };
}

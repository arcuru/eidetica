# Documentation packages
{
  craneLib,
  debugArgs,
  releaseArgs,
  noDepsArgs,
  pkgs,
  rootSrc,
}: let
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
      src = rootSrc;
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
      src = rootSrc;
    } ''
      cd $src
      mdbook build docs -d $out
    '';

  # Note: Uses noDepsArgs because it deletes and rebuilds deps anyway
  doc-book-test = craneLib.mkCargoDerivation (noDepsArgs
    // {
      pname = "book-test";
      src = rootSrc; # Needs the docs directory (not just cleanCargoSource)
      nativeBuildInputs = noDepsArgs.nativeBuildInputs ++ [pkgs.mdbook];
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

  doc-test = craneLib.cargoTest (releaseArgs
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

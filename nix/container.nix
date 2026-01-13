# OCI container image configuration
{
  pkgs,
  eidetica-bin,
}: let
  # Non-root user setup for container security
  # Creates passwd/group/shadow files for the eidetica user (UID/GID 1000)
  nonRootUserSetup = let
    user = "eidetica";
    uid = "1000";
    gid = "1000";
  in [
    (pkgs.writeTextDir "etc/passwd" ''
      root:x:0:0:root:/root:/bin/false
      ${user}:x:${uid}:${gid}::/${user}:
    '')
    (pkgs.writeTextDir "etc/group" ''
      root:x:0:
      ${user}:x:${gid}:
    '')
    (pkgs.writeTextDir "etc/shadow" ''
      root:!x:::::::
      ${user}:!:::::::
    '')
  ];

  # License file for container compliance
  licenseFile = pkgs.runCommand "license" {} ''
    mkdir -p $out
    cp ${../LICENSE.txt} $out/LICENSE
  '';

  # OCI container image
  eidetica-image = pkgs.dockerTools.buildImage {
    name = "eidetica";
    tag = "dev";
    created = "now";

    copyToRoot = pkgs.buildEnv {
      name = "image-root";
      paths = [eidetica-bin licenseFile] ++ nonRootUserSetup;
      pathsToLink = ["/bin" "/etc" "/"];
    };

    config = {
      Cmd = ["${eidetica-bin}/bin/eidetica"];
      User = "1000:1000";
      WorkingDir = "/data";
      ExposedPorts = {
        "3000/tcp" = {};
      };
      Volumes = {
        "/data" = {};
      };
      Env = [
        "EIDETICA_DATA_DIR=/data"
        "EIDETICA_HOST=0.0.0.0"
        "HOME=/tmp"
      ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/arcuru/eidetica";
        "org.opencontainers.image.description" = "Eidetica: Remember Everything - Decentralized Database";
        "org.opencontainers.image.licenses" = "AGPL-3.0-or-later";
      };
    };
  };
in {
  inherit eidetica-image;
}

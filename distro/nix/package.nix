{
  lib,
  rustPlatform,
  src,
  version,
}:
let
  source = lib.cleanSourceWith {
    inherit src;
    filter =
      path: type:
      let
        relativePath = lib.removePrefix "${toString src}/" (toString path);
        sourceDirectories = [
          ".cargo"
          "apps"
          "crates"
          "xtask"
        ];
      in
      relativePath == toString src
      || builtins.elem relativePath [
        "Cargo.toml"
        "Cargo.lock"
        "LICENSE"
        "README.md"
      ]
      || builtins.any (
        directory:
        relativePath == directory || lib.hasPrefix "${directory}/" relativePath
      ) sourceDirectories;
  };
in
rustPlatform.buildRustPackage {
  pname = "mcbtodo";
  inherit version;
  src = source;

  cargoLock.lockFile = "${source}/Cargo.lock";
  cargoBuildFlags = [
    "--locked"
    "--workspace"
    "--bin=mcbtodo"
    "--bin=mcbtodo-daemon"
    "--bin=mcbtodo-worker"
  ];

  # The full acceptance suite is run by cargo xtask check in CI. Several tests
  # exercise host-installed Linux sandbox and script-runner tools.
  doCheck = false;

  postInstall = ''
    install -Dm644 "${source}/LICENSE" \
      "$out/share/licenses/mcbtodo/LICENSE"
    install -Dm644 \
      "${source}/apps/mcbtodo-daemon/assets/mcbtodo-daemon.service" \
      "$out/lib/systemd/user/mcbtodo-daemon.service"
  '';

  meta = {
    description = "A local-first task manager with a calendar TUI and workflow automation";
    homepage = "https://github.com/MCB-SMART-BOY/mcbtodo";
    license = lib.licenses.asl20;
    mainProgram = "mcbtodo";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}

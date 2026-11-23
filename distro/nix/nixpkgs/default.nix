{
  pkgs ? import <nixpkgs> { },
  src ? ../../..,
  version ? "0.1.0-pre",
}:
pkgs.callPackage ../package.nix { inherit src version; }

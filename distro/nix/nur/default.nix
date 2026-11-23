{ pkgs }:
{
  mcbtodo = pkgs.callPackage ../package.nix {
    version = "0.1.0-pre";
    src = ../../..;
  };
}

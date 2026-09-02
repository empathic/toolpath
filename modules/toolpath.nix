self:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.toolpath;

  # A package whose bin/path points outside the store, at a working tree's
  # cargo output. Nothing about it changes when the binary behind it does, so
  # `cargo build --release -p path-cli` is the whole iteration loop — no
  # rebuild, no switch, no generation. A development convenience only: the
  # link dangles if the target directory is cleaned away.
  devPackage = pkgs.runCommandLocal "toolpath-dev" { } ''
    mkdir -p $out/bin
    ln -s ${lib.escapeShellArg cfg.devBin} $out/bin/path
  '';
in
{
  options.programs.toolpath = {
    enable = lib.mkEnableOption "the Toolpath provenance CLI (binary: path)";

    package = lib.mkOption {
      type = lib.types.package;
      # `pkgs.stdenv.hostPlatform.system`, not `pkgs.system`: the latter is
      # renamed in nixpkgs and every consumer that imports this module gets an
      # evaluation warning on every build, attributed to their config rather
      # than to this file.
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.toolpath;
      description = "The toolpath package to install (defaults to this flake's build).";
    };

    devBin = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/home/you/src/toolpath/target/release/path";
      description = ''
        Absolute path to a locally built `path` to install instead of
        {option}`programs.toolpath.package`. A string, not a path, so the
        working tree is never copied into the store — what is installed is a
        symlink, and it follows the binary you rebuild.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ (if cfg.devBin == null then cfg.package else devPackage) ];
  };
}

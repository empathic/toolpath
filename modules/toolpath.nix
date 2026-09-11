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

  tomlFormat = pkgs.formats.toml { };
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

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = { };
      example = lib.literalExpression ''
        {
          project = [
            { origin = "acme/widget"; remote = "dev/pathstash"; }
            { dir = "~/src/client"; remote = "dev/client-paths"; }
          ];
        }
      '';
      description = ''
        Contents of `~/.toolpath/config.toml`, rendered from this attrset.
        Left at the default empty set, no file is written and the config
        stays imperative — the option is inert until you set it.

        The schema is toolpath's own and is not typed here, deliberately:
        the CLI ignores unknown keys so an older binary tolerates config
        written for a newer one, and a typed Nix layer would fork a schema
        that lives in the Rust. Today the only key is `project`, a list
        of rules routing `path share`, each selecting sessions by `dir`
        (a subtree of the session's directory, most specific wins) or by
        `origin` (the `owner/name` of the enclosing repository's `origin`
        remote), and naming a `remote` — a bare `owner/name` or a
        canonical Pathbase repo URL.

        Prefer `origin` when this config is version-controlled: it names
        the repository rather than a place on one machine, so nothing
        about your directory layout is committed and the same rule works
        on every checkout, clone and worktree. `dir` takes a leading
        `~/`, expanded by toolpath when it reads the file; if you need it
        anyway, interpolate `config.home.homeDirectory` and map checkout
        names onto one `repoRoot` binding rather than spelling out paths.

        Two consequences of managing the file from here.

        `path config edit` stops working, and does so quietly: the target
        becomes a read-only symlink into the store, so the editor cannot
        save, but the command then re-reads the *unchanged* file, validates
        it, prints a rule count and exits 0. It looks like it worked. Edit
        this option and switch instead — or leave `settings` unset if you
        want to keep the imperative path.

        The file also moves from mode 0600 in your home directory to
        world-readable in the Nix store. `config.toml` holds no
        credentials, but it does name local directories and the remotes
        they publish to. `credentials.json`, `documents/` and
        `manifest.json` are untouched by this option and stay imperative.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ (if cfg.devBin == null then cfg.package else devPackage) ];

    # Written only when the user actually sets `settings`; an empty attrset
    # leaves an existing hand-maintained config.toml alone. Note that on the
    # switch that first populates this, home-manager refuses to clobber a
    # pre-existing real file — adopt with `home-manager switch -b backup`.
    home.file.".toolpath/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = tomlFormat.generate "toolpath-config.toml" cfg.settings;
    };
  };
}

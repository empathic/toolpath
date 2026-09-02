{
  description = "toolpath — a format for artifact transformation provenance";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
      # The crate's own version, so a release bump is one edit, not two.
      pathCliVersion = (builtins.fromTOML (builtins.readFile ./crates/path-cli/Cargo.toml)).package.version;
    in
    {
      # The `path` binary, built from this checkout. This is what a consumer
      # that wants to follow a branch of this repo pins — bdelanghe/home tracks
      # lobby that way (its flake exports a package and a module, home's input
      # names a ref and no rev, and an auto-updater bumps it whenever CI is
      # green on the new tip). Until now the only nix build of path-cli lived in
      # bdelanghe/empathic-nix, pinned to a rev of this repo by hand; that pin
      # stays valid, it just stops being the only route.
      packages = forAll (pkgs: rec {
        toolpath = pkgs.rustPlatform.buildRustPackage {
          pname = "toolpath-path";
          version = pathCliVersion;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # Just the CLI. The workspace excludes the deprecated toolpath-cli
          # shim (both produce a binary named `path`), and the library crates
          # are pulled in as path-cli's dependencies.
          cargoBuildFlags = [ "-p" "path-cli" ];

          # openssl is not a direct dependency — it arrives under git2
          # (libgit2-sys → libssh2-sys → openssl-sys), and openssl-sys refuses
          # to build unless pkg-config finds the dev output. OPENSSL_NO_VENDOR
          # keeps it off the bundled-source path.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
          env.OPENSSL_NO_VENDOR = "1";

          # The workspace test surface is large, spawns harnesses, and reaches
          # for the network; CI runs it with the pinned toolchain. The package
          # is the binary.
          doCheck = false;

          meta = {
            description = "Toolpath provenance CLI (binary: path)";
            mainProgram = "path";
          };
        };
        default = toolpath;
      });

      # `programs.toolpath` — enable, package, devBin — for a home-manager
      # config that takes this flake as an input.
      homeManagerModules.toolpath = import ./modules/toolpath.nix self;
      homeManagerModules.default = self.homeManagerModules.toolpath;

      # `nix develop` gives you what the justfile and scripts/quality_gates.sh
      # already assume is on PATH — for working in the checkout itself, rather
      # than building it. There is no rustup on the machines this runs on, so
      # without this shell `cargo` is simply absent and every just recipe fails
      # at `command not found`.
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          # Same openssl wiring as the package above, for the same reason.
          # Splitting these two across nativeBuildInputs and buildInputs is
          # what wires PKG_CONFIG_PATH up; listing them both in `packages`
          # puts the binaries on PATH and still fails the build.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];

          packages = [
            # nixpkgs decides the Rust version here. CI installs a different
            # one: deploy-site.yml runs `rustup show`, which honours
            # rust-toolchain.toml's 1.94.0 pin. That file is read by rustup, so
            # inside this shell it is inert — you get whatever nixpkgs ships,
            # currently ahead of the pin. clippy gains lints between releases,
            # so a tree that is clean under `just ci` in here is evidence, not
            # proof; the pinned toolchain is what the gate actually is.
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.just

            # quality_gates.sh, beyond the Rust gates: `shellcheck` is a hard
            # requirement of its own gate (it errors out rather than skipping),
            # the site gate runs `pnpm install --frozen-lockfile && pnpm run
            # build`, and the format gate shells out to `npx prettier`, which
            # comes from nodejs.
            pkgs.shellcheck
            pkgs.nodejs
            pkgs.pnpm

            # plugins/claude-code/scripts/ensure-path.sh downloads and verifies
            # a release tarball. Its checksum step already falls back from
            # sha256sum to shasum, so coreutils is not needed here.
            pkgs.jq
            pkgs.curl

            # The interactive pickers in path-cli prefer an external fzf when
            # one is on PATH and fall back to the embedded skim picker
            # otherwise. Having it here means the default path is the one you
            # exercise while developing.
            pkgs.fzf
          ];
        };
      });
    };
}

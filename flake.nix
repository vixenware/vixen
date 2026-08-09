{
  description = "Vix — a typed, demand-driven language whose evaluation is a build";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      crane,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # The toolchain file is the single source of truth for the channel and
        # its components, so the flake never drifts from a `rustup` checkout.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # NOT `cleanCargoSource`: that keeps only Rust sources, and this
        # workspace's build inputs are much wider than `.rs` — `vix-core`'s
        # build script generates the surface AST from Snark grammars (`.js`),
        # the ratchet corpus is `.vix`, the fixtures are whole file trees, and
        # rustdoc reads the `arborium-header.html` files. Dropping any of them
        # fails deep in a build script with a confusing error, so the filter
        # keeps everything but the noise.
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              base = baseNameOf path;
            in
            !(
              (type == "directory" && (base == "target" || base == ".git"))
              || pkgs.lib.hasSuffix ".log" base
            );
        };

        commonArgs = {
          inherit src;
          strictDeps = true;

          # The workspace root is virtual — it carries `[workspace]` and no
          # `[package]` — so crane has no manifest to read a name and version
          # off and would fall back to placeholders. Naming them here is the
          # fix crane's own warning recommends.
          pname = "vixen";
          version = "0.0.0";

          # `blake3` and `ring` (through `ureq`'s rustls stack) compile C and
          # assembly, so `cc` needs a compiler driver on PATH. `nodejs` is
          # `vix-core`'s build script: it generates the surface typed AST from
          # the Snark grammars, and `snark-dsl` shells out to `node` or `bun`
          # to evaluate them — without one the build fails at
          # `JsRuntimeNotFound`, not at a missing library.
          nativeBuildInputs = with pkgs; [
            nodejs
            pkg-config
            perl
          ];
          buildInputs = [ ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        vixen = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            # The test suite is a separate check so a failing rung does not
            # block producing the binaries.
            doCheck = false;
          }
        );
      in
      {
        packages = {
          default = vixen;
          inherit vixen;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = vixen;
          name = "vx";
        };

        checks = {
          inherit vixen;

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
            }
          );

          # No `cargoFmt` check on purpose. The tree is not rustfmt-clean at
          # HEAD — 77 diffs across 34 files under the pinned 1.96 toolchain,
          # none of them in this branch's changes — so the gate would be red on
          # arrival and would say nothing about any given change. Add it back in
          # the commit that formats the tree, not before.

          # `exec_tree_mounts` compiles and runs a real Rust binary through the
          # exec rail, so the test environment needs a rustc of its own and the
          # `RUSTC` the harness reads (the README's `rustup which rustc`, which
          # does not exist under nix).
          tests = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
                rustToolchain
                pkgs.bash
              ];
              RUSTC = "${rustToolchain}/bin/rustc";
            }
          );
        };

        devShells.default = craneLib.devShell {
          inputsFrom = [ vixen ];
          packages = with pkgs; [
            cargo-nextest
            nodejs
            pkg-config
            perl
          ];
          # Same reason the check sets it: driving `vx` or the exec rungs by
          # hand needs a rustc the harness can spawn.
          RUSTC = "${rustToolchain}/bin/rustc";
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}

{
  description = "snot: reader, formatter, checker and language server for Simple Note Format";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
        in
        {
          default = self.packages.${system}.snot;
          snot = pkgs.rustPlatform.buildRustPackage {
            pname = "snot";
            inherit (cargoToml.workspace.package) version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                # Test fixtures, once they exist (PLAN.md).
                (lib.fileset.maybeMissing ./conformance)
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            # Test every crate, not just the binary.
            cargoTestFlags = [ "--workspace" ];
            meta = {
              description = "Reader, formatter, checker and language server for Simple Note Format";
              homepage = "https://github.com/zjom/snot";
              license = pkgs.lib.licenses.mit;
              mainProgram = "snot";
            };
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.snot ];
            packages = [
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.cargo-insta # snapshot tests (conformance corpus)
              pkgs.cargo-dist # release binaries
            ];
            # rust-analyzer needs the standard library source.
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      # `nix flake check` is what CI runs: build and test, clippy, rustfmt.
      checks = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (self.packages.${system}) snot;
        in
        {
          # Building the package runs `cargo test --workspace`.
          inherit snot;

          clippy = snot.overrideAttrs (old: {
            pname = "snot-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = ''
              runHook preBuild
              cargo clippy --workspace --all-targets --offline -- --deny warnings
              runHook postBuild
            '';
            doCheck = false;
            installPhase = "touch $out";
          });

          rustfmt =
            pkgs.runCommand "snot-rustfmt"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  pkgs.rustfmt
                ];
              }
              ''
                cd ${snot.src}
                cargo fmt --all --check
                touch $out
              '';
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

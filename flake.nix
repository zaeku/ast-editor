{
  description = "ast-editor development shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.just
            pkgs.cargo-release
            pkgs.cargo-llvm-cov
          ];

          # cargo-llvm-cov needs the coverage tools rustc's own LLVM wrote the
          # profiles with. Nothing ships them beside rustc here, so the version
          # is matched by hand: `rustc --version --verbose` names it, and a
          # mismatch is read as a corrupt profile rather than as a wrong tool.
          LLVM_COV = "${pkgs.llvmPackages_21.llvm}/bin/llvm-cov";
          LLVM_PROFDATA = "${pkgs.llvmPackages_21.llvm}/bin/llvm-profdata";

          # This shell's rustc and the system's are different versions on their
          # own update schedules, and cargo keyed on one of them reads the
          # other's artifacts as fresh: it answers `Finished` and leaves a
          # binary the source no longer describes, which `touch` does not
          # correct. Giving this shell a directory of its own means the two
          # never meet. Set from $PWD so it is the checkout's rather than
          # whatever directory the shell was entered from.
          shellHook = ''
            export CARGO_TARGET_DIR="$PWD/target/nix"
          '';
        };
      });
    };
}

{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      source = builtins.path {
        path = ./.;
        name = "cano-source";
        filter = path: _:
          !builtins.elem (builtins.baseNameOf path) [
            ".git"
            "build"
            "result"
            "target"
          ];
      };
    in {
      packages.${system} = rec {
        cano = pkgs.rustPlatform.buildRustPackage {
          pname = "cano";
          version = "0.1.0";
          src = source;
          cargoLock.lockFile = source + "/Cargo.lock";
          CANO_HELP_DIR = "${placeholder "out"}/share/cano/help";
          postInstall = ''
            mkdir -p "$out/share/cano/help"
            cp docs/help/* "$out/share/cano/help/"
          '';
        };
        default = cano;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy ];
      };
    };
}

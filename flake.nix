{
  description = "A language server, formatter, and linter for Julia";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        fatou = pkgs.rustPlatform.buildRustPackage {
          pname = "fatou";
          version = "0.1.0";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          meta = with pkgs.lib; {
            description = "An LSP, formatter, and linter for Julia";
            homepage = "https://github.com/jolars/fatou";
            license = licenses.mit;
            maintainers = [ ];
          };
        };
      in
      {
        packages = {
          default = fatou;
          fatou = fatou;
        };

        apps = {
          default = {
            type = "app";
            program = "${fatou}/bin/fatou";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            go-task
          ];
        };
      }
    );
}

{
  pkgs,
  ...
}:

{
  # Make every Julia invocation in the repo use the pinned root project
  # (`Project.toml` + `Manifest.toml`), searching upward from the cwd.
  env.JULIA_PROJECT = "@.";

  packages = [
    pkgs.perf
    pkgs.cargo-flamegraph
    pkgs.cargo-llvm-cov
    pkgs.cargo-audit
    pkgs.cargo-deny
    pkgs.cargo-insta
    pkgs.go-task
    pkgs.mdbook
    pkgs.llvmPackages.bintools
    pkgs.prettier
    pkgs.ruff
    pkgs.shfmt
    pkgs.wasm-pack
    pkgs.stylua
    pkgs.hyperfine
    pkgs.yamlfmt
    pkgs.vsce
    pkgs.air-formatter
  ];

  languages = {
    rust = {
      enable = true;

      toolchainFile = ./rust-toolchain.toml;
    };

    julia = {
      enable = true;

      # nix provides only the bare interpreter. All Julia packages are managed by
      # Pkg via the repo's root `Project.toml` + committed `Manifest.toml`, which
      # every Julia invocation activates through `JULIA_PROJECT` (set below).
      # nixpkgs' `withPackages` is deliberately avoided: it resolves an old
      # registry snapshot (which pinned JuliaSyntax by accident, defeating the
      # oracle's exact-version contract) and gives no version control. See
      # AGENTS.md.
      package = pkgs.julia-bin;
    };

    javascript = {
      enable = true;

      pnpm = {
        enable = true;

        install = {
          enable = true;
        };
      };
    };

    typescript = {
      enable = true;
    };
  };

  git-hooks = {
    hooks = {
      clippy = {
        enable = true;
        settings = {
          allFeatures = true;
        };
      };

      rustfmt = {
        enable = true;
      };

      biome = {
        enable = true;
        args = [ "--no-errors-on-unmatched" ];
      };

      # panache-format = {
      #   enable = true;
      # };
    };
  };
}

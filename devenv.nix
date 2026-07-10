{
  pkgs,
  ...
}:

{
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
    # JuliaSyntax.jl (the parser oracle, see AGENTS.md) installs via the Julia
    # environment, not nixpkgs.
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

      package = (
        pkgs.julia-bin.withPackages [
          "CairoMakie"
          "JET"
          "JuliaFormatter"
          "JuliaSyntax"
          "Makie"
          "Plots"
          "Revise"
          "Runic"
          "StaticLint"
          "Test"
        ]
      );
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
      };

      # panache-format = {
      #   enable = true;
      # };
    };
  };
}

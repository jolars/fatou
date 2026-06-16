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
    pkgs.llvmPackages.bintools
    pkgs.prettier
    # Runic.jl and JuliaSyntax.jl (the formatter compat oracle and parser
    # oracle, see AGENTS.md) install via the Julia environment, not nixpkgs.
    pkgs.ruff
    pkgs.shfmt
    pkgs.wasm-pack
    pkgs.stylua
    pkgs.hyperfine
    pkgs.yamlfmt
    pkgs.vsce
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
          "Runic"
          "JuliaFormatter"
          "Makie"
          "CairoMakie"
          "Plots"
          "Revise"
          "Test"
          "JuliaSyntax"
          "JET"
          "Lint"
          "StaticLint"
          "JuliaLowering"
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
    };
  };
}

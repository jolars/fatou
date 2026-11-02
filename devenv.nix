{
  inputs,
  config,
  lib,
  pkgs,
  ...
}:

{
  # Make every Julia invocation in the repo use the pinned root project
  # (`Project.toml` + `Manifest.toml`), searching upward from the cwd.
  env.JULIA_PROJECT = "@.";

  # The npm workerd binary needs the Nix dynamic loader on Linux.
  env.MINIFLARE_WORKERD_PATH = lib.mkIf pkgs.stdenv.isLinux "${pkgs.writeShellScript "fatou-workerd" ''
    cd "${config.devenv.root}/docs"
    binary=$(node -e 'const {createRequire}=require("node:module"); const wrangler=createRequire(require.resolve("wrangler/package.json")); const miniflare=createRequire(wrangler.resolve("miniflare/package.json")); process.stdout.write(miniflare("workerd").default);')
    loader=$(cat "$NIX_CC/nix-support/dynamic-linker")
    exec "$loader" "$binary" "$@"
  ''}";

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
    pkgs.ruff
    pkgs.shfmt
    pkgs.wasm-pack
    pkgs.stylua
    pkgs.hyperfine
    # GNU time, for the peak-RSS readings in bench/cli_memory.py. The shell
    # builtin cannot report ru_maxrss, and measuring from the Python harness
    # charges the forked interpreter's own resident set to the child.
    pkgs.time
    pkgs.imagemagick
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
      package =
        (import inputs.nixpkgs-julia {
          system = pkgs.stdenv.hostPlatform.system;
        }).julia_113-bin;
    };

    javascript = {
      enable = true;
      package = pkgs.nodejs_24;
      directory = "./docs";

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

      panache-format = {
        enable = true;
      };
    };
  };
}

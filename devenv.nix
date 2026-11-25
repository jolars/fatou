{
  inputs,
  config,
  lib,
  pkgs,
  ...
}:

let
  stableJulia =
    (import inputs.nixpkgs-julia {
      system = pkgs.stdenv.hostPlatform.system;
    }).julia_113-bin;
  benchmarkJulia = stableJulia.overrideAttrs (_: {
    # Check the executable without running Julia's full test suite on shell entry.
    installCheckPhase = ''
      runHook preInstallCheck
      "$out/bin/julia" --startup-file=no --version
      runHook postInstallCheck
    '';
  });
in
{
  # Make every Julia invocation in the repo use the pinned root project
  # (`Project.toml` + `Manifest.toml`), searching upward from the cwd.
  env.JULIA_PROJECT = "@.";

  # LanguageServer.jl's dependencies may fail to precompile on the oracle nightly.
  env.BENCH_JULIA = "${benchmarkJulia}/bin/julia";

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
    pkgs.python3
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

      # Share the oracle's interpreter pin while keeping packages Pkg-managed.
      # Native ELF patching also makes Julia's precompile workers work on NixOS.
      package =
        if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
          pkgs.callPackage ./nix/julia.nix {
            pin = builtins.fromJSON (builtins.readFile ./scripts/julia-oracle.lock.json);
          }
        else
          # The lock currently provides only a Linux x86-64 archive.
          stableJulia;
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

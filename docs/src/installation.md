# Installation

Fatou runs on Linux, macOS, and Windows (x86_64 and arm64). Use the installer
below for the command line, or choose a package manager you already use.

The [VS Code extension](guide/editors.md#vs-code) includes the binary and starts
the language server for you. The [Zed extension](guide/editors.md#zed) can use
an installed copy or download one automatically.

## Install script

The installer selects the release for your platform and installs to a user-local
directory.

::: code-group

```sh [macOS/Linux]
curl --proto '=https' --tlsv1.2 -sSf https://fatou.dev/install | sh
```

```powershell [Windows]
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://fatou.dev/install.ps1 | iex"
```

:::

## Package managers

### Cargo

Install from [crates.io](https://crates.io/crates/fatou) with Cargo:

```bash
cargo install fatou
```

### Homebrew

On macOS or Linux:

```bash
brew install jolars/tap/fatou
```

### npm

The `fatou-cli` package bundles a prebuilt binary:

```bash
npm install -g fatou-cli
```

### PyPI

Install the binary as a Python tool:

```bash
uv tool install fatou
# or
pipx install fatou
```

### AUR

On Arch Linux, install the prebuilt
[`fatou-bin`](https://aur.archlinux.org/packages/fatou-bin) package with an AUR
helper:

```bash
paru -S fatou-bin
```

### Nix

Fatou is available as `fatou` in
[Nixpkgs](https://search.nixos.org/packages?channel=unstable&show=fatou). For a
shell with Fatou available:

```bash
nix shell nixpkgs#fatou
```

For a persistent NixOS installation, add `pkgs.fatou` to
`environment.systemPackages`.

### mise and Aqua

[mise](https://mise.jdx.dev/dev-tools/backends/aqua.html) can install Fatou
through its Aqua backend:

```bash
mise use aqua:jolars/fatou
```

Commit the resulting `mise.toml` to share the selected version. With
[Aqua](https://aquaproj.github.io/docs/tutorial/) directly, run `aqua init` if
the project has no `aqua.yaml`, then add and install Fatou:

```bash
aqua g -i jolars/fatou
aqua install
```

Commit `aqua.yaml` to share the selected version. Both tools use the
[`jolars/fatou` registry
entry](https://github.com/aquaproj/aqua-registry/tree/main/pkgs/jolars/fatou).

## Prebuilt binaries

Download an archive for your platform from the [releases
page](https://github.com/jolars/fatou/releases) and put the `fatou` binary on
your `PATH`.

## From source

Clone the repository and build a release binary:

```bash
git clone https://github.com/jolars/fatou
cd fatou
cargo build --release
```

The binary is written to `target/release/fatou`.

## dprint

To format Julia files with dprint, add the [Fatou dprint
plugin](guide/integrations.md#dprint). It bundles the formatter and requires
neither Julia nor the Fatou CLI. Follow the linked recipe for setup and
configuration.

## Verify the installation

Check that Fatou is available in your terminal:

```sh
fatou --version
```

Continue with [Getting started](guide/getting-started.md) to check and format an
existing Julia project, or [Editor setup](guide/editors.md) to use Fatou while
editing.

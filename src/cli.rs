use std::path::PathBuf;

use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use clap::{Parser, Subcommand};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

#[derive(Parser)]
#[command(name = "fatou")]
#[command(author, version)]
#[command(about = "Fatou: a language server, formatter, and linter for Julia")]
#[command(styles = STYLES)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    /// Path to an explicit `fatou.toml` (skips discovery).
    #[arg(long, value_name = "PATH", global = true, conflicts_with = "no_config")]
    pub config: Option<PathBuf>,

    /// Ignore any `fatou.toml` (project, `FATOU_CONFIG`, or global) and use
    /// built-in defaults.
    #[arg(long, global = true)]
    pub no_config: bool,

    /// When to colorize human-readable output.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, global = true)]
    pub color: ColorChoice,

    /// Suppress non-essential output (errors are still shown). Under
    /// `format --check` this drops the per-file diff, leaving the list of files
    /// that would be reformatted and the summary; under `parse` it suppresses
    /// the CST.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Colorize when writing to a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Parse and display the CST for debugging.
    Parse {
        /// Input file (stdin if not provided).
        file: Option<PathBuf>,

        /// Verify parser losslessness (`reconstruct(text) == text`).
        #[arg(long)]
        verify: bool,

        /// Output representation: the lossless CST (default) or the JuliaSyntax
        /// s-expression projection (the parser oracle).
        #[arg(long, value_enum, default_value_t = ParseFormat::Cst)]
        to: ParseFormat,
    },
    /// Format `.jl` files.
    Format {
        /// Input file(s) or path(s) (stdin if omitted).
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Check formatting without writing; prints a diff and exits non-zero if
        /// any file would change.
        #[arg(long)]
        check: bool,

        /// Override the target line width.
        #[arg(long, value_name = "N")]
        line_width: Option<u32>,

        /// Override the indent width.
        #[arg(long, value_name = "N")]
        indent_width: Option<u32>,

        /// Additional gitignore-style exclude patterns (repeatable or
        /// comma-separated); augments the configured `exclude`/`extend-exclude`.
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Vec<String>,

        /// Apply exclude patterns to files named explicitly on the command line
        /// too (they are normally always processed); for runners like
        /// pre-commit that pass staged files as arguments.
        #[arg(long)]
        force_exclude: bool,
    },
    /// Lint `.jl` files.
    Lint {
        /// Input file(s) or path(s).
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Apply safe fixes to the source and write the files back.
        #[arg(long)]
        fix: bool,

        /// Also apply fixes marked unsafe (implies `--fix`).
        #[arg(long)]
        unsafe_fixes: bool,

        /// Additional gitignore-style exclude patterns (repeatable or
        /// comma-separated); augments the configured `exclude`/`extend-exclude`.
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Vec<String>,

        /// Apply exclude patterns to files named explicitly on the command line
        /// too (they are normally always processed); for runners like
        /// pre-commit that pass staged files as arguments.
        #[arg(long)]
        force_exclude: bool,

        /// Target Julia version or range for version-compat checks (e.g. `1.10`
        /// or `1.6 - 1.11`); overrides `[julia] version` and the project's
        /// `Project.toml` `[compat]`.
        #[arg(long, value_name = "VERSION")]
        julia_version: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value_t = LintOutput::Pretty)]
        output: LintOutput,
    },
    /// Run the language server on stdio.
    Lsp,
    /// Debug utilities for parser and formatter diagnostics.
    ///
    /// Intended for CI smoke tests and local triage; hidden from help and the
    /// generated docs, and covered by no stability promise.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
}

/// Subcommands under `fatou debug`.
#[derive(Subcommand)]
pub enum DebugCommand {
    /// Check formatter and parser invariants per file, writing nothing back.
    ///
    /// Runs the selected checks (losslessness: `reconstruct(x) == x`;
    /// idempotency: `fmt(fmt(x)) == fmt(x)`) over each input file. `--report`
    /// emits a Markdown summary to stdout; `--dump-dir` writes per-pass
    /// artifacts for triage.
    Format {
        /// Files, directories, or globs to check.
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Which invariant checks to run.
        #[arg(long, value_enum, default_value_t = DebugChecksArg::All)]
        checks: DebugChecksArg,

        /// Emit a Markdown report to stdout instead of log lines.
        #[arg(long)]
        report: bool,

        /// Directory where per-pass artifacts are written on failure.
        #[arg(long, value_name = "DIR")]
        dump_dir: Option<PathBuf>,

        /// Write pass artifacts even when all checks pass.
        #[arg(long, requires = "dump_dir")]
        dump_passes: bool,

        /// Additional gitignore-style exclude patterns (repeatable or
        /// comma-separated); augments the configured `exclude`/`extend-exclude`.
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Vec<String>,

        /// Apply exclude patterns to files named explicitly on the command line
        /// too (they are normally always processed).
        #[arg(long)]
        force_exclude: bool,
    },
}

/// Which checks `fatou debug format` runs.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugChecksArg {
    /// Only the formatter fixed-point check: `fmt(fmt(x)) == fmt(x)`.
    Idempotency,
    /// Only the parser round-trip check: `reconstruct(x) == x`.
    Losslessness,
    /// Both checks (default).
    All,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LintOutput {
    Pretty,
    Concise,
    Json,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFormat {
    /// The lossless `rowan` concrete syntax tree.
    Cst,
    /// The JuliaSyntax-native s-expression projection.
    Sexpr,
}

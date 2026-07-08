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

    /// Ignore any discovered `fatou.toml` and use built-in defaults.
    #[arg(long, global = true)]
    pub no_config: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Parse and display the CST for debugging.
    Parse {
        /// Input file (stdin if not provided).
        file: Option<PathBuf>,

        /// Suppress CST output to stdout.
        #[arg(long)]
        quiet: bool,

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

        /// Output format.
        #[arg(long, value_enum, default_value_t = LintOutput::Pretty)]
        output: LintOutput,
    },
    /// Run the language server on stdio.
    Lsp,
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

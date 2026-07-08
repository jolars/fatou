use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use fatou::cli::{Cli, ColorChoice, Commands, LintOutput, ParseFormat};
use fatou::config::Config;
use fatou::formatter::{self, FormatStyle};
use fatou::linter::{self, LintStatus, OutputMode};
use fatou::parser::{parse, reconstruct, to_juliasyntax_sexpr};

fn main() -> ExitCode {
    env_logger::init();
    let cli = Cli::parse();

    match run(cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Commands::Parse {
            file,
            quiet,
            verify,
            to,
        } => run_parse(file, quiet, verify, to),
        Commands::Format {
            paths,
            check,
            line_width,
            indent_width,
        } => {
            let style = resolve_style(&cli.config, cli.no_config, line_width, indent_width)?;
            run_format(paths, check, style)
        }
        Commands::Lint {
            paths,
            fix,
            unsafe_fixes,
            output,
        } => {
            let config = load_config(&cli.config, cli.no_config)?;
            run_lint(paths, output, fix, unsafe_fixes, cli.color, &config)
        }
        Commands::Lsp => fatou::lsp::run()
            .map(|()| ExitCode::SUCCESS)
            .map_err(|err| err.to_string()),
    }
}

fn run_parse(
    file: Option<PathBuf>,
    quiet: bool,
    verify: bool,
    to: ParseFormat,
) -> Result<ExitCode, String> {
    let text = read_source(file.as_deref())?;
    let output = parse(&text);

    if !quiet {
        match to {
            ParseFormat::Cst => print!("{:#?}", output.cst),
            ParseFormat::Sexpr => {
                println!("{}", to_juliasyntax_sexpr(&output.cst, &output.diagnostics))
            }
        }
        for diag in &output.diagnostics {
            eprintln!(
                "diagnostic [{}..{}]: {}",
                diag.start, diag.end, diag.message
            );
        }
    }

    if verify {
        let reconstructed = reconstruct(&text);
        if reconstructed == text {
            eprintln!("losslessness OK");
        } else {
            eprintln!("losslessness FAILED: reconstruction differs from input");
            return Ok(ExitCode::FAILURE);
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn run_format(paths: Vec<PathBuf>, check: bool, style: FormatStyle) -> Result<ExitCode, String> {
    // No paths: format stdin to stdout.
    if paths.is_empty() {
        let text = read_source(None)?;
        let formatted = formatter::format_with_style(&text, style).map_err(|e| e.to_string())?;
        print!("{formatted}");
        return Ok(ExitCode::SUCCESS);
    }

    if check {
        let result = formatter::check_paths(&paths, style).map_err(|e| e.to_string())?;
        for changed in &result.changed {
            println!("would reformat {}", changed.path.display());
            print!("{}", changed.diff);
        }
        return Ok(if result.changed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let files = fatou::file_discovery::collect_julia_files(&paths).map_err(|e| e.to_string())?;
    for path in &files {
        let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let formatted =
            formatter::format_with_style(&original, style).map_err(|e| e.to_string())?;
        if formatted != original {
            std::fs::write(path, formatted).map_err(|e| e.to_string())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_lint(
    paths: Vec<PathBuf>,
    output: LintOutput,
    fix: bool,
    unsafe_fixes: bool,
    color: ColorChoice,
    config: &Config,
) -> Result<ExitCode, String> {
    if paths.is_empty() {
        return Err("lint requires at least one path".to_string());
    }

    let mode = match output {
        LintOutput::Pretty => OutputMode::Pretty,
        LintOutput::Concise => OutputMode::Concise,
        LintOutput::Json => OutputMode::Json,
    };

    if fix || unsafe_fixes {
        return run_lint_fix(paths, mode, unsafe_fixes, color, config);
    }

    let use_color = color_enabled(color, std::io::stderr().is_terminal());
    let result =
        linter::check_paths_with_config(&paths, &config.lint).map_err(|e| e.to_string())?;
    warn_unknown_rules(&result.unknown_rules);

    let diagnostics: Vec<_> = result
        .reports
        .iter()
        .flat_map(|report| report.diagnostics.clone())
        .collect();
    let rendered = linter::render_findings(&diagnostics, mode, use_color, &|path| {
        path.and_then(|p| std::fs::read_to_string(p).ok())
    });
    emit(mode, &rendered);

    let has_parse_errors = result
        .reports
        .iter()
        .any(|report| matches!(report.status, LintStatus::ParseDiagnostics { .. }));
    if result.total_findings > 0 || has_parse_errors {
        Ok(ExitCode::FAILURE)
    } else {
        eprintln!("checked {} file(s): clean", result.checked_files);
        Ok(ExitCode::SUCCESS)
    }
}

/// Apply fixes across every discovered file, writing changed files back, then
/// report whatever findings remain. Exits non-zero if any remain (Ruff-style).
fn run_lint_fix(
    paths: Vec<PathBuf>,
    mode: OutputMode,
    unsafe_fixes: bool,
    color: ColorChoice,
    config: &Config,
) -> Result<ExitCode, String> {
    let use_color = color_enabled(color, std::io::stderr().is_terminal());
    let (_, unknown_rules) =
        linter::ResolvedRules::resolve(config.lint.select.as_deref(), &config.lint.ignore);
    warn_unknown_rules(&unknown_rules);
    let files = fatou::file_discovery::collect_julia_files(&paths).map_err(|e| e.to_string())?;

    let mut applied = 0usize;
    let mut changed_files = 0usize;
    let mut remaining = Vec::new();

    for path in &files {
        let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let outcome = linter::fix_source(Some(path), &original, &config.lint, unsafe_fixes);
        if outcome.output != original {
            std::fs::write(path, &outcome.output).map_err(|e| e.to_string())?;
            changed_files += 1;
        }
        applied += outcome.applied;
        remaining.extend(outcome.remaining);
    }

    let rendered = linter::render_findings(&remaining, mode, use_color, &|path| {
        path.and_then(|p| std::fs::read_to_string(p).ok())
    });
    emit(mode, &rendered);

    if applied > 0 {
        eprintln!("fixed {applied} issue(s) in {changed_files} file(s)");
    }

    if remaining.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Warn (once, to stderr) about any `select`/`ignore` entry that names no
/// shipped rule, so a typo'd `--select` doesn't silently select nothing.
fn warn_unknown_rules(unknown: &[String]) {
    for id in unknown {
        eprintln!("warning: unknown rule `{id}` in select/ignore");
    }
}

/// Route rendered lint output: JSON to stdout (machine-readable), human-facing
/// pretty/concise to stderr.
fn emit(mode: OutputMode, rendered: &str) {
    if matches!(mode, OutputMode::Json) {
        print!("{rendered}");
    } else {
        eprint!("{rendered}");
    }
}

fn color_enabled(choice: ColorChoice, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && is_terminal,
    }
}

fn resolve_style(
    explicit_config: &Option<PathBuf>,
    no_config: bool,
    line_width: Option<u32>,
    indent_width: Option<u32>,
) -> Result<FormatStyle, String> {
    let config = load_config(explicit_config, no_config)?;
    let mut style = FormatStyle::from(&config.format);
    if let Some(width) = line_width {
        style.line_width = width;
    }
    if let Some(width) = indent_width {
        style.indent_width = width;
    }
    Ok(style)
}

fn load_config(explicit_config: &Option<PathBuf>, no_config: bool) -> Result<Config, String> {
    let anchor = std::env::current_dir().map_err(|e| e.to_string())?;
    let (config, _path) = Config::resolve(explicit_config.as_deref(), no_config, &anchor)
        .map_err(|e| e.to_string())?;
    Ok(config)
}

fn read_source(path: Option<&Path>) -> Result<String, String> {
    match path {
        Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string()),
        None => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| e.to_string())?;
            Ok(buffer)
        }
    }
}

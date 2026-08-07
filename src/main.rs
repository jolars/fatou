use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use rayon::prelude::*;

use fatou::cli::{
    Cli, ColorChoice, Commands, DebugChecksArg, DebugCommand, LintOutput, ParseFormat,
};
use fatou::config::{Config, ConfigSource};
use fatou::debug::{
    CheckKind, DebugArtifacts, DebugFailure, build_debug_report, checks_label,
    run_debug_checks_for_file, sanitize_path_for_filename, write_debug_artifacts,
};
use fatou::file_discovery::ExcludeFilter;
use fatou::formatter::{self, FormatStyle};
use fatou::linter::{self, LintStatus, OutputMode, RenderOptions};
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
        Commands::Parse { file, verify, to } => run_parse(file, cli.quiet, verify, to),
        Commands::Format {
            paths,
            check,
            line_width,
            indent_width,
            exclude,
            force_exclude,
        } => {
            let (config, source) = load_config(&cli.config, cli.no_config)?;
            let style = style_with_overrides(&config, line_width, indent_width);
            let filter = resolve_exclude_filter(&config, &source, &exclude, force_exclude)?;
            run_format(paths, check, style, &filter, cli.quiet)
        }
        Commands::Lint {
            paths,
            fix,
            unsafe_fixes,
            exclude,
            force_exclude,
            julia_version,
            output,
        } => {
            let (config, source) = load_config(&cli.config, cli.no_config)?;
            let filter = resolve_exclude_filter(&config, &source, &exclude, force_exclude)?;
            let anchor = std::env::current_dir().map_err(|e| e.to_string())?;
            let julia_target = resolve_julia_target(julia_version.as_deref(), &config, &anchor);
            run_lint(
                paths,
                output,
                fix,
                unsafe_fixes,
                cli.color,
                &config,
                &filter,
                julia_target,
            )
        }
        Commands::Lsp => fatou::lsp::run()
            .map(|()| ExitCode::SUCCESS)
            .map_err(|err| err.to_string()),
        Commands::Debug { command } => match command {
            DebugCommand::Format {
                paths,
                checks,
                report,
                dump_dir,
                dump_passes,
                exclude,
                force_exclude,
            } => {
                let (config, source) = load_config(&cli.config, cli.no_config)?;
                let style = style_with_overrides(&config, None, None);
                let filter = resolve_exclude_filter(&config, &source, &exclude, force_exclude)?;
                run_debug_format(
                    &paths,
                    checks,
                    report,
                    dump_dir.as_deref(),
                    dump_passes,
                    style,
                    &filter,
                )
            }
        },
    }
}

/// `fatou debug format`: check invariants over the discovered files, writing
/// nothing back. Exit 0 when everything passes, 1 on any failure or unreadable
/// file (config and discovery errors keep their usual codes upstream).
fn run_debug_format(
    paths: &[PathBuf],
    checks: DebugChecksArg,
    report: bool,
    dump_dir: Option<&Path>,
    dump_passes: bool,
    style: FormatStyle,
    exclude: &ExcludeFilter,
) -> Result<ExitCode, String> {
    if paths.is_empty() {
        eprintln!("fatou: debug format requires at least one file or directory");
        return Ok(ExitCode::from(2));
    }
    let files =
        fatou::file_discovery::collect_julia_files(paths, exclude).map_err(|e| e.to_string())?;
    if files.is_empty() {
        if exclude.force() {
            return Ok(ExitCode::SUCCESS);
        }
        return Err("no .jl files found under the provided input paths".to_string());
    }

    // Checks are pure functions of the file content, so they parallelize like
    // `run_format`; the order-preserving collect keeps output and report
    // numbering deterministic.
    let outcomes: Vec<(String, Result<DebugArtifacts, String>)> = files
        .par_iter()
        .map(|path| {
            let label = path.display().to_string();
            let outcome = match std::fs::read_to_string(path) {
                Ok(content) => Ok(run_debug_checks_for_file(&content, style, checks)),
                Err(err) => Err(format!("fatou: cannot read {label}: {err}")),
            };
            (label, outcome)
        })
        .collect();

    let mut files_checked = 0usize;
    let mut io_failed = false;
    let mut collected: Vec<(String, DebugFailure)> = Vec::new();
    for (label, outcome) in outcomes {
        match outcome {
            Err(msg) => {
                eprintln!("{msg}");
                io_failed = true;
            }
            Ok(artifacts) => {
                files_checked += 1;
                if let Some(dir) = dump_dir {
                    let stem = sanitize_path_for_filename(&label);
                    if let Err(err) = write_debug_artifacts(dir, &stem, &artifacts, dump_passes) {
                        eprintln!(
                            "fatou: cannot write debug artifacts to {}: {err}",
                            dir.display()
                        );
                        io_failed = true;
                    }
                }
                for failure in artifacts.failures {
                    if !report {
                        eprintln!("Debug check failed ({}) in {label}", failure.kind.label());
                        if failure.kind == CheckKind::FormatError {
                            eprintln!("  {}", failure.left);
                        }
                    }
                    collected.push((label.clone(), failure));
                }
            }
        }
    }

    if report {
        print!("{}", build_debug_report(checks, files_checked, &collected));
    } else if collected.is_empty() && !io_failed {
        println!(
            "All checks passed (checks: {}, files: {files_checked})",
            checks_label(checks)
        );
    }
    Ok(if collected.is_empty() && !io_failed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
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

fn run_format(
    paths: Vec<PathBuf>,
    check: bool,
    style: FormatStyle,
    exclude: &ExcludeFilter,
    quiet: bool,
) -> Result<ExitCode, String> {
    // No paths: format stdin to stdout.
    if paths.is_empty() {
        let text = read_source(None)?;
        let formatted = formatter::format_with_style(&text, style).map_err(|e| e.to_string())?;
        print!("{formatted}");
        return Ok(ExitCode::SUCCESS);
    }

    if check {
        let result = formatter::check_paths(&paths, style, exclude).map_err(|e| e.to_string())?;
        for changed in &result.changed {
            println!("would reformat {}", changed.path.display());
            // `--check` writes nothing, so the diff is normally the only account
            // of what would change; `--quiet` trades it for the file list plus a
            // summary, for callers (a CI step over a wholly unformatted project)
            // that would drown in hunks.
            if !quiet {
                print!("{}", changed.diff);
            }
        }
        if quiet && !result.changed.is_empty() {
            println!(
                "{} of {} file(s) would be reformatted",
                result.changed.len(),
                result.checked
            );
        }
        return Ok(if result.changed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let files =
        fatou::file_discovery::collect_julia_files(&paths, exclude).map_err(|e| e.to_string())?;
    files.par_iter().try_for_each(|path| {
        let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let formatted =
            formatter::format_with_style(&original, style).map_err(|e| e.to_string())?;
        if formatted != original {
            std::fs::write(path, formatted).map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    })?;
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn run_lint(
    paths: Vec<PathBuf>,
    output: LintOutput,
    fix: bool,
    unsafe_fixes: bool,
    color: ColorChoice,
    config: &Config,
    exclude: &ExcludeFilter,
    julia_target: Option<fatou::julia_version::VersionRange>,
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
        return run_lint_fix(paths, mode, unsafe_fixes, color, config, exclude);
    }

    let use_color = color_enabled(color, std::io::stderr().is_terminal());

    // A resolution-dependent rule (`undefined-name`, `call-arity`) resolves
    // names across files: harvest the enclosing project so a sibling file's
    // `using`/`import` and same-module globals resolve exactly as in the
    // language server. The default lint selects neither, so it pays no harvest.
    let library = if wants_project_resolution(&config.lint) {
        harvest_project(&paths)
    } else {
        None
    };
    let project = match &library {
        Some(lib) => linter::ProjectContext::Harvested(lib),
        None => linter::ProjectContext::SystemOnly,
    };

    let result =
        linter::check_paths_with_config(&paths, &config.lint, exclude, julia_target, project)
            .map_err(|e| e.to_string())?;
    warn_unknown_rules(&result.unknown_rules);

    let diagnostics: Vec<_> = result
        .reports
        .iter()
        .flat_map(|report| report.diagnostics.clone())
        .collect();
    let rendered =
        linter::render_findings(&diagnostics, RenderOptions::new(mode, use_color), &|path| {
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

/// Whether any selected rule needs project-wide name resolution. Both such
/// rules are default-off (they require project context to be sound), so a plain
/// `select` check suffices and keeps the harvest off the default `fatou lint`.
fn wants_project_resolution(config: &fatou::config::LintConfig) -> bool {
    const RESOLUTION_RULES: [&str; 2] = ["undefined-name", "call-arity"];
    RESOLUTION_RULES.iter().any(|id| {
        config
            .select
            .as_deref()
            .is_some_and(|sel| sel.iter().any(|r| r == id))
            && !config.ignore.iter().any(|r| r == id)
    })
}

/// Harvest the environment enclosing the lint targets — Base/Core/stdlib and
/// manifest dependencies plus the workspace package — cached and in parallel,
/// exactly as the language server does. `None` when no environment resolves (a
/// loose file outside any project), which leaves the lint in single-file mode.
fn harvest_project(paths: &[PathBuf]) -> Option<fatou::index::HarvestedLibrary> {
    use fatou::environment::{self, EnvContext};
    let env = environment::resolve(&EnvContext::from_process(lint_anchor(paths)))
        .ok()
        .flatten()?;
    let pool = rayon::ThreadPoolBuilder::new().build().ok()?;
    let cache = fatou::index::IndexCache::open();
    Some(fatou::index::harvest_libraries_parallel(
        std::slice::from_ref(&env),
        cache.as_ref(),
        &pool,
    ))
}

/// The directory to resolve the environment from: the first target's directory
/// (its parent when it is a file), else the current directory.
fn lint_anchor(paths: &[PathBuf]) -> PathBuf {
    let first = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    let anchor = if first.is_file() {
        first.parent().map(Path::to_path_buf).unwrap_or(first)
    } else {
        first
    };
    std::path::absolute(&anchor).unwrap_or(anchor)
}

/// Apply fixes across every discovered file, writing changed files back, then
/// report whatever findings remain. Exits non-zero if any remain (Ruff-style).
fn run_lint_fix(
    paths: Vec<PathBuf>,
    mode: OutputMode,
    unsafe_fixes: bool,
    color: ColorChoice,
    config: &Config,
    exclude: &ExcludeFilter,
) -> Result<ExitCode, String> {
    let use_color = color_enabled(color, std::io::stderr().is_terminal());
    let (_, unknown_rules) = linter::ResolvedRules::resolve(&config.lint);
    warn_unknown_rules(&unknown_rules);
    let files =
        fatou::file_discovery::collect_julia_files(&paths, exclude).map_err(|e| e.to_string())?;

    // Fix files in parallel; each writes back to its own path. Per-file results
    // are reduced afterward so counts and the `remaining` list stay stable.
    let outcomes = files
        .par_iter()
        .map(|path| {
            let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let outcome = linter::fix_source(Some(path), &original, &config.lint, unsafe_fixes);
            let changed = outcome.output != original;
            if changed {
                std::fs::write(path, &outcome.output).map_err(|e| e.to_string())?;
            }
            Ok::<_, String>((outcome.applied, changed, outcome.remaining))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut applied = 0usize;
    let mut changed_files = 0usize;
    let mut remaining = Vec::new();
    for (file_applied, changed, file_remaining) in outcomes {
        applied += file_applied;
        if changed {
            changed_files += 1;
        }
        remaining.extend(file_remaining);
    }

    let rendered =
        linter::render_findings(&remaining, RenderOptions::new(mode, use_color), &|path| {
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
        eprintln!("warning: unknown rule `{id}` in select/ignore/severity");
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

/// Resolve the effective Julia target range for version-compat checks, by
/// precedence: the `--julia-version` flag, then `[julia] version` from
/// `fatou.toml`, then the project's `Project.toml` `[compat]` / `Manifest.toml`
/// discovered from `anchor`. `None` (nothing declared) leaves the check silent.
///
/// A malformed `--julia-version` is reported as a warning and ignored, matching
/// how the config layer treats a bad `[julia] version`.
fn resolve_julia_target(
    cli: Option<&str>,
    config: &Config,
    anchor: &Path,
) -> Option<fatou::julia_version::VersionRange> {
    if let Some(spec) = cli {
        return match fatou::julia_version::parse_compat(spec) {
            Ok(range) => Some(range),
            Err(_) => {
                eprintln!("warning: --julia-version `{spec}` is not a valid Julia version");
                None
            }
        };
    }
    if let Some(range) = config.julia.version {
        return Some(range);
    }
    fatou::environment::discover_julia_target(anchor)
}

fn style_with_overrides(
    config: &Config,
    line_width: Option<u32>,
    indent_width: Option<u32>,
) -> FormatStyle {
    let mut style = FormatStyle::from(&config.format);
    if let Some(width) = line_width {
        style.line_width = width;
    }
    if let Some(width) = indent_width {
        style.indent_width = width;
    }
    style
}

/// Build the file-discovery exclude filter from the resolved config plus any
/// `--exclude` CLI patterns, applying `--force-exclude`. Patterns resolve
/// relative to the directory holding a project `fatou.toml`, or the working
/// directory for the env and global configs and the no-config case.
fn resolve_exclude_filter(
    config: &Config,
    source: &ConfigSource,
    cli_patterns: &[String],
    force: bool,
) -> Result<ExcludeFilter, String> {
    let anchor = std::env::current_dir().map_err(|e| e.to_string())?;
    let filter = config
        .exclude_filter(source, &anchor, cli_patterns)
        .map_err(|e| e.to_string())?;
    Ok(filter.with_force_exclude(force))
}

/// Resolve the config, returning it alongside the source it came from, needed
/// to root exclude patterns relative to the right directory.
fn load_config(
    explicit_config: &Option<PathBuf>,
    no_config: bool,
) -> Result<(Config, ConfigSource), String> {
    let anchor = std::env::current_dir().map_err(|e| e.to_string())?;
    let (config, source, warnings) =
        Config::resolve(explicit_config.as_deref(), no_config, &anchor)
            .map_err(|e| e.to_string())?;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    Ok((config, source))
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

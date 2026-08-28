//! mdBook preprocessor for the Fatou docs.
//!
//! It substitutes four markers on the performance page with content rendered
//! from the committed benchmark artifact `bench/results.json` (produced by
//! `bench/compare_format.sh`; never regenerated here):
//!
//!   `{{ benchmark-meta }}`       -> a bullet list of corpus, versions, and host
//!   `{{ benchmark-results-single }}`  -> a Vega-Lite dot plot of the warm-loop
//!   `{{ benchmark-results-project }}`    single-file and project scenarios
//!                                   respectively (time relative to Fatou, one dot
//!                                   per scenario stacked at each tool, log axis),
//!                                   each plus a collapsed fallback table.
//!   `{{ benchmark-cold-start }}` -> a log-scale dot plot of cold-start
//!                                   (fresh-process) time relative to Fatou per
//!                                   tool, plus a collapsed fallback table.
//!
//! The chart itself is drawn client-side by `docs/theme/bench-charts.js` from an
//! inline JSON payload; this crate only shapes the data and the fallback.
//!
//! The same page's language-server speed and memory markers come from a second
//! artifact and live in
//! [`memory`].

mod memory;

use mdbook_preprocessor::book::Book;
use mdbook_preprocessor::errors::Result;
use mdbook_preprocessor::{Preprocessor, PreprocessorContext};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

/// Preprocessing entry point.
pub fn handle_preprocessing() -> Result<()> {
    let pre = DocUtils;
    let (ctx, book) = mdbook_preprocessor::parse_input(io::stdin())?;

    let book_version = Version::parse(&ctx.mdbook_version)?;
    let version_req = VersionReq::parse(mdbook_preprocessor::MDBOOK_VERSION)?;
    if !version_req.matches(&book_version) {
        eprintln!(
            "warning: The {} plugin was built against version {} of mdbook, \
             but we're being called from version {}",
            pre.name(),
            mdbook_preprocessor::MDBOOK_VERSION,
            ctx.mdbook_version
        );
    }

    let processed_book = pre.run(&ctx, book)?;
    serde_json::to_writer(io::stdout(), &processed_book)?;
    Ok(())
}

struct DocUtils;

impl Preprocessor for DocUtils {
    fn name(&self) -> &str {
        "doc-utils"
    }

    fn run(&self, _ctx: &PreprocessorContext, mut book: Book) -> Result<Book> {
        insert_benchmarks(&mut book);
        memory::insert(&mut book, project_root());
        Ok(book)
    }
}

/// The project root, one level up from the book root (`docs/`), which is the
/// working directory mdbook runs preprocessors in.
fn project_root() -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

// --- Benchmark artifact schema (mirrors bench/merge.py output) ---------------

const BENCH_META_MARKER: &str = "{{ benchmark-meta }}";
const BENCH_RESULTS_SINGLE_MARKER: &str = "{{ benchmark-results-single }}";
const BENCH_RESULTS_PROJECT_MARKER: &str = "{{ benchmark-results-project }}";
const BENCH_COLD_MARKER: &str = "{{ benchmark-cold-start }}";

/// Scenario order comes from the artifact's own `scenario_order` list (set by
/// `bench/compare_format.sh`), so adding a corpus or a single-file target needs
/// no change here. Tools are rendered in this fixed order within each scenario,
/// so every table reads Fatou -> Runic -> JuliaFormatter.
const TOOL_ORDER: &[(&str, &str)] = &[
    ("fatou", "Fatou"),
    ("runic", "Runic"),
    ("juliaformatter", "JuliaFormatter"),
];

#[derive(Deserialize)]
struct Benchmarks {
    meta: Meta,
    #[serde(default)]
    scenario_order: Vec<String>,
    scenarios: HashMap<String, Scenario>,
}

impl Benchmarks {
    /// The warm-loop scenarios of one scope, in render order. `cold_start` is not
    /// in `scenario_order`; it has its own marker.
    fn ordered(&self, scope: Scope) -> Vec<(&str, &Scenario)> {
        self.scenario_order
            .iter()
            .filter(|k| scope.owns(k))
            .filter_map(|k| self.scenarios.get(k.as_str()).map(|sc| (k.as_str(), sc)))
            .collect()
    }
}

/// The two warm-loop scopes, each rendered as its own chart: one file through a
/// tool's pure `String -> String` formatter, and a whole source tree through its
/// directory entry point. Scenario keys carry the scope as a prefix, the naming
/// `bench/compare_format.sh` gives them.
#[derive(Clone, Copy)]
enum Scope {
    Single,
    Project,
}

impl Scope {
    /// Whether a scenario key belongs to this scope.
    fn owns(self, key: &str) -> bool {
        key.starts_with(self.key_prefix())
    }

    fn key_prefix(self) -> &'static str {
        match self {
            Scope::Single => "single_",
            Scope::Project => "project_",
        }
    }

    /// The redundant part of a scenario's display label inside its own chart:
    /// with the scope in the surrounding heading, "Single file: kinds.jl" is just
    /// "kinds.jl".
    fn label_prefix(self) -> &'static str {
        match self {
            Scope::Single => "Single file: ",
            Scope::Project => "Project: ",
        }
    }

    /// What one dot stands for, used as the chart's legend title.
    fn legend_title(self) -> &'static str {
        match self {
            Scope::Single => "File",
            Scope::Project => "Project",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            Scope::Single => {
                "Formatting time relative to Fatou on a log scale (lower is faster). One dot \
                 per file, grouped at each tool and colored by file; Fatou sits on the dashed \
                 baseline at 1 and slower tools appear above it. Each file goes through the \
                 tool's pure <code>String -&gt; String</code> formatter, in the tool's own \
                 default style. Hover a dot for the exact figures."
            }
            Scope::Project => {
                "Formatting time relative to Fatou on a log scale (lower is faster). One dot \
                 per project, grouped at each tool and colored by project; Fatou sits on the \
                 dashed baseline at 1 and slower tools appear above it. Each tool walks the \
                 whole source tree through its own directory entry point, so file discovery, \
                 IO, and internal parallelism all count; <code>Runic</code> is absent because \
                 it has no in-process directory API. Hover a dot for the exact figures."
            }
        }
    }
}

#[derive(Deserialize)]
struct Meta {
    host: String,
    os: String,
    cpu: String,
    warmup: u32,
    #[serde(default)]
    iterations_single: Option<u64>,
    #[serde(default)]
    iterations_project: Option<u64>,
    #[serde(default)]
    iterations_cold: Option<u64>,
    #[serde(default)]
    corpora: Vec<Corpus>,
    versions: Versions,
}

#[derive(Deserialize)]
struct Corpus {
    name: String,
    repo: String,
    tag: String,
    commit: String,
}

#[derive(Deserialize)]
struct Versions {
    fatou: Option<String>,
    julia: Option<String>,
    runic: Option<String>,
    juliaformatter: Option<String>,
}

#[derive(Deserialize)]
struct Scenario {
    /// Display name from the artifact, e.g. "Single file: kinds.jl".
    #[serde(default)]
    label: String,
    target: String,
    tools: HashMap<String, Agg>,
}

#[derive(Deserialize)]
struct Agg {
    files_ok: u64,
    total_bytes: u64,
    median_total_ns: f64,
    throughput_mbps: f64,
    #[serde(default)]
    skipped: Vec<Skipped>,
}

#[derive(Deserialize)]
struct Skipped {
    file: String,
    reason: String,
}

/// One dot in the warm-loop chart. `relative_time` (this tool's median time as a
/// multiple of Fatou's in the same scenario) is the quantity plotted on the log
/// axis; the rest feed the tooltip. Serialized inline for `bench-charts.js`.
#[derive(Serialize)]
struct ChartPoint {
    scenario: String,
    tool: String,
    relative_time: f64,
    throughput_mbps: f64,
    files_ok: u64,
    total_bytes: u64,
    median_ms: f64,
    relative: String,
}

/// One dot in the cold-start chart: a tool's cold time as a multiple of Fatou's
/// (`relative_time`, plotted on the log axis) plus tooltip numbers.
#[derive(Serialize)]
struct ColdPoint {
    tool: String,
    relative_time: f64,
    median_ms: f64,
    throughput_mbps: f64,
    relative: String,
}

/// Substitute the benchmark markers with content rendered from the committed
/// `bench/results.json`. The JSON is read but never regenerated here, so the
/// benchmark is only ever run manually (via `task bench`), not at build time.
fn insert_benchmarks(book: &mut Book) {
    const MARKERS: &[&str] = &[
        BENCH_META_MARKER,
        BENCH_RESULTS_SINGLE_MARKER,
        BENCH_RESULTS_PROJECT_MARKER,
        BENCH_COLD_MARKER,
    ];

    let needs_render = {
        let mut found = false;
        book.for_each_chapter_mut(|ch| {
            if MARKERS.iter().any(|m| ch.content.contains(m)) {
                found = true;
            }
        });
        found
    };
    if !needs_render {
        return;
    }

    let path = project_root().join("bench/results.json");
    let rendered: Vec<String> = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Benchmarks>(&s).ok())
    {
        Some(b) => vec![
            render_meta(&b.meta),
            render_results(&b, Scope::Single),
            render_results(&b, Scope::Project),
            render_cold_start(&b),
        ],
        None => {
            let note = format!(
                "_Benchmark data unavailable (`{}` missing or unreadable; run `task bench`)._",
                path.display()
            );
            vec![note; MARKERS.len()]
        }
    };

    book.for_each_chapter_mut(|ch| {
        for (marker, content) in MARKERS.iter().zip(&rendered) {
            if ch.content.contains(marker) {
                ch.content = ch.content.replace(marker, content);
            }
        }
    });
}

/// A scenario's display name within its own scope's chart: the label the
/// benchmark artifact recorded, minus the scope prefix the heading already
/// carries, or the raw key as a fallback for an artifact predating labelled
/// scenarios.
fn scenario_label(key: &str, sc: &Scenario, scope: Scope) -> String {
    if sc.label.is_empty() {
        return key.to_string();
    }
    sc.label
        .strip_prefix(scope.label_prefix())
        .unwrap_or(&sc.label)
        .to_string()
}

/// A Markdown bullet list of corpus pins, tool versions, host, and run settings.
fn render_meta(meta: &Meta) -> String {
    let v = &meta.versions;

    let mut versions = Vec::new();
    if let Some(s) = &v.fatou {
        versions.push(format!("**Fatou** `{s}`"));
    }
    match &v.runic {
        Some(s) => versions.push(format!("**Runic** `{s}`")),
        None => versions.push("**Runic** not measured".to_string()),
    }
    match &v.juliaformatter {
        Some(s) => versions.push(format!("**JuliaFormatter** `{s}`")),
        None => versions.push("**JuliaFormatter** not measured".to_string()),
    }
    if let Some(s) = &v.julia {
        versions.push(format!("Julia `{s}`"));
    }

    let iters = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string());

    let mut out = String::new();
    let corpora: Vec<String> = meta
        .corpora
        .iter()
        .map(|c| format!("[{}.jl]({}) `{}` ({})", c.name, c.repo, c.tag, c.commit))
        .collect();
    if !corpora.is_empty() {
        out.push_str(&format!("- **Corpora**: {}\n", corpora.join(", ")));
    }
    out.push_str(&format!("- **Versions**: {}\n", versions.join(", ")));
    out.push_str(&format!("- **Host**: {} ({})\n", meta.cpu, meta.os));
    out.push_str(&format!("- **Machine**: `{}`\n", meta.host));
    out.push_str(&format!(
        "- **Warm-loop iterations**: {} single, {} project; {} warmup\n",
        iters(meta.iterations_single),
        iters(meta.iterations_project),
        meta.warmup,
    ));
    if let Some(n) = meta.iterations_cold {
        out.push_str(&format!(
            "- **Cold-start iterations**: {n} fresh-process runs (single file)\n"
        ));
    }
    out
}

/// One scope's results marker becomes an interactive dot plot (Vega-Lite, driven
/// by `docs/theme/bench-charts.js` and wired via `book.toml`'s `additional-js`)
/// plus a collapsed HTML table with the same numbers as a no-JS/print fallback.
/// Single files and projects get a chart each: they measure different work at
/// different sizes, and a shared axis buries that.
fn render_results(b: &Benchmarks, scope: Scope) -> String {
    let points = chart_points(b, scope);
    if points.is_empty() {
        return "_No scenarios of this kind in the benchmark artifact (run `task bench`)._"
            .to_string();
    }
    let data_json = serde_json::to_string(&points).unwrap_or_else(|_| "[]".to_string());

    let mut out = String::new();
    out.push_str("<div class=\"bench-chart-block\">\n");
    out.push_str("<figure class=\"bench-figure\">\n");
    out.push_str(&format!(
        "<div class=\"bench-chart\" data-legend=\"{}\"></div>\n",
        scope.legend_title()
    ));
    out.push_str("<script type=\"application/json\" class=\"bench-data\">");
    out.push_str(&data_json);
    out.push_str("</script>\n");
    out.push_str(&format!("<figcaption>{}</figcaption>\n", scope.caption()));
    out.push_str("</figure>\n");
    out.push_str(
        "<noscript>Enable JavaScript for the interactive chart; \
         the data table below has the same numbers.</noscript>\n",
    );
    out.push_str("<details class=\"bench-table\">\n<summary>Data table</summary>\n");
    out.push_str(&render_results_tables_html(b, scope));
    out.push_str("</details>\n");
    out.push_str("</div>\n");
    out
}

/// One dot per (scenario, tool) within a scope, in scenario then tool order. The
/// plotted value is each tool's median time as a multiple of Fatou's in the same
/// scenario (Fatou = 1); absolute throughput and time ride along in the tooltip.
fn chart_points(b: &Benchmarks, scope: Scope) -> Vec<ChartPoint> {
    let mut points = Vec::new();
    for (key, sc) in b.ordered(scope) {
        let label = scenario_label(key, sc, scope);
        let base = sc.tools.get("fatou").map(|a| a.median_total_ns);
        for &(tool, tool_label) in TOOL_ORDER {
            let Some(agg) = sc.tools.get(tool) else {
                continue;
            };
            let (relative_time, relative) = relative_time(tool, agg.median_total_ns, base);
            points.push(ChartPoint {
                scenario: label.clone(),
                tool: tool_label.to_string(),
                relative_time,
                throughput_mbps: agg.throughput_mbps,
                files_ok: agg.files_ok,
                total_bytes: agg.total_bytes,
                median_ms: agg.median_total_ns / 1e6,
                relative,
            });
        }
    }
    points
}

/// One `<h4>` + HTML `<table>` per scenario of a scope, in scenario order; rows
/// follow tool order. `Relative` is each tool's median time as a multiple of
/// Fatou's (what the chart plots), so the table and the dot plot tell the same
/// story. The heading level sits one below the scope's own `###` on the page.
fn render_results_tables_html(b: &Benchmarks, scope: Scope) -> String {
    let mut out = String::new();
    for (key, sc) in b.ordered(scope) {
        let base = sc.tools.get("fatou").map(|a| a.median_total_ns);

        out.push_str(&format!(
            "<h4>{} (<code>{}</code>)</h4>\n",
            esc(&scenario_label(key, sc, scope)),
            esc(&sc.target)
        ));
        out.push_str(
            "<table>\n<thead><tr><th>Tool</th><th>Files</th><th>Bytes</th>\
             <th>Median (ms)</th><th>Throughput (MB/s)</th><th>Relative</th></tr></thead>\n<tbody>\n",
        );
        for &(tool, tool_label) in TOOL_ORDER {
            let Some(agg) = sc.tools.get(tool) else {
                continue;
            };
            let (_, relative) = relative_time(tool, agg.median_total_ns, base);
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{:.2}</td><td>{}</td></tr>\n",
                tool_label,
                agg.files_ok,
                thousands(agg.total_bytes),
                agg.median_total_ns / 1e6,
                agg.throughput_mbps,
                esc(&relative),
            ));
        }
        out.push_str("</tbody>\n</table>\n");

        // Note any skipped files (e.g. JuliaFormatter cannot parse parser.jl).
        for &(tool, tool_label) in TOOL_ORDER {
            let Some(agg) = sc.tools.get(tool) else {
                continue;
            };
            for s in &agg.skipped {
                out.push_str(&format!(
                    "<p class=\"bench-skip\">{} skipped <code>{}</code>: {}</p>\n",
                    tool_label,
                    esc(&s.file),
                    esc(&s.reason),
                ));
            }
        }
    }
    out
}

/// The cold-start marker becomes a log-scale dot plot of each tool's median
/// fresh-process time relative to Fatou (startup plus, for the Julia tools,
/// package load and first-call JIT), Fatou on a dashed baseline at 1, plus a
/// collapsed HTML fallback table with the absolute numbers. A log axis keeps the
/// points readable across the wide dynamic range. (Points, not bars: a log scale
/// has no zero baseline for bars to grow from.)
fn render_cold_start(b: &Benchmarks) -> String {
    let Some(sc) = b.scenarios.get("cold_start") else {
        return "_Cold-start data unavailable (run `task bench`)._".to_string();
    };
    let points = cold_points(sc);
    let data_json = serde_json::to_string(&points).unwrap_or_else(|_| "[]".to_string());

    let mut out = String::new();
    out.push_str("<div class=\"bench-chart-block\">\n");
    out.push_str("<figure class=\"bench-figure\">\n");
    out.push_str("<div class=\"bench-chart\" data-kind=\"cold\"></div>\n");
    out.push_str("<script type=\"application/json\" class=\"bench-data\">");
    out.push_str(&data_json);
    out.push_str("</script>\n");
    out.push_str(
        "<figcaption>Median cold-start time relative to Fatou on a logarithmic scale \
         (lower is faster). Fatou is the dashed baseline at 1; each Julia tool sits above at \
         its slowdown factor. Each run is a brand-new process that starts up, formats once, \
         and exits. Fatou runs through <code>fatou format</code>; the Julia tools run through \
         the same <code>julia -e 'using ...'</code> path a shell user takes, so Julia startup, \
         package load, and first-call compilation all count. Hover a dot for the exact \
         figures.</figcaption>\n",
    );
    out.push_str("</figure>\n");
    out.push_str(
        "<noscript>Enable JavaScript for the interactive chart; \
         the data table below has the same numbers.</noscript>\n",
    );
    out.push_str("<details class=\"bench-table\">\n<summary>Data table</summary>\n");
    out.push_str(&render_cold_table_html(sc));
    out.push_str("</details>\n");
    out.push_str("</div>\n");
    out
}

/// One dot per tool for the cold-start chart, in tool order; the plotted value is
/// each tool's cold time as a multiple of Fatou's (Fatou = 1, log axis).
fn cold_points(sc: &Scenario) -> Vec<ColdPoint> {
    let base = sc.tools.get("fatou").map(|a| a.median_total_ns);
    let mut points = Vec::new();
    for &(tool, tool_label) in TOOL_ORDER {
        let Some(agg) = sc.tools.get(tool) else {
            continue;
        };
        let (relative_time, relative) = relative_time(tool, agg.median_total_ns, base);
        points.push(ColdPoint {
            tool: tool_label.to_string(),
            relative_time,
            median_ms: agg.median_total_ns / 1e6,
            throughput_mbps: agg.throughput_mbps,
            relative,
        });
    }
    points
}

/// The cold-start fallback table: absolute median time, throughput, and time
/// relative to Fatou (what the dot plot shows).
fn render_cold_table_html(sc: &Scenario) -> String {
    let base = sc.tools.get("fatou").map(|a| a.median_total_ns);
    let mut out = String::new();
    out.push_str(&format!(
        "<table>\n<caption>Cold start: <code>{}</code>, one fresh process per run</caption>\n",
        esc(&sc.target)
    ));
    out.push_str(
        "<thead><tr><th>Tool</th><th>Median time</th>\
         <th>Throughput (MB/s)</th><th>vs Fatou</th></tr></thead>\n<tbody>\n",
    );
    for &(tool, tool_label) in TOOL_ORDER {
        let Some(agg) = sc.tools.get(tool) else {
            continue;
        };
        let (_, relative) = relative_time(tool, agg.median_total_ns, base);
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.2}</td><td>{}</td></tr>\n",
            tool_label,
            fmt_time(agg.median_total_ns),
            agg.throughput_mbps,
            esc(&relative),
        ));
    }
    out.push_str("</tbody>\n</table>\n");
    out
}

/// A duration in nanoseconds as a compact human string, choosing the unit by
/// magnitude: seconds for cold Julia runs, milliseconds for Fatou.
fn fmt_time(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.2} s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.1} ms", ns / 1e6)
    } else {
        format!("{:.0} µs", ns / 1e3)
    }
}

/// A tool's median time as a multiple of Fatou's, returned as both the raw ratio
/// (plotted on the log axis) and a display label (`baseline` for Fatou, else e.g.
/// `3.41x`). Fatou is always exactly 1; a missing/zero baseline yields `—`.
fn relative_time(tool: &str, ns: f64, base: Option<f64>) -> (f64, String) {
    if tool == "fatou" {
        return (1.0, "baseline".to_string());
    }
    match base {
        Some(b) if b > 0.0 => {
            let ratio = ns / b;
            (ratio, format!("{ratio:.2}x"))
        }
        _ => (1.0, "—".to_string()),
    }
}

/// Minimal HTML text escaping for the fallback table's cell text.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Group a byte count with thousands separators, e.g. `123456` -> `123,456`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

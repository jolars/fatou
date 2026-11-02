//! Benchmark renderer for the Fatou docs.
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
//! The chart itself is drawn client-side by the VitePress theme from an inline
//! JSON payload; this crate only shapes the data and the fallback.
//!
//! The same page's language-server speed and memory markers come from a second
//! artifact and live in
//! [`memory`].

mod memory;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Render the committed benchmark artifacts without running measurements.
pub fn render(project_root: &Path) -> BTreeMap<&'static str, String> {
    let mut replacements = BTreeMap::new();
    insert_benchmarks(&mut replacements, project_root);
    memory::insert(&mut replacements, project_root);
    replacements
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
                "Median formatting time relative to Fatou (lower is faster). \
                 Each color is one file; the dashed line at 1 is Fatou."
            }
            Scope::Project => {
                "Median formatting time relative to Fatou (lower is faster). \
                 Each color is one project; the dashed line at 1 is Fatou."
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
fn insert_benchmarks(replacements: &mut BTreeMap<&'static str, String>, project_root: &Path) {
    const MARKERS: &[&str] = &[
        BENCH_META_MARKER,
        BENCH_RESULTS_SINGLE_MARKER,
        BENCH_RESULTS_PROJECT_MARKER,
        BENCH_COLD_MARKER,
    ];

    let path = project_root.join("bench/results.json");
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

    for (&marker, content) in MARKERS.iter().zip(rendered) {
        replacements.insert(marker, content);
    }
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
/// by `docs/.vitepress/theme/bench-charts.js` in the VitePress theme)
/// plus an HTML data table that collapses after the chart renders.
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
    out.push_str("<details class=\"bench-table\" open>\n<summary>Data table</summary>\n");
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
/// an HTML data table with the absolute numbers. A log axis keeps the
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
        "<figcaption>Median cold-start time relative to Fatou (lower is faster). \
         The dashed line at 1 is Fatou. Each run starts a fresh process and \
         formats one file.</figcaption>\n",
    );
    out.push_str("</figure>\n");
    out.push_str(
        "<noscript>Enable JavaScript for the interactive chart; \
         the data table below has the same numbers.</noscript>\n",
    );
    out.push_str("<details class=\"bench-table\" open>\n<summary>Data table</summary>\n");
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

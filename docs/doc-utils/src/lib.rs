//! mdBook preprocessor for the Fatou docs.
//!
//! It substitutes two markers on the performance page with content rendered
//! from the committed benchmark artifact `bench/results.json` (produced by
//! `bench/compare_format.sh`; never regenerated here):
//!
//!   `{{ benchmark-meta }}`     -> a bullet list of corpus, versions, and host
//!   `{{ benchmark-results }}`  -> a Vega-Lite grouped bar chart (throughput per
//!                                 scenario, one bar per tool) plus a collapsed
//!                                 HTML fallback table with the same numbers.
//!
//! The chart itself is drawn client-side by `docs/theme/bench-charts.js` from an
//! inline JSON payload; this crate only shapes the data and the fallback.

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
const BENCH_RESULTS_MARKER: &str = "{{ benchmark-results }}";

/// Scenarios and tools are rendered in this fixed order regardless of the map
/// order in the JSON, so the page reads single -> project and, within each,
/// Fatou -> Runic -> JuliaFormatter.
const SCENARIO_ORDER: &[(&str, &str)] = &[("single_file", "Single file"), ("project", "Project")];
const TOOL_ORDER: &[(&str, &str)] = &[
    ("fatou", "Fatou"),
    ("runic", "Runic"),
    ("juliaformatter", "JuliaFormatter"),
];

#[derive(Deserialize)]
struct Benchmarks {
    meta: Meta,
    scenarios: HashMap<String, Scenario>,
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
    corpus: Corpus,
    versions: Versions,
}

#[derive(Deserialize)]
struct Corpus {
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

/// One bar in the chart: a (scenario, tool) throughput and the numbers its
/// tooltip shows. Serialized inline for `docs/theme/bench-charts.js`.
#[derive(Serialize)]
struct ChartPoint {
    scenario: String,
    tool: String,
    throughput_mbps: f64,
    files_ok: u64,
    total_bytes: u64,
    median_ms: f64,
    relative: String,
}

/// Substitute the benchmark markers with content rendered from the committed
/// `bench/results.json`. The JSON is read but never regenerated here, so the
/// benchmark is only ever run manually (via `task bench`), not at build time.
fn insert_benchmarks(book: &mut Book) {
    let needs_render = {
        let mut found = false;
        book.for_each_chapter_mut(|ch| {
            if ch.content.contains(BENCH_META_MARKER) || ch.content.contains(BENCH_RESULTS_MARKER) {
                found = true;
            }
        });
        found
    };
    if !needs_render {
        return;
    }

    let path = project_root().join("bench/results.json");
    let (meta, results) = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Benchmarks>(&s).ok())
    {
        Some(b) => (render_meta(&b.meta), render_results(&b)),
        None => {
            let note = format!(
                "_Benchmark data unavailable (`{}` missing or unreadable; run `task bench`)._",
                path.display()
            );
            (note.clone(), note)
        }
    };

    book.for_each_chapter_mut(|ch| {
        if ch.content.contains(BENCH_META_MARKER) {
            ch.content = ch.content.replace(BENCH_META_MARKER, &meta);
        }
        if ch.content.contains(BENCH_RESULTS_MARKER) {
            ch.content = ch.content.replace(BENCH_RESULTS_MARKER, &results);
        }
    });
}

/// A Markdown bullet list of corpus pin, tool versions, host, and run settings.
fn render_meta(meta: &Meta) -> String {
    let c = &meta.corpus;
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
    out.push_str(&format!(
        "- **Corpus**: [JuliaSyntax.jl]({}) `{}` ({})\n",
        c.repo, c.tag, c.commit
    ));
    out.push_str(&format!("- **Versions**: {}\n", versions.join(", ")));
    out.push_str(&format!("- **Host**: {} ({})\n", meta.cpu, meta.os));
    out.push_str(&format!("- **Machine**: `{}`\n", meta.host));
    out.push_str(&format!(
        "- **Warm-loop iterations**: {} single, {} project; {} warmup\n",
        iters(meta.iterations_single),
        iters(meta.iterations_project),
        meta.warmup,
    ));
    out
}

/// The results marker becomes an interactive grouped bar chart (Vega-Lite,
/// driven by `docs/theme/bench-charts.js` and wired via `book.toml`'s
/// `additional-js`) plus a collapsed HTML table with the same numbers as a
/// no-JS/print fallback.
fn render_results(b: &Benchmarks) -> String {
    let points = chart_points(b);
    let data_json = serde_json::to_string(&points).unwrap_or_else(|_| "[]".to_string());

    let mut out = String::new();
    out.push_str("<div class=\"bench-chart-block\">\n");
    out.push_str("<figure class=\"bench-figure\">\n");
    out.push_str("<div class=\"bench-chart\"></div>\n");
    out.push_str("<script type=\"application/json\" class=\"bench-data\">");
    out.push_str(&data_json);
    out.push_str("</script>\n");
    out.push_str(
        "<figcaption>Formatting throughput in megabytes per second (higher is faster). \
         Bars are grouped by scenario and colored by tool; each tool uses its own default \
         style. The <em>Project</em> scenario formats the whole source tree through each \
         tool's directory entry point; <code>Runic</code> has no in-process directory API, \
         so it is absent there. Hover a bar for the exact figures.</figcaption>\n",
    );
    out.push_str("</figure>\n");
    out.push_str(
        "<noscript>Enable JavaScript for the interactive chart; \
         the data table below has the same numbers.</noscript>\n",
    );
    out.push_str("<details class=\"bench-table\">\n<summary>Data table</summary>\n");
    out.push_str(&render_results_tables_html(b));
    out.push_str("</details>\n");
    out.push_str("</div>\n");
    out
}

/// One bar per (scenario, tool), in scenario then tool order. Each tool's
/// throughput is shown absolutely (MB/s) and, in the tooltip, relative to Fatou
/// in the same scenario.
fn chart_points(b: &Benchmarks) -> Vec<ChartPoint> {
    let mut points = Vec::new();
    for &(key, label) in SCENARIO_ORDER {
        let Some(sc) = b.scenarios.get(key) else {
            continue;
        };
        let base = sc.tools.get("fatou").map(|a| a.throughput_mbps);
        for &(tool, tool_label) in TOOL_ORDER {
            let Some(agg) = sc.tools.get(tool) else {
                continue;
            };
            points.push(ChartPoint {
                scenario: label.to_string(),
                tool: tool_label.to_string(),
                throughput_mbps: agg.throughput_mbps,
                files_ok: agg.files_ok,
                total_bytes: agg.total_bytes,
                median_ms: agg.median_total_ns / 1e6,
                relative: relative_cell(tool, agg.throughput_mbps, base),
            });
        }
    }
    points
}

/// One `<h3>` + HTML `<table>` per scenario, in scenario order; rows follow tool
/// order. `Relative` is each tool's throughput as a multiple of Fatou's.
fn render_results_tables_html(b: &Benchmarks) -> String {
    let mut out = String::new();
    for &(key, label) in SCENARIO_ORDER {
        let Some(sc) = b.scenarios.get(key) else {
            continue;
        };
        let base = sc.tools.get("fatou").map(|a| a.throughput_mbps);

        out.push_str(&format!(
            "<h3>{} (<code>{}</code>)</h3>\n",
            label,
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
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{:.2}</td><td>{}</td></tr>\n",
                tool_label,
                agg.files_ok,
                thousands(agg.total_bytes),
                agg.median_total_ns / 1e6,
                agg.throughput_mbps,
                esc(&relative_cell(tool, agg.throughput_mbps, base)),
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

/// Throughput of a tool as a multiple of the Fatou baseline (`baseline` for
/// Fatou itself), e.g. `0.65x`.
fn relative_cell(tool: &str, mbps: f64, base: Option<f64>) -> String {
    if tool == "fatou" {
        return "baseline".to_string();
    }
    match base {
        Some(b) if b > 0.0 => format!("{:.2}x", mbps / b),
        _ => "—".to_string(),
    }
}

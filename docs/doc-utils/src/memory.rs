//! Memory markers for the performance page, rendered from the committed
//! `bench/memory.json` (produced by `bench/memory_compare.sh`; never regenerated
//! here, exactly like the throughput artifact next door):
//!
//!   `{{ memory-meta }}`    -> a bullet list of workspace, session, and versions
//!   `{{ memory-servers }}` -> the language-server table, Fatou first
//!   `{{ memory-cli }}`     -> the one-shot CLI table
//!
//! These are plain Markdown tables rather than the charts the throughput
//! scenarios get. A chart earns its place when a reader has to compare many
//! points across two dimensions; three servers with three milestones each is a
//! table, and pretending otherwise would cost legibility to buy symmetry.

use mdbook_preprocessor::book::Book;
use serde::Deserialize;
use std::path::PathBuf;

pub const META_MARKER: &str = "{{ memory-meta }}";
pub const SERVERS_MARKER: &str = "{{ memory-servers }}";
pub const CLI_MARKER: &str = "{{ memory-cli }}";

const MARKERS: &[&str] = &[META_MARKER, SERVERS_MARKER, CLI_MARKER];

// --- artifact schema (mirrors bench/memory_merge.py output) ------------------

#[derive(Deserialize)]
struct Memory {
    meta: Meta,
    session: Session,
    #[serde(default)]
    servers: Vec<Server>,
    cli: Cli,
}

#[derive(Deserialize)]
struct Meta {
    host: String,
    os: String,
    cpu: String,
    #[serde(default)]
    memory_gb: Option<u64>,
    #[serde(default)]
    corpora: Vec<Corpus>,
    #[serde(default)]
    servers: ServerVersions,
    versions: Versions,
}

#[derive(Deserialize, Default)]
struct ServerVersions {
    #[serde(default)]
    languageserver: Option<LanguageServerPin>,
    #[serde(default)]
    jetls: Option<JetlsPin>,
}

#[derive(Deserialize)]
struct LanguageServerPin {
    version: String,
}

#[derive(Deserialize)]
struct JetlsPin {
    commit: String,
    #[serde(default)]
    date: Option<String>,
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
}

#[derive(Deserialize)]
struct Session {
    project: String,
    file_count: u64,
    total_bytes: u64,
}

#[derive(Deserialize)]
struct Server {
    label: String,
    #[serde(default)]
    doing: String,
    baseline_rss_mb: Option<f64>,
    settled_rss_mb: Option<f64>,
    peak_rss_mb: Option<f64>,
    #[serde(default)]
    processes_at_settle: Option<u64>,
    #[serde(default)]
    settled_seconds: Option<f64>,
    #[serde(default)]
    relative_to_fatou: Option<f64>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Deserialize)]
struct Cli {
    tree_files: u64,
    tree_bytes: u64,
    runs: u64,
    #[serde(default)]
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    command: String,
    /// What the command ran over: "src tree", "one file", or a floor marker.
    target: String,
    peak_rss_mb: f64,
    seconds: f64,
    #[serde(default)]
    input_bytes: u64,
}

// --- rendering ---------------------------------------------------------------

/// Substitute the memory markers, if the page uses any.
pub fn insert(book: &mut Book, project_root: PathBuf) {
    let mut needed = false;
    book.for_each_chapter_mut(|ch| {
        if MARKERS.iter().any(|m| ch.content.contains(m)) {
            needed = true;
        }
    });
    if !needed {
        return;
    }

    let path = project_root.join("bench/memory.json");
    let rendered: Vec<String> = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Memory>(&s).ok())
    {
        Some(m) => vec![render_meta(&m), render_servers(&m), render_cli(&m.cli)],
        None => {
            let note = format!(
                "_Memory data unavailable (`{}` missing or unreadable; run `task bench-memory`)._",
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

fn render_meta(m: &Memory) -> String {
    let meta = &m.meta;

    let mut versions = Vec::new();
    if let Some(s) = &meta.versions.fatou {
        versions.push(format!("**Fatou** `{s}`"));
    }
    match &meta.servers.languageserver {
        Some(ls) => versions.push(format!("**LanguageServer.jl** `{}`", ls.version)),
        None => versions.push("**LanguageServer.jl** not measured".to_string()),
    }
    match &meta.servers.jetls {
        Some(j) => versions.push(match &j.date {
            Some(d) => format!("**JETLS** `{}` ({d})", j.commit),
            None => format!("**JETLS** `{}`", j.commit),
        }),
        None => versions.push("**JETLS** not measured".to_string()),
    }
    if let Some(s) = &meta.versions.julia {
        versions.push(format!("Julia `{s}`"));
    }

    let mut out = String::new();
    if let Some(c) = meta
        .corpora
        .iter()
        .find(|c| m.session.project.contains(&c.name))
    {
        out.push_str(&format!(
            "- **Workspace**: [{}.jl]({}) `{}` ({}), instantiated\n",
            c.name, c.repo, c.tag, c.commit
        ));
    } else {
        out.push_str(&format!("- **Workspace**: {}\n", m.session.project));
    }
    out.push_str(&format!(
        "- **Session**: {} files opened ({} of source), diagnostics, symbols, and hovers\n",
        m.session.file_count,
        human_bytes(m.session.total_bytes)
    ));
    out.push_str(&format!("- **Versions**: {}\n", versions.join(", ")));
    match meta.memory_gb {
        Some(gb) => out.push_str(&format!(
            "- **Host**: {} ({}, {gb} GB RAM)\n",
            meta.cpu, meta.os
        )),
        None => out.push_str(&format!("- **Host**: {} ({})\n", meta.cpu, meta.os)),
    }
    out.push_str(&format!("- **Machine**: `{}`\n", meta.host));
    out
}

fn render_servers(m: &Memory) -> String {
    if m.servers.is_empty() {
        return "_No servers in the memory artifact (run `task bench-memory`)._".to_string();
    }

    let mut out = String::from(
        "| Server | Baseline | Settled | Peak | vs Fatou | Settled after | Doing |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for s in &m.servers {
        let relative = match s.relative_to_fatou {
            Some(r) if (r - 1.0).abs() < f64::EPSILON => "baseline".to_string(),
            Some(r) => format!("{r:.1}x"),
            None => "-".to_string(),
        };
        // A server that fanned work out to a helper is worth flagging inline:
        // its peak is the only milestone that saw the helper at all.
        let processes = match s.processes_at_settle {
            Some(n) if n > 1 => format!("{} ({n} processes)", s.doing),
            _ => s.doing.clone(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            s.label,
            megabytes(s.baseline_rss_mb),
            megabytes(s.settled_rss_mb),
            megabytes(s.peak_rss_mb),
            relative,
            s.settled_seconds
                .map(|v| format!("{v:.0} s"))
                .unwrap_or_else(|| "-".to_string()),
            processes,
        ));
    }

    let notes: Vec<String> = m
        .servers
        .iter()
        .flat_map(|s| s.notes.iter().map(move |n| format!("{}: {n}", s.label)))
        .collect();
    if !notes.is_empty() {
        out.push_str(&format!("\n_Harness notes: {}._\n", notes.join("; ")));
    }
    out
}

fn render_cli(cli: &Cli) -> String {
    if cli.cases.is_empty() {
        return "_No CLI cases in the memory artifact (run `task bench-memory`)._".to_string();
    }

    let mut out = String::from(
        "| Command | Over | Input | Peak RSS | Wall |\n| --- | --- | ---: | ---: | ---: |\n",
    );
    for c in &cli.cases {
        let input = if c.input_bytes == 0 {
            "-".to_string()
        } else {
            human_bytes(c.input_bytes)
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {:.1} MB | {:.0} ms |\n",
            c.command,
            c.target,
            input,
            c.peak_rss_mb,
            c.seconds * 1000.0
        ));
    }
    out.push_str(&format!(
        "\n_The `src` tree is {} files, {}. Lowest of {} runs each, measured with GNU `time`._\n",
        cli.tree_files,
        human_bytes(cli.tree_bytes),
        cli.runs
    ));
    out
}

fn megabytes(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.0} MB"))
        .unwrap_or_else(|| "-".to_string())
}

/// A byte count as KiB or MiB, whichever reads better at that size.
fn human_bytes(n: u64) -> String {
    let kib = n as f64 / 1024.0;
    if kib >= 1024.0 {
        format!("{:.1} MiB", kib / 1024.0)
    } else {
        format!("{kib:.0} KiB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real artifact, cut down to one server and one CLI case. Its point is
    /// to pin the field names: the schema is written by `bench/memory_merge.py`
    /// and read here, so a rename on one side has to fail on the other rather
    /// than quietly render every cell as "-".
    const ARTIFACT: &str = r#"{
      "schema_version": 1,
      "meta": {
        "host": "terra", "os": "Linux x86_64", "cpu": "Ryzen 9 7900",
        "memory_gb": 61,
        "corpora": [{"name": "DataFrames", "repo": "https://example.invalid",
                     "tag": "v1.8.2", "commit": "946c72a"}],
        "servers": {
          "languageserver": {"version": "5.0.0"},
          "jetls": {"commit": "7e01ca58", "date": "2026-08-08"}
        },
        "versions": {"fatou": "0.12.0", "julia": "1.12.6"}
      },
      "session": {"project": "DataFrames", "file_count": 5, "total_bytes": 351116},
      "servers": [
        {"key": "fatou", "label": "Fatou", "doing": "static analysis",
         "baseline_rss_mb": 96.1, "settled_rss_mb": 100.3, "settled_pss_mb": 98.8,
         "peak_rss_mb": 100.3, "processes_at_settle": 1, "settled_seconds": 6.58,
         "relative_to_fatou": 1.0, "notes": []},
        {"key": "jetls", "label": "JETLS", "doing": "type inference",
         "baseline_rss_mb": 795.3, "settled_rss_mb": 1962.5, "settled_pss_mb": 1960.0,
         "peak_rss_mb": 2044.8, "processes_at_settle": 1, "settled_seconds": 34.47,
         "relative_to_fatou": 19.6, "notes": ["still busy at the settle timeout"]}
      ],
      "cli": {
        "tree_files": 36, "tree_bytes": 870581, "runs": 5,
        "cases": [{"key": "lint_tree", "command": "fatou lint", "target": "src tree",
                   "peak_rss_mb": 37.1, "seconds": 0.03, "input_bytes": 870581,
                   "exit_code": 1}]
      }
    }"#;

    fn artifact() -> Memory {
        serde_json::from_str(ARTIFACT).expect("artifact schema drifted from the renderer")
    }

    #[test]
    fn meta_names_the_workspace_and_every_measured_tool() {
        let out = render_meta(&artifact());
        assert!(out.contains("[DataFrames.jl]"), "{out}");
        assert!(out.contains("**Fatou** `0.12.0`"), "{out}");
        assert!(out.contains("**LanguageServer.jl** `5.0.0`"), "{out}");
        assert!(out.contains("**JETLS** `7e01ca58` (2026-08-08)"), "{out}");
        assert!(out.contains("343 KiB"), "{out}");
    }

    #[test]
    fn server_table_carries_the_milestones_and_the_ratio() {
        let out = render_servers(&artifact());
        assert!(
            out.contains("| Fatou | 96 MB | 100 MB | 100 MB | baseline |"),
            "{out}"
        );
        assert!(
            out.contains("| JETLS | 795 MB | 1962 MB | 2045 MB | 19.6x |"),
            "{out}"
        );
        assert!(out.contains("_Harness notes: JETLS: still busy"), "{out}");
    }

    #[test]
    fn cli_table_is_a_table() {
        let out = render_cli(&artifact().cli);
        let rows: Vec<&str> = out.lines().filter(|l| l.starts_with('|')).collect();
        // Header, separator, one case -- and nothing wedged between them, which
        // is what would silently stop it rendering as a table.
        assert_eq!(rows.len(), 3, "{out}");
        assert!(
            rows[2].contains("`fatou lint` | src tree | 850 KiB | 37.1 MB"),
            "{out}"
        );
        assert!(out.contains("36 files, 850 KiB"), "{out}");
    }

    #[test]
    fn a_missing_field_is_rendered_as_absent_rather_than_guessed() {
        let mut m = artifact();
        m.servers[1].settled_rss_mb = None;
        m.servers[1].relative_to_fatou = None;
        let out = render_servers(&m);
        assert!(
            out.contains("| JETLS | 795 MB | - | 2045 MB | - |"),
            "{out}"
        );
    }
}

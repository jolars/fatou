//! Opt-in handler timing without stdio or request scheduling.
//!
//! Set `FATOU_DOC_BENCH_SOURCE` to a Julia file containing the probes from
//! `bench/lsp_documentation.py`, and `FATOU_DOC_BENCH_OUTPUT` to a JSON path.
//! Run the ignored `warm_markdown_requests` test under the release profile on
//! a pinned CPU. `FATOU_DOC_BENCH_ITERATIONS` controls the calls per sample.

use std::hint::black_box;
use std::time::Instant;

use crate::incremental::IncrementalDatabase;
use crate::text::{PositionEncoding, TextBuffer};

fn measure<T: PartialEq + std::fmt::Debug>(
    iterations: usize,
    mut request: impl FnMut() -> T,
) -> serde_json::Value {
    let expected = request();
    for _ in 0..3 {
        assert_eq!(request(), expected);
    }
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(request());
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / iterations as f64);
        assert_eq!(request(), expected);
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    serde_json::json!({
        "median_us": (sorted[9] + sorted[10]) / 2.0,
        "min_us": sorted[0],
        "samples_us": samples,
    })
}

#[test]
#[ignore = "opt-in local performance measurement"]
fn warm_markdown_requests() {
    let source = std::env::var("FATOU_DOC_BENCH_SOURCE").expect("benchmark source path");
    let output = std::env::var("FATOU_DOC_BENCH_OUTPUT").expect("benchmark output path");
    let iterations: usize =
        std::env::var("FATOU_DOC_BENCH_ITERATIONS").map_or(20, |value| value.parse().unwrap());
    assert!(iterations > 0);
    let text = std::fs::read_to_string(&source).unwrap();
    let live = TextBuffer::from(text.as_str());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("docs.jl");
    let uri = super::uri::from_path(&path).unwrap();
    let mut db = IncrementalDatabase::new();
    let file = db.upsert_file(&path, live.text_arc());
    let snapshot = db.snapshot();
    snapshot.semantic_model(file);
    let encoding = PositionEncoding::Utf16;
    let position = |marker: &str| {
        live.line_index()
            .byte_to_position(text.find(marker).unwrap() + marker.len(), encoding)
    };
    let mut results = serde_json::Map::new();
    let completion_position = position("@ref benchmark-tar");
    results.insert(
        "completion".to_string(),
        measure(iterations, || {
            super::completion::completion_via_db(
                &snapshot,
                &path,
                &live,
                completion_position,
                encoding,
            )
        }),
    );
    for (name, marker) in [
        ("anchor_definition", "@ref benchmark-tar"),
        ("missing_definition", "#benchmark-miss"),
        ("footnote_definition", "[^benchmarkno"),
    ] {
        let position = position(marker);
        results.insert(
            name.to_string(),
            measure(iterations, || {
                super::definition::definition_via_db(
                    &snapshot, &uri, &path, &live, position, encoding,
                )
            }),
        );
    }
    std::fs::write(
        output,
        serde_json::to_string_pretty(&serde_json::json!({
            "source": source, "bytes": text.len(), "samples": 20,
            "iterations_per_sample": iterations, "requests": results,
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
}

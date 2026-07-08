//! Generate the mdBook rule-reference pages from rule metadata.
//!
//! Run with `cargo run --example docgen`. For each rule that carries examples,
//! this renders the same markdown the snapshot test pins
//! ([`fatou::linter::render_rule_doc`]) and writes it under the mdBook source
//! tree, plus an index page linking them.
//!
//! Living as an `examples/` target (not a `[[bin]]`) keeps `fatou` a single,
//! publishable crate: `examples/` is outside the Cargo `include` whitelist, so
//! this never ships to crates.io.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use fatou::linter::docs::documented_pages;

fn main() -> io::Result<()> {
    let reference_dir = Path::new("docs/src/reference");
    let rules_dir = reference_dir.join("rules");
    fs::create_dir_all(&rules_dir)?;

    let mut index = String::from(
        "# Lint Rules\n\nEach rule is documented with a description and a worked example whose \
diagnostics are rendered by running the linter itself, so the reference can \
never drift from behavior.\n\n",
    );

    for (id, page) in documented_pages() {
        write_if_changed(&rules_dir.join(format!("{id}.md")), &page)?;
        let _ = writeln!(index, "- [`{id}`](rules/{id}.md)");
    }

    write_if_changed(&reference_dir.join("rules.md"), &index)?;
    Ok(())
}

/// Write `content` to `path` only when it differs from what's already there, so
/// re-running the generator leaves unchanged files (and their mtimes) alone.
fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    fs::write(path, content)?;
    println!("wrote {}", path.display());
    Ok(())
}

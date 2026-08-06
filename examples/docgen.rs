//! Generate the mdBook lint-rule reference from rule metadata.
//!
//! Run with `cargo run --example docgen`. It renders the same markdown the
//! snapshot test pins ([`fatou::linter::docs::render_reference_page`]) and
//! writes it to the mdBook source tree as a single page, one section per rule.
//!
//! Living as an `examples/` target (not a `[[bin]]`) keeps `fatou` a single,
//! publishable crate: `examples/` is outside the Cargo `include` whitelist, so
//! this never ships to crates.io.

use std::fs;
use std::io;
use std::path::Path;

use fatou::linter::docs::render_reference_page;

fn main() -> io::Result<()> {
    write_if_changed(
        Path::new("docs/src/reference/rules.md"),
        &render_reference_page(),
    )
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

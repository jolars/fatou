//! Render benchmark markers as JSON for the documentation build.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    serde_json::to_writer(std::io::stdout(), &doc_utils::render(&project_root))?;
    Ok(())
}

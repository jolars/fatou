//! Generation and parity tests for the published `fatou.toml` JSON Schema.
//!
//! The schema is a generated artifact. Set `UPDATE_EXPECTED=1` and run
//! `cargo test config_schema` after an intentional configuration change, then
//! review the resulting `fatou.schema.json` diff.

use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::Validator;
use schemars::generate::SchemaSettings;
use schemars::transform::RestrictFormats;
use serde_json::Value;

use super::RawConfig;

const SCHEMA_ID: &str = "https://fatou.dev/fatou.schema.json";
const SCHEMA_PATH: &str = "fatou.schema.json";

fn generate_schema_json() -> Value {
    // SchemaStore validates schemas in strict mode. Schemars otherwise emits
    // Rust-specific formats such as `uint32`, which add no constraint beyond
    // the generated integer bounds and are not part of draft 7.
    let generator = SchemaSettings::draft07()
        .with_transform(RestrictFormats::default())
        .into_generator();
    let schema = generator.into_root_schema_for::<RawConfig>();
    let mut json = serde_json::to_value(schema).expect("serialize configuration schema");
    let Value::Object(root) = &mut json else {
        panic!("root configuration schema must be an object");
    };
    root.insert("$id".into(), SCHEMA_ID.into());
    root.insert("title".into(), "Fatou configuration".into());
    root.insert(
        "description".into(),
        "Schema for fatou.toml. Generated from Fatou's configuration types; do not hand-edit—run `UPDATE_EXPECTED=1 cargo test config_schema` instead."
            .into(),
    );
    json
}

fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_PATH)
}

fn render_schema(schema: &Value) -> String {
    let mut rendered = serde_json::to_string_pretty(schema).expect("render schema");
    rendered.push('\n');
    rendered
}

fn validator() -> Validator {
    Validator::new(&generate_schema_json()).expect("compile generated configuration schema")
}

fn toml_to_json(source: &str) -> Value {
    let value: toml::Value = toml::from_str(source).expect("parse test configuration as TOML");
    serde_json::to_value(value).expect("convert test configuration to JSON")
}

fn assert_accepts(source: &str) {
    toml::from_str::<RawConfig>(source).expect("runtime configuration should accept fixture");
    let value = toml_to_json(source);
    let errors: Vec<_> = validator()
        .iter_errors(&value)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "schema rejected a runtime-valid configuration:\n{}",
        errors.join("\n")
    );
}

fn assert_rejects(source: &str) {
    toml::from_str::<RawConfig>(source).expect_err("runtime configuration should reject fixture");
    let value = toml_to_json(source);
    assert!(
        validator().iter_errors(&value).next().is_some(),
        "schema accepted a runtime-invalid configuration: {source}"
    );
}

#[test]
fn config_schema_is_in_sync() {
    let generated = render_schema(&generate_schema_json());
    let path = schema_path();

    if std::env::var_os("UPDATE_EXPECTED").is_some() {
        fs::write(&path, generated).expect("write generated configuration schema");
        return;
    }

    let checked_in = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing {}: {error}. Run `UPDATE_EXPECTED=1 cargo test config_schema` to create it.",
            path.display()
        )
    });
    assert_eq!(
        checked_in,
        generated,
        "{} is out of date; run `UPDATE_EXPECTED=1 cargo test config_schema`",
        path.display()
    );
}

#[test]
fn config_schema_uses_the_public_draft_7_identity() {
    let schema = generate_schema_json();
    assert_eq!(schema["$id"], SCHEMA_ID);
    assert_eq!(schema["$schema"], "http://json-schema.org/draft-07/schema#");
    assert!(
        !render_schema(&schema).contains("\"format\": \"uint32\""),
        "schema must not expose Rust-specific formats"
    );
    validator();
}

#[test]
fn config_schema_accepts_supported_configuration() {
    assert_accepts("");
    assert_accepts(
        r#"
exclude = ["vendored/"]
extend-exclude = ["generated/"]

[format]
line-width = 100
indent-width = 2
line-ending = "native"

[lint]
select = ["future-rule"]
ignore = ["unused-binding"]

[lint.severity]
one = "error"
two = "warning"
three = "info"
four = "hint"

[lint.rules.discouraged-function]
functions = { sleep = "use a timer" }
extend-functions = { exit = "return an error" }

[julia]
version = "1.6, 1.10 - 1.11"
"#,
    );
    assert_accepts("[format]\nline_width = 100\nindent_width = 2\n");

    for line_ending in ["auto", "lf", "crlf", "native"] {
        assert_accepts(&format!("[format]\nline-ending = \"{line_ending}\"\n"));
    }

    // Runtime compatibility parsing warns about this spelling but does not
    // reject the configuration, so the structural schema must accept it too.
    assert_accepts("[julia]\nversion = \"not a version\"\n");
}

#[test]
fn config_schema_rejects_invalid_configuration() {
    for source in [
        "unknown = true\n",
        "[format]\nline-width = \"wide\"\n",
        "[format]\nline-ending = \"mac\"\n",
        "[lint]\nunknown = true\n",
        "[lint.severity]\nunused-binding = \"fatal\"\n",
        "[lint.rules.unknown-rule]\n",
        "[lint.rules.discouraged-function]\nfuncs = {}\n",
        "[julia]\ntarget = \"1.6\"\n",
    ] {
        assert_rejects(source);
    }
}

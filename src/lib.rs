pub use fatou_parser::{ast, parser, syntax};

pub mod cli;
pub mod config;
pub mod debug;
pub mod environment;
pub mod file_discovery;
pub mod formatter;
pub mod incremental;
pub mod index;
pub mod julia_version;
pub mod linter;
pub mod lsp;
pub mod project;
pub mod resolve;
pub mod semantic;
pub mod text;

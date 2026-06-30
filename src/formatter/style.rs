use crate::config::FormatConfig;

/// The knobs that govern layout: the target line width and the indent step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatStyle {
    pub line_width: u32,
    pub indent_width: u32,
}

impl Default for FormatStyle {
    fn default() -> Self {
        FormatStyle {
            line_width: 92,
            indent_width: 4,
        }
    }
}

impl From<&FormatConfig> for FormatStyle {
    fn from(config: &FormatConfig) -> Self {
        FormatStyle {
            line_width: config.line_width,
            indent_width: config.indent_width,
        }
    }
}

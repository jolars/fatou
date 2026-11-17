//! Explicit, host-independent Julia syntax selection.

/// A Julia syntax edition supported by this parser. Editions are separate from
/// the runtime versions a package supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyntaxEdition {
    Julia113,
    Julia114,
    Julia115,
}

impl SyntaxEdition {
    pub const ALL: [Self; 3] = [Self::Julia113, Self::Julia114, Self::Julia115];
    pub const OLDEST: Self = Self::Julia113;
    pub const LATEST: Self = Self::Julia115;

    /// Julia's `normalized_edition`: select the newest supported edition no
    /// greater than the version, clamping at either end of the edition table.
    pub const fn normalized(major: u32, minor: u32) -> Self {
        if major > 1 || (major == 1 && minor >= 15) {
            Self::Julia115
        } else if major == 1 && minor >= 14 {
            Self::Julia114
        } else {
            Self::Julia113
        }
    }
}

impl Default for SyntaxEdition {
    fn default() -> Self {
        Self::LATEST
    }
}

impl std::fmt::Display for SyntaxEdition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Julia113 => "1.13",
            Self::Julia114 => "1.14",
            Self::Julia115 => "1.15",
        })
    }
}

/// An explicit edition must name a supported edition. Project metadata instead
/// normalizes a Julia version with [`SyntaxEdition::normalized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSyntaxEdition;

impl std::fmt::Display for InvalidSyntaxEdition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected syntax edition 1.13, 1.14, or 1.15")
    }
}
impl std::error::Error for InvalidSyntaxEdition {}

impl std::str::FromStr for SyntaxEdition {
    type Err = InvalidSyntaxEdition;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "1.13" => Ok(Self::Julia113),
            "1.14" => Ok(Self::Julia114),
            "1.15" => Ok(Self::Julia115),
            _ => Err(InvalidSyntaxEdition),
        }
    }
}

/// Resolved parsing options. The parser never discovers projects or invokes Julia.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParseOptions {
    pub syntax_edition: SyntaxEdition,
}

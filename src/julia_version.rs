//! Julia version numbers, version ranges, and `Pkg`-style `[compat]` parsing.
//!
//! The linter is version-aware without a Julia runtime: it parses the
//! *superset* of Julia syntax and flags constructs that a project's declared
//! Julia support range does not cover (see the `julia-version-compat` rule).
//! This module supplies the value types for that: a comparable [`Version`], a
//! [`VersionRange`] (the supported floor, plus an optional ceiling reserved for
//! future removed-syntax checks), and [`parse_compat`], which reads the range
//! out of a `Project.toml` `[compat].julia` entry or a `fatou.toml`
//! `[julia] version` string.
//!
//! Julia's `Pkg` compat grammar is *not* Cargo semver: a bare `"1.6"` carries an
//! implicit caret (`>= 1.6.0, < 2.0.0`), `-` spells an inclusive range, and a
//! comma spells a union. Dependency updates use exact disjoint intervals;
//! [`parse_compat`] projects their lowest floor and highest ceiling for the
//! linter's syntax compatibility checks.

mod compat;

pub(crate) use compat::CompatSpec;

use std::fmt;
use std::str::FromStr;

/// A Julia version, compared field by field. Missing trailing fields read as
/// zero, so `1.6` orders as `1.6.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// This version with `minor`/`patch` defaulted to zero.
    pub const fn major_only(major: u32) -> Self {
        Self::new(major, 0, 0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parse a `major[.minor[.patch]]` version, defaulting absent fields to zero.
/// Extra dot-separated components (e.g. a prerelease tail) are ignored; an empty
/// or non-numeric leading component fails.
impl FromStr for Version {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split(['.', '-', '+']);
        let major = parse_component(parts.next())?;
        let minor = match parts.next() {
            Some(part) => parse_component(Some(part))?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(part) => parse_component(Some(part))?,
            None => 0,
        };
        Ok(Version::new(major, minor, patch))
    }
}

fn parse_component(part: Option<&str>) -> Result<u32, ParseError> {
    part.filter(|p| !p.is_empty())
        .and_then(|p| p.parse().ok())
        .ok_or(ParseError)
}

/// A supported Julia version range: an inclusive floor and an optional
/// (exclusive) ceiling. Only [`min`](Self::min) drives compat checks today; the
/// ceiling is retained for future removed-syntax checks across a major bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    /// The lowest supported version — the floor a feature must not exceed.
    pub min: Version,
    /// The exclusive upper bound, when the spec implies one.
    pub max: Option<Version>,
}

impl VersionRange {
    /// A range that supports exactly `version` and nothing else — how a
    /// `Manifest.toml` point version (`julia_version`) is treated.
    pub const fn exact(version: Version) -> Self {
        Self {
            min: version,
            max: None,
        }
    }

    /// Whether a feature introduced in `introduced` is safe for this range: it
    /// is only safe when the whole supported range is at or above it, i.e. the
    /// floor already includes it.
    pub fn covers_feature(&self, introduced: Version) -> bool {
        self.min >= introduced
    }
}

/// The error type for [`Version`] and [`parse_compat`]. Deliberately opaque: the
/// caller reports "could not parse the Julia version" with the offending string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid Julia version specifier")
    }
}

impl std::error::Error for ParseError {}

/// Parse a `Pkg`-style compat spec into the overall supported [`VersionRange`].
///
/// A comma spells a union of clauses; we return the lowest floor and highest
/// ceiling across them. Each clause is one of:
/// - `1.6` / `1.6.2` / `^1.6` — caret: floor at the value, ceiling at the next
///   major (the first nonzero component's next value, per `Pkg` caret rules).
/// - `~1.6` / `~1.6.2` — tilde: floor at the value, ceiling at the next minor.
/// - `=1.6` — exact.
/// - `1.6 - 1.11` — inclusive hyphen range.
/// - `>= 1.6`, `≥ 1.6`, `< 2` — inequalities.
pub fn parse_compat(spec: &str) -> Result<VersionRange, ParseError> {
    Ok(CompatSpec::parse(spec)?.envelope())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version::new(major, minor, patch)
    }

    #[test]
    fn parses_and_defaults_version_components() {
        assert_eq!("1".parse::<Version>().unwrap(), v(1, 0, 0));
        assert_eq!("1.6".parse::<Version>().unwrap(), v(1, 6, 0));
        assert_eq!("1.6.2".parse::<Version>().unwrap(), v(1, 6, 2));
        assert_eq!(" 1.11 ".parse::<Version>().unwrap(), v(1, 11, 0));
        // Prerelease/build tails are ignored after the numeric core.
        assert_eq!("1.12.0-rc1".parse::<Version>().unwrap(), v(1, 12, 0));
    }

    #[test]
    fn rejects_non_numeric_versions() {
        assert!("".parse::<Version>().is_err());
        assert!("x".parse::<Version>().is_err());
        assert!("1.x".parse::<Version>().is_err());
    }

    #[test]
    fn version_orders_field_by_field() {
        assert!(v(1, 6, 0) < v(1, 11, 0));
        assert!(v(1, 11, 0) > v(1, 6, 5));
        assert!(v(2, 0, 0) > v(1, 99, 99));
        assert_eq!(v(1, 6, 0), "1.6".parse().unwrap());
    }

    #[test]
    fn bare_version_is_caret() {
        let r = parse_compat("1.6").unwrap();
        assert_eq!(r.min, v(1, 6, 0));
        assert_eq!(r.max, Some(v(2, 0, 0)));
    }

    #[test]
    fn caret_ceiling_follows_first_nonzero() {
        assert_eq!(parse_compat("^1.2.3").unwrap().max, Some(v(2, 0, 0)));
        assert_eq!(parse_compat("0.3").unwrap().max, Some(v(0, 4, 0)));
        assert_eq!(parse_compat("0.0.4").unwrap().max, Some(v(0, 0, 5)));
    }

    #[test]
    fn tilde_bumps_minor() {
        let r = parse_compat("~1.6").unwrap();
        assert_eq!(r.min, v(1, 6, 0));
        assert_eq!(r.max, Some(v(1, 7, 0)));
    }

    #[test]
    fn equality_pins_one_patch() {
        let r = parse_compat("=1.6.2").unwrap();
        assert_eq!(r.min, v(1, 6, 2));
        assert_eq!(r.max, Some(v(1, 6, 3)));
    }

    #[test]
    fn hyphen_range_is_inclusive() {
        let r = parse_compat("1.6 - 1.11").unwrap();
        assert_eq!(r.min, v(1, 6, 0));
        assert_eq!(r.max, Some(v(1, 12, 0)));
    }

    #[test]
    fn comma_union_takes_lowest_floor_highest_ceiling() {
        let r = parse_compat("1.6, 1.10").unwrap();
        assert_eq!(r.min, v(1, 6, 0));
        assert_eq!(r.max, Some(v(2, 0, 0)));
    }

    #[test]
    fn empty_or_all_blank_spec_fails() {
        assert!(parse_compat("").is_err());
        assert!(parse_compat("  ,  ").is_err());
    }

    #[test]
    fn compat_respects_partial_upper_bounds_and_inequalities() {
        assert_eq!(parse_compat("1.6 - 1.11").unwrap().max, Some(v(1, 12, 0)));
        assert_eq!(parse_compat("0").unwrap().max, Some(v(1, 0, 0)));
        assert_eq!(parse_compat("0.0").unwrap().max, Some(v(0, 1, 0)));
        assert_eq!(parse_compat("~1").unwrap().max, Some(v(2, 0, 0)));
        assert_eq!(
            parse_compat(">= 1.2").unwrap(),
            VersionRange {
                min: v(1, 2, 0),
                max: None
            }
        );
        assert_eq!(
            parse_compat("< 2").unwrap(),
            VersionRange {
                min: v(0, 0, 0),
                max: Some(v(2, 0, 0))
            }
        );
    }

    #[test]
    fn covers_feature_checks_the_floor() {
        // A project supporting 1.6+ does NOT cover a 1.11 feature.
        let range = parse_compat("1.6").unwrap();
        assert!(!range.covers_feature(v(1, 11, 0)));
        assert!(range.covers_feature(v(1, 6, 0)));
        assert!(range.covers_feature(v(1, 0, 0)));

        // An exact 1.11 target covers the 1.11 feature.
        let exact = VersionRange::exact(v(1, 11, 0));
        assert!(exact.covers_feature(v(1, 11, 0)));
        assert!(!exact.covers_feature(v(1, 12, 0)));
    }
}

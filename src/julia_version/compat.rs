//! Exact Pkg compatibility intervals and edits that retain requirement syntax.
//!
//! Unlike Cargo requirements, comma-separated Pkg clauses form a union. Keep
//! the intervals separate here; only the linter's public projection merges them.

use std::ops::Range;

use super::{ParseError, Version, VersionRange};

pub(crate) struct CompatSpec<'a> {
    text: &'a str,
    clauses: Vec<Clause>,
}

struct Clause {
    kind: Kind,
    lower: Number,
    upper: Option<Number>,
    interval: VersionRange,
}

struct Number {
    version: Version,
    precision: usize,
    span: Range<usize>,
}

#[derive(Clone, Copy)]
enum Kind {
    Caret,
    Tilde,
    Exact,
    AtLeast,
    LessThan,
    Hyphen,
}

impl<'a> CompatSpec<'a> {
    pub(crate) fn parse(text: &'a str) -> Result<Self, ParseError> {
        let mut clauses = Vec::new();
        let mut offset = 0;
        for part in text.split(',') {
            let clause = part.trim();
            let start = offset + part.len() - part.trim_start().len();
            clauses.push(parse_clause(clause, start)?);
            offset += part.len() + 1;
        }
        Ok(Self { text, clauses })
    }

    pub(crate) fn contains(&self, version: Version) -> bool {
        self.clauses
            .iter()
            .any(|clause| contains(clause.interval, version))
    }

    pub(crate) fn envelope(&self) -> VersionRange {
        self.clauses
            .iter()
            .map(|clause| clause.interval)
            .reduce(|a, b| VersionRange {
                min: a.min.min(b.min),
                max: a.max.zip(b.max).map(|(a, b)| a.max(b)),
            })
            .expect("a parsed spec has at least one clause")
    }

    /// Mirror precision-preserving dependency edits, while keeping Julia union
    /// arms independent. An upgrade outside a union adds an arm rather than
    /// rewriting every older support series to the same new version.
    pub(crate) fn with_version(&self, version: Version) -> Option<String> {
        let matching = self
            .clauses
            .iter()
            .filter(|clause| contains(clause.interval, version))
            .max_by_key(|clause| clause.interval.min);
        let clause =
            matching.or_else(|| self.clauses.iter().max_by_key(|clause| clause.interval.min))?;
        if version < clause.interval.min {
            return None;
        }
        if matching.is_none() && self.clauses.len() > 1 {
            let new_arm = match clause.kind {
                Kind::Caret | Kind::Tilde | Kind::Exact => {
                    let prefix = match clause.kind {
                        Kind::Tilde => "~",
                        Kind::Exact => "=",
                        _ => "",
                    };
                    format!(
                        "{prefix}{}",
                        format_version(version, precision_for(clause, version))
                    )
                }
                _ => version.to_string(),
            };
            return Some(format!("{}, {new_arm}", self.text));
        }
        let (number, replacement) = match clause.kind {
            Kind::Caret | Kind::Tilde | Kind::Exact => (
                &clause.lower,
                format_version(version, precision_for(clause, version)),
            ),
            Kind::LessThan if matching.is_none() => (
                &clause.lower,
                format_version(
                    successor(version, clause.lower.precision)?,
                    clause.lower.precision,
                ),
            ),
            Kind::Hyphen if matching.is_none() => {
                let upper = clause.upper.as_ref()?;
                (upper, format_version(version, upper.precision))
            }
            _ => return Some(self.text.to_owned()),
        };
        let mut text = self.text.to_owned();
        text.replace_range(number.span.clone(), &replacement);
        Some(text)
    }
}

fn precision_for(clause: &Clause, version: Version) -> usize {
    if matches!(clause.kind, Kind::Exact) {
        // Julia's `=1` means exactly 1.0.0, whereas Cargo permits all 1.x.
        clause.lower.precision.max(if version.patch != 0 {
            3
        } else if version.minor != 0 {
            2
        } else {
            1
        })
    } else {
        clause.lower.precision
    }
}

fn contains(range: VersionRange, version: Version) -> bool {
    range.min <= version && range.max.is_none_or(|max| version < max)
}

fn parse_clause(text: &str, offset: usize) -> Result<Clause, ParseError> {
    if let Some((lo, hi)) = text.split_once('-') {
        if !lo.ends_with(char::is_whitespace) || !hi.starts_with(char::is_whitespace) {
            return Err(ParseError);
        }
        let lower = number(lo.trim_end(), offset)?;
        let upper = number(
            hi.trim_start(),
            offset + lo.len() + 1 + hi.len() - hi.trim_start().len(),
        )?;
        let interval = VersionRange {
            min: lower.version,
            max: successor(upper.version, upper.precision),
        };
        return Ok(Clause {
            kind: Kind::Hyphen,
            lower,
            upper: Some(upper),
            interval,
        });
    }
    let (kind, raw, spaces) =
        if let Some(raw) = text.strip_prefix(">=").or_else(|| text.strip_prefix('≥')) {
            (Kind::AtLeast, raw, true)
        } else if let Some(raw) = text.strip_prefix('<') {
            (Kind::LessThan, raw, true)
        } else if let Some(raw) = text.strip_prefix('=') {
            (Kind::Exact, raw, true)
        } else if let Some(raw) = text.strip_prefix('~') {
            (Kind::Tilde, raw, false)
        } else {
            (Kind::Caret, text.strip_prefix('^').unwrap_or(text), false)
        };
    let raw = if spaces { raw.trim_start() } else { raw };
    let lower = number(raw, offset + text.len() - raw.len())?;
    let v = lower.version;
    if lower.precision == 3 && v == Version::new(0, 0, 0) {
        return Err(ParseError);
    }
    let max = match kind {
        Kind::Caret => successor(
            v,
            if v.major != 0 {
                1
            } else if v.minor != 0 {
                2
            } else {
                lower.precision
            },
        ),
        Kind::Tilde => successor(v, lower.precision.min(2)),
        Kind::Exact => successor(v, 3),
        Kind::AtLeast => None,
        Kind::LessThan => Some(v),
        Kind::Hyphen => unreachable!(),
    };
    let min = if matches!(kind, Kind::LessThan) {
        Version::new(0, 0, 0)
    } else {
        v
    };
    Ok(Clause {
        kind,
        lower,
        upper: None,
        interval: VersionRange { min, max },
    })
}

fn number(text: &str, offset: usize) -> Result<Number, ParseError> {
    let digits = text.strip_prefix('v').unwrap_or(text);
    let parts: Vec<_> = digits.split('.').collect();
    if parts.len() > 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(ParseError);
    }
    let mut values = [0; 3];
    for (dest, part) in values.iter_mut().zip(&parts) {
        *dest = part.parse().map_err(|_| ParseError)?;
    }
    Ok(Number {
        version: Version::new(values[0], values[1], values[2]),
        precision: parts.len(),
        span: offset + text.len() - digits.len()..offset + text.len(),
    })
}

fn format_version(version: Version, precision: usize) -> String {
    match precision {
        1 => version.major.to_string(),
        2 => format!("{}.{}", version.major, version.minor),
        _ => version.to_string(),
    }
}

fn successor(version: Version, precision: usize) -> Option<Version> {
    let mut parts = [version.major, version.minor, version.patch];
    parts[precision..].fill(0);
    for index in (0..precision).rev() {
        if let Some(next) = parts[index].checked_add(1) {
            parts[index] = next;
            return Some(Version::new(parts[0], parts[1], parts[2]));
        }
        parts[index] = 0;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_membership_keeps_gaps_and_partial_bounds() {
        let spec = CompatSpec::parse("0.2, 1.2 - 1.4").unwrap();
        for raw in ["0.2.9", "1.2.0", "1.4.99"] {
            assert!(spec.contains(raw.parse().unwrap()), "{raw}");
        }
        for raw in ["0.3.0", "1.1.9", "1.5.0"] {
            assert!(!spec.contains(raw.parse().unwrap()), "{raw}");
        }
    }

    #[test]
    fn version_edits_preserve_precision_operators_and_julia_unions() {
        for (before, target, after) in [
            ("1", "1.9.3", "1"),
            ("1.2", "1.9.3", "1.9"),
            ("^v1.2.0", "1.9.3", "^v1.9.3"),
            ("~1.2", "1.2.9", "~1.2"),
            ("=1", "2.3.4", "=2.3.4"),
            ("< 2", "2.3.4", "< 3"),
            (">= 1.2", "2.3.4", ">= 1.2"),
            ("1.2 - 1.4", "2.3.4", "1.2 - 2.3"),
            ("0.2, 1.2", "1.9.3", "0.2, 1.9"),
            ("0.2, 1", "2.3.4", "0.2, 1, 2"),
        ] {
            let version = target.parse().unwrap();
            let edited = CompatSpec::parse(before)
                .unwrap()
                .with_version(version)
                .unwrap();
            assert_eq!(edited, after, "{before}");
            assert!(
                CompatSpec::parse(&edited).unwrap().contains(version),
                "{edited}"
            );
        }
    }

    #[test]
    fn invalid_compat_never_produces_an_edit() {
        for text in [
            "", "1,", "1,,2", "1.2.3.4", "1.0-rc1", "1.*", "<=2", "^ 1", "0.0.0",
        ] {
            assert!(CompatSpec::parse(text).is_err(), "{text}");
        }
    }
}

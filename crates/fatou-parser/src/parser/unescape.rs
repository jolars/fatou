//! Decoding a string literal's source text into the value it denotes.
//!
//! The CST is lossless, so a `STRING_CONTENT` token carries the *source* bytes
//! between the quotes: `"sub\\a.jl"` holds the ten characters `sub\\a.jl`, not
//! the nine the literal denotes. Every consumer that reads a literal as **data**
//! — the s-expression projector, `include` path resolution — has to decode
//! first, and they all decode the same way, so the decoding lives here once.
//!
//! Julia's rules, as Base's own lexer reads a double-quoted string: the named
//! escapes, `\xNN` and octal escapes contributing a single *byte*, `\u`/`\U`
//! contributing a codepoint, and a backslash-newline line continuation that
//! swallows the newline and the indentation after it. Byte escapes are why the
//! decoder accumulates bytes rather than chars: `"\xce\xb1"` is one `α`.

/// Why a `STRING_CONTENT` token's source could not be reduced to a value. The
/// two failures matter separately to the s-expression projector, which shows
/// them differently; a consumer that just wants the value treats both as "this
/// literal does not denote a string".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringDecodeError {
    /// A malformed backslash escape (`\xq`, `\q`, `\400`).
    BadEscape,
    /// Well-formed bytes that don't decode as UTF-8 (`\xff`). Julia's `String`
    /// holds them; Rust's does not.
    BadUtf8,
}

/// The value a `STRING_CONTENT` token's source text denotes: escapes decoded and
/// line continuations dropped.
pub fn string_value(text: &str) -> Result<String, StringDecodeError> {
    Ok(string_chunks(text)?.concat())
}

/// Decode one `STRING_CONTENT` token's source into its literal value, split into
/// chunks at each `\`-newline line continuation (JuliaSyntax keeps one `String`
/// per chunk; [`string_value`] just concatenates them). A continuation drops the
/// backslash, the newline (`\n`/`\r`/`\r\n`), and the run of spaces/tabs that
/// follow.
pub(crate) fn string_chunks(text: &str) -> Result<Vec<String>, StringDecodeError> {
    let mut chunks: Vec<Vec<u8>> = vec![Vec::new()];
    let mut buf = [0u8; 4];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let last = chunks.last_mut().unwrap();
            last.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek() {
            Some('\n') | Some('\r') => {
                let nl = chars.next().unwrap();
                if nl == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                while matches!(chars.peek(), Some(' ') | Some('\t')) {
                    chars.next();
                }
                chunks.push(Vec::new());
            }
            _ => decode_escape_into(&mut chars, chunks.last_mut().unwrap())
                .ok_or(StringDecodeError::BadEscape)?,
        }
    }
    chunks
        .into_iter()
        .map(|c| String::from_utf8(c).map_err(|_| StringDecodeError::BadUtf8))
        .collect()
}

/// Decode a single backslash escape (the backslash already consumed) into `bytes`,
/// the way Julia reads a `Char`/`String` literal: byte escapes (`\xNN`, octal) push
/// one byte; `\u`/`\U` push a codepoint's UTF-8 bytes; named escapes push their
/// control byte. Returns `None` on a malformed or unknown escape.
pub(crate) fn decode_escape_into(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    bytes: &mut Vec<u8>,
) -> Option<()> {
    let mut buf = [0u8; 4];
    match chars.next()? {
        'n' => bytes.push(b'\n'),
        't' => bytes.push(b'\t'),
        'r' => bytes.push(b'\r'),
        'a' => bytes.push(0x07),
        'b' => bytes.push(0x08),
        'f' => bytes.push(0x0c),
        'v' => bytes.push(0x0b),
        'e' => bytes.push(0x1b),
        '\\' => bytes.push(b'\\'),
        '\'' => bytes.push(b'\''),
        '"' => bytes.push(b'"'),
        '$' => bytes.push(b'$'),
        'x' => bytes.push(take_hex(chars, 2)? as u8),
        'u' => {
            let cp = char::from_u32(take_hex(chars, 4)?)?;
            bytes.extend_from_slice(cp.encode_utf8(&mut buf).as_bytes());
        }
        'U' => {
            let cp = char::from_u32(take_hex(chars, 8)?)?;
            bytes.extend_from_slice(cp.encode_utf8(&mut buf).as_bytes());
        }
        d @ '0'..='7' => {
            let mut val = d.to_digit(8)?;
            for _ in 0..2 {
                match chars.peek().and_then(|c| c.to_digit(8)) {
                    Some(o) => {
                        val = val * 8 + o;
                        chars.next();
                    }
                    None => break,
                }
            }
            // Julia caps an octal escape at one byte; `\400` and up overflow.
            bytes.push(u8::try_from(val).ok()?);
        }
        _ => return None,
    }
    Some(())
}

/// Consume up to `max` hex digits from `chars` and return their value; `None`
/// if there is not at least one digit.
pub(crate) fn take_hex(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    max: usize,
) -> Option<u32> {
    let mut val = 0u32;
    let mut n = 0;
    while n < max {
        match chars.peek().and_then(|c| c.to_digit(16)) {
            Some(d) => {
                val = val * 16 + d;
                chars.next();
                n += 1;
            }
            None => break,
        }
    }
    (n > 0).then_some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_free_content_is_its_own_value() {
        assert_eq!(string_value("sub/a.jl").as_deref(), Ok("sub/a.jl"));
        assert_eq!(string_value("").as_deref(), Ok(""));
    }

    #[test]
    fn a_doubled_backslash_denotes_one() {
        assert_eq!(string_value("sub\\\\a.jl").as_deref(), Ok("sub\\a.jl"));
    }

    #[test]
    fn named_and_numeric_escapes_decode() {
        assert_eq!(string_value("a\\tb\\n").as_deref(), Ok("a\tb\n"));
        assert_eq!(string_value("\\x41\\101").as_deref(), Ok("AA"));
        assert_eq!(string_value("\\u03b1\\U0001f600").as_deref(), Ok("α😀"));
    }

    #[test]
    fn byte_escapes_join_into_one_codepoint() {
        assert_eq!(string_value("\\xce\\xb1").as_deref(), Ok("α"));
    }

    #[test]
    fn quote_dollar_and_backslash_escapes_decode() {
        assert_eq!(string_value("\\\"\\$\\\\").as_deref(), Ok("\"$\\"));
    }

    #[test]
    fn a_line_continuation_swallows_the_newline_and_indent() {
        assert_eq!(string_value("a\\\n    b").as_deref(), Ok("ab"));
        assert_eq!(string_value("a\\\r\n\tb").as_deref(), Ok("ab"));
    }

    #[test]
    fn a_malformed_escape_is_bad_escape() {
        assert_eq!(string_value("a\\q"), Err(StringDecodeError::BadEscape));
        assert_eq!(string_value("\\xq"), Err(StringDecodeError::BadEscape));
        assert_eq!(string_value("\\400"), Err(StringDecodeError::BadEscape));
        assert_eq!(string_value("a\\"), Err(StringDecodeError::BadEscape));
    }

    #[test]
    fn a_lone_non_utf8_byte_is_bad_utf8() {
        assert_eq!(string_value("\\xff"), Err(StringDecodeError::BadUtf8));
    }

    #[test]
    fn chunks_split_at_each_line_continuation() {
        assert_eq!(
            string_chunks("a\\\nb\\\nc").unwrap(),
            ["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}

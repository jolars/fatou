//! Fixed and generated operator recognition.

use super::{
    DIVIDE_SIGN, Lexer, MINUS_SIGN, TokKind, XOR_SIGN, is_unicode_infix_tier, unicode_op_kind,
};

impl Lexer<'_> {
    /// Lex the operator at `start`, or one unknown char if none matches.
    ///
    /// The ASCII table and generated Unicode lookup each return their longest
    /// match. Choosing the longer result preserves longest match across both.
    pub(super) fn lex_operator_or_unknown(&mut self, start: usize) {
        let rest = &self.bytes[self.pos..];
        let ascii = super::try_ascii_op(rest);
        let unicode = match (rest.first(), rest.get(1)) {
            (Some(b), _) if !b.is_ascii() => self.unicode_op_at(0),
            (Some(b'.'), Some(b)) if !b.is_ascii() => self.unicode_op_at(1),
            _ => None,
        };
        let best = [ascii, unicode]
            .into_iter()
            .flatten()
            .max_by_key(|&(_, len)| len);

        match best {
            Some((kind, len)) => {
                self.pos += len;
                self.push_op(kind, start);
            }
            None => {
                let ch = self.char_at(self.pos);
                self.pos += ch.len_utf8();
                self.push(TokKind::Unknown, start, self.pos);
            }
        }
    }

    /// Return the Unicode operator `lead` bytes ahead as `(kind, cursor_len)`.
    fn unicode_op_at(&self, lead: usize) -> Option<(TokKind, usize)> {
        let ch = self.char_at(self.pos + lead);
        let dotted = lead == 1;
        let eq = self.peek(lead + ch.len_utf8()) == Some(b'=');

        if ch == MINUS_SIGN {
            let kind = match (dotted, eq) {
                (false, false) => TokKind::Minus,
                (false, true) => TokKind::MinusEq,
                (true, false) => TokKind::DotMinus,
                (true, true) => TokKind::DotMinusEq,
            };
            return Some((kind, lead + ch.len_utf8() + usize::from(eq)));
        }

        if eq && matches!(ch, DIVIDE_SIGN | XOR_SIGN) {
            let kind = match (dotted, ch == DIVIDE_SIGN) {
                (false, true) => TokKind::DivEq,
                (false, false) => TokKind::XorEq,
                (true, true) => TokKind::DotDivEq,
                (true, false) => TokKind::DotXorEq,
            };
            return Some((kind, lead + ch.len_utf8() + 1));
        }

        let kind = unicode_op_kind(ch)?;
        if dotted && !(is_unicode_infix_tier(kind) || kind == TokKind::UniRadical) {
            return None;
        }
        Some((kind, lead + ch.len_utf8()))
    }
}

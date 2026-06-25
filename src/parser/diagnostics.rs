/// The classification of a [`ParseDiagnostic`]. The projector
/// (`src/parser/sexpr.rs`) keys off the recovery kinds to reconstruct
/// JuliaSyntax's `(error-t)`/`(error)` error shapes without dedicated CST nodes;
/// the remaining kinds document the recovery taxonomy and feed the side-channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    // --- zero-width markers: no CST node; the projector splices `(error-t)` at a
    // recorded byte point (or count, for the multiplicity cases) ---
    /// A block form truncated before its `end` (`if c\n x`). Anchored at the
    /// opening keyword.
    MissingEnd,
    /// A `try` with no `catch`/`finally` handler. Anchored at the `try` keyword.
    MissingTryHandler,
    /// A string literal glued to another term (`"a"x`, `2"b"`). Anchored at the
    /// left operand's end.
    StringJuxtapose,
    /// Disallowed whitespace before a field-access dot (`x .y`). Anchored at the
    /// dot's end.
    DotWhitespace,
    /// Disallowed whitespace after `:` before a quoted symbol (`: foo`). Anchored
    /// at the `:`'s end.
    QuoteColonWhitespace,
    /// A quote-paren `:(…)` whose body can't start an expression — a closing
    /// block keyword (`:(end)`, `:(else)`, `:(catch)`) — so the quoted form is a
    /// zero-width `(error-t)` and the keyword spills to the trailing-junk driver
    /// (`:(end)` ⇒ `(quote-: (error-t)) (error-t end ✘)`). Anchored at the `(`'s
    /// end; the `(` stays a loose token child of the `QUOTE_SYM`.
    EmptyQuoteParen,
    /// A numeric/flag suffix glued after a string-macro close (`var"x"2`).
    /// Anchored at the literal's start.
    StringSuffixSpace,
    /// A string/command/`var"…"` literal with no closing delimiter. Anchored at
    /// the literal's start.
    UnterminatedLiteral,
    /// A comprehension/generator `for` glued to the preceding element
    /// (`[(x)for x in xs]`). Anchored at the `for`'s start.
    GluedFor,
    /// Disallowed whitespace before a postfix/broadcast opener (`f (a)`, `f. (x)`).
    /// Anchored at the opener's start.
    OpenerWhitespace,
    /// Disallowed whitespace before the call-form `(` of a unary-prefix operator
    /// (`+ (a,b)` → `(call + (error) a b)`). Distinct from `OpenerWhitespace`: a
    /// valid unary operator callee projects a zero-width `(error)`, not the
    /// `(error-t)` of an identifier callee. Anchored at the opener's start.
    PrefixOpenerWhitespace,
    /// An argument list with no closing delimiter (`f(a`). Anchored at the opener's
    /// start.
    UnterminatedArgList,
    /// Disallowed whitespace around a ternary `?` (`a?b`). Anchored at the `?`'s
    /// end; pushed once per missing side.
    TernaryQWhitespace,
    /// Disallowed/absent whitespace around a ternary `:` (`a ? b: c`, `a ? b c`).
    /// Anchored at the true-branch's end; pushed once per missing side.
    TernaryColonWhitespace,
    /// `else if` written on one line (`if a … else if b … end`) — JuliaSyntax
    /// recovers it as an `elseif` clause, splicing a zero-width `(error-t)` into
    /// the (missing) else position. Anchored at the opening `if` keyword.
    ElseIf,
    /// A space and `;;` separator mixed in one array (`[a b ;; c]`,
    /// `[a ;; b c]`) — JuliaSyntax establishes a row-/column-major order from the
    /// first space/`;;` separator and flags a later conflicting one, splicing a
    /// zero-width `(error-t)` after the element preceding it. Anchored at that
    /// element's end byte.
    ArraySeparatorMismatch,

    // --- zero-width point driving a *wrapping* `(error …)` reconstruction: the
    // CST topology is faithful; the projector wraps the whole node from the
    // recorded diagnostic ---
    /// A `const` whose declaration is not a plain `=` assignment (`const x`,
    /// `const x += 1`, `const global x`) — JuliaSyntax wraps the `const` in
    /// `(error …)`. Anchored at the `const` keyword start.
    ConstNotAssignment,
    /// A `function`/`macro` whose signature is a bare identifier (`f`, `$f`) but
    /// which has a non-empty body (`function f body end`, `function f; end`) —
    /// JuliaSyntax error-wraps the name (`(function (error f) (block body))`). A
    /// bare-name header with a truly empty body (`function f end`) is instead the
    /// valid forward-declaration form `(function f)` and is left alone. Anchored
    /// at the `SIGNATURE` node's start.
    InvalidFunctionSignature,
    /// A `catch` variable that is not a plain identifier (`catch e+3`,
    /// `catch e.f`, `catch f(e)`) — JuliaSyntax wraps the variable expression in
    /// `(error …)` (`(catch (error (call-i e + 3)) …)`). A bare identifier,
    /// `$`-interpolation, or `var"…"` non-standard identifier is left alone.
    /// Anchored at the catch-variable node's start.
    CatchVarNotIdentifier,
    /// An `export` item parenthesized around something other than a single symbol
    /// (`export (x::T)`, `export (x, y)`, `export ()`, `export ((x))`) —
    /// JuliaSyntax error-wraps the parenthesized expression (`(export (error
    /// (::-i x T)))`). A paren around a single identifier, operator, `var"…"`, or
    /// `$`-interpolation (`export (x)`, `export (+)`, `export ($a)`) is the valid
    /// unwrapped form and is left alone. Anchored at the parenthesized node's
    /// start.
    InvalidExportItem,
    /// A macro name that is a non-identifier bracketed expression (`@[x]`,
    /// `@{x}`) — JuliaSyntax parses the expression and error-wraps it as the macro
    /// name (`@[x] y z` ⇒ `(macrocall (error (vect x)) y z)`). Anchored at the
    /// bracketed expression's start.
    InvalidMacroName,
    /// A broadcast call `.(…)` applied to a macro name (`@M.(x)`) — invalid, since
    /// a macro cannot be broadcast. JuliaSyntax re-heads it as a macrocall wrapping
    /// the dotcall with a zero-width marker (`@M.(x)` ⇒
    /// `(macrocall (dotcall @M (error-t) x))`). Anchored at the broadcast `(` opener.
    MacroDotBroadcast,
    /// A misplaced macro sigil in a qualified macro name: the `@` sits on a
    /// non-final component with a trailing `.ident` continuation (`A.@B.x`).
    /// JuliaSyntax relocates the sigil to the final component and splices a
    /// zero-width marker (`A.@B.x` ⇒ `(macrocall (. (. A (quote B)) (error-t)
    /// (quote @x)))`). Anchored at the `@` sigil.
    MacroSigilTrailing,
    /// A leading-`@` qualified macro name whose dotted path is structurally
    /// invalid: the final component is a `$`-interpolation (`@A.$x` ⇒
    /// `(macrocall (. A (inert (error x))))`) or the path carries a second `@`
    /// sigil (`@A.B.@x` ⇒ `(macrocall (. (. A (quote B)) (quote (error-t) @x)))`).
    /// JuliaSyntax relocates the sigil to the final component, turning the
    /// excess sigil/interpolation into zero-width `(error-t)`/`(error …)`.
    /// Anchored at the leading `@` sigil.
    MacroSigilLeading,

    // --- byte-bearing recovery: the run is wrapped in a real `ERROR` node and the
    // projector renders it as `(error-t …)` (the diagnostic falls inside the node) ---
    /// A stray closing delimiter swallowing the rest of the line (`) x`).
    StrayCloser,
    /// Junk after the first statement on a separator-less line (`x y`).
    TrailingJunk,
    /// A clause after a recovery `:` in `import`/`using` (`import A, B: y`).
    ImportRecoveryColon,

    // --- byte-bearing `ERROR` nodes rendered as the plain `(error …)` (default);
    // kinds recorded for the side-channel only ---
    /// An `else` clause before any `catch` (`try x else y end`).
    ElseWithoutCatch,
    /// A binary-only operator used in prefix position (`/x`, `.*x`) —
    /// JuliaSyntax error-wraps the operator and applies it as a prefix call.
    InvalidPrefixOperator,
    /// A syntactic operator with no value meaning used where an atom is expected
    /// (`=`, `+=`, `&&`, `||`, `->`, `...`, `?`) — JuliaSyntax emits `(error op)`.
    LoneOperator,
    /// An `as` rename invalid in this position (`using A as B`).
    InvalidAsAlias,
    MissingOperand,
    MissingWhereBound,
    MissingStruct,
    MissingCondition,
    UnclosedParen,
    UnclosedComprehension,
    MissingTernaryTrue,
    MissingTernaryFalse,
    MissingTernaryColon,
    /// A `$(…)` string interpolation whose parens hold a multi-value form — a
    /// block (`$(x;y)`), tuple (`$(x,y)`), generator (`$(x for …)`), or the empty
    /// `$()` — rather than a single expression. JuliaSyntax renders the operand as
    /// `(error …)`; the projector reconstructs that shape from the CST topology.
    InvalidInterpolation,
    /// A reserved keyword used as the name in a `struct`/`module`/`function`/
    /// `macro` signature (`struct try end`, `function begin() end`) — JuliaSyntax
    /// error-wraps the keyword as the name (`(error try)`). Anchored at the
    /// keyword. The CST holds a real `ERROR` node around the keyword.
    InvalidNameKeyword,
    /// A middle/closing block keyword (`end`, `else`, `elseif`, `catch`,
    /// `finally`) appearing where a statement is expected (`@doc x\nend`,
    /// `end y z`) — JuliaSyntax wraps the keyword alone in `(error <kw>)` and
    /// bumps the rest of the line as a separate trailing-junk run. Anchored at
    /// the keyword token; the CST holds a real `ERROR` node around it.
    StrayKeyword,
    /// A range colon glued directly to a single `<` or `>` (`a :< b`, `a:>b`) —
    /// JuliaSyntax lexes the pair as one invalid operator at the colon
    /// precedence tier and heads the infix call with both tokens error-wrapped
    /// (`a :< b` ⇒ `(call-i a (error : <) b)`). Recorded as a zero-width point at
    /// the colon; the two operator tokens stay loose children of the
    /// `BINARY_EXPR`.
    InvalidGluedOperator,
    /// An incomplete ternary whose missing `:`/false-branch is terminated by a
    /// closing block keyword (`end`/`elseif`/`else`/`catch`/`finally`) — `x ? true
    /// end`, `x ? true : elseif …`. JuliaSyntax re-heads the recovered node from
    /// `?` to `if`, splicing one zero-width `(error-t)` per missing piece (no
    /// colon ⇒ two markers `(if x true (error-t) (error-t))`; colon present but
    /// false missing ⇒ one `(if x true (error-t))`). Recorded as a zero-width
    /// point at the `?`'s end, pushed once per trailing marker; the projector
    /// keys head and marker count off the multiplicity.
    IncompleteTernaryIf,
}

/// A parse-time diagnostic: a classified message anchored to a byte range in the
/// source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
    pub start: usize,
    pub end: usize,
}

pub(crate) fn push_diagnostic(
    diagnostics: &mut Vec<ParseDiagnostic>,
    kind: DiagnosticKind,
    message: &str,
    start: usize,
    end: usize,
) {
    diagnostics.push(ParseDiagnostic {
        kind,
        message: message.to_string(),
        start,
        end,
    });
}

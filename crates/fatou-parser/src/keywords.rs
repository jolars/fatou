//! The keyword table: the one list every keyword-shaped table is generated from.
//!
//! Julia's reserved words are needed in several shapes — as text (completion),
//! as a text -> [`TokKind`](crate::parser) classifier, as a predicate over
//! `TokKind`, as the `TokKind` -> [`SyntaxKind`](crate::syntax::SyntaxKind)
//! materialization, and as a predicate over `SyntaxKind`. Writing the set out
//! once per shape is how they drift.
//!
//! Instead, each site defines a generator macro and expands [`keyword_table`]
//! with it: the table below is handed to the generator as rows of
//! `"text" TokKindVariant SYNTAX_KIND_VARIANT,`, and the generator emits
//! whatever shape it needs (ignoring the columns it does not use). Adding a
//! keyword is one row here; every shape follows.
//!
//! The callback indirection is what lets the table live at the crate root: it
//! passes bare tokens, so it never has to name `TokKind` (private to the
//! `parser` module tree) or `SyntaxKind` itself.

/// Expand `$callback` with the keyword table.
///
/// `$callback` must be a macro accepting rows of
/// `"text" TokKindVariant SYNTAX_KIND_VARIANT,` and must already be in scope
/// (`macro_rules!` macros are textually scoped, so define it just above the
/// call).
macro_rules! keyword_table {
    ($callback:ident) => {
        $callback! {
            "function"   FunctionKw    FUNCTION_KW,
            "macro"      MacroKw       MACRO_KW,
            "end"        EndKw         END_KW,
            "if"         IfKw          IF_KW,
            "elseif"     ElseifKw      ELSEIF_KW,
            "else"       ElseKw        ELSE_KW,
            "begin"      BeginKw       BEGIN_KW,
            "true"       TrueKw        TRUE_KW,
            "false"      FalseKw       FALSE_KW,
            "while"      WhileKw       WHILE_KW,
            "for"        ForKw         FOR_KW,
            "do"         DoKw          DO_KW,
            "let"        LetKw         LET_KW,
            "quote"      QuoteKw       QUOTE_KW,
            "try"        TryKw         TRY_KW,
            "catch"      CatchKw       CATCH_KW,
            "finally"    FinallyKw     FINALLY_KW,
            "struct"     StructKw      STRUCT_KW,
            "mutable"    MutableKw     MUTABLE_KW,
            "module"     ModuleKw      MODULE_KW,
            "baremodule" BaremoduleKw  BAREMODULE_KW,
            "return"     ReturnKw      RETURN_KW,
            "break"      BreakKw       BREAK_KW,
            "continue"   ContinueKw    CONTINUE_KW,
            "const"      ConstKw       CONST_KW,
            "global"     GlobalKw      GLOBAL_KW,
            "local"      LocalKw       LOCAL_KW,
            "import"     ImportKw      IMPORT_KW,
            "using"      UsingKw       USING_KW,
            "export"     ExportKw      EXPORT_KW,
            "where"      WhereKw       WHERE_KW,
        }
    };
}

pub(crate) use keyword_table;

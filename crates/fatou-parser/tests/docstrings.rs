use fatou_parser::ast::{
    AstNode, Doc, DocAttachment, DocAttachmentKind, DocText, OpaqueDocReason, StaticDocText,
};
use fatou_parser::parser::parse;
use rowan::TextRange;

fn attachment(source: &str) -> DocAttachment {
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let node = parsed.cst.children().next().expect("one top-level node");
    DocAttachment::cast(node).expect("documentation attachment")
}

fn static_text(source: &str) -> StaticDocText {
    match attachment(source).text() {
        DocText::Static(text) => text,
        other => panic!("expected static documentation, got {other:?}"),
    }
}

#[test]
fn typed_doc_exposes_literal_and_target() {
    let parsed = parse("\"Docs.\"\nf(x) = x\n");
    let node = parsed.cst.children().next().unwrap();
    let doc = Doc::cast(node).expect("DOC wrapper");

    assert_eq!(
        doc.literal().unwrap().syntax().text().to_string(),
        "\"Docs.\""
    );
    assert_eq!(
        doc.target().unwrap().syntax().text().to_string(),
        "f(x) = x"
    );
}

#[test]
fn recognizes_ordinary_and_qualified_doc_attachments() {
    let ordinary = attachment("\"Docs.\"\nf(x) = x\n");
    assert_eq!(ordinary.kind(), DocAttachmentKind::Juxtaposed);
    assert_eq!(ordinary.payload().text().to_string(), "\"Docs.\"");
    assert_eq!(ordinary.target().text().to_string(), "f(x) = x");

    for (source, target) in [
        ("@doc \"Docs.\" f(x) = x\n", "f(x) = x"),
        ("@doc(\"Docs.\", f)\n", "f"),
        ("Base.@doc \"Docs.\" f(x) = x\n", "f(x) = x"),
        ("@Base.doc \"Docs.\" f(x) = x\n", "f(x) = x"),
    ] {
        let explicit = attachment(source);
        assert_eq!(explicit.kind(), DocAttachmentKind::Macro);
        assert_eq!(explicit.payload().text().to_string(), "\"Docs.\"");
        assert_eq!(explicit.target().text().to_string(), target);
    }
}

#[test]
fn retrieval_and_noncanonical_arity_are_not_attachments() {
    for source in [
        "@doc f\n",
        "@doc \"Docs.\" f extra\n",
        "raw\"Docs.\"\nf() = 1\n",
    ] {
        let parsed = parse(source);
        let node = parsed.cst.children().next().unwrap();
        assert!(DocAttachment::cast(node).is_none(), "{source}");
    }
}

#[test]
fn decodes_ordinary_string_content_and_maps_escapes() {
    let source = "\"A\\nβ\"\nf() = 1\n";
    let text = static_text(source);
    assert_eq!(text.as_str(), "A\nβ");

    let newline = TextRange::new(1.into(), 2.into());
    let escaped = text.source_map().source_range(newline).unwrap();
    let escape_start = source.find("\\n").unwrap() as u32;
    assert_eq!(
        escaped,
        TextRange::new(escape_start.into(), (escape_start + 2).into())
    );

    let beta = TextRange::new(2.into(), 4.into());
    let beta_source = source.find('β').unwrap() as u32;
    assert_eq!(
        text.source_map().source_range(beta).unwrap(),
        TextRange::new(beta_source.into(), (beta_source + 2).into())
    );
}

#[test]
fn triple_strings_normalize_newlines_and_dedent_with_mapping() {
    let source = "\"\"\"\r\n    alpha\r\n      beta\r\n    \"\"\"\nf() = 1\n";
    let text = static_text(source);
    assert_eq!(text.as_str(), "alpha\n  beta\n");

    let alpha = TextRange::new(0.into(), 5.into());
    let alpha_start = source.find("alpha").unwrap() as u32;
    assert_eq!(
        text.source_map().source_range(alpha).unwrap(),
        TextRange::new(alpha_start.into(), (alpha_start + 5).into())
    );

    let first_newline = TextRange::new(5.into(), 6.into());
    let crlf = source.find("alpha\r\n").unwrap() as u32 + 5;
    assert_eq!(
        text.source_map().source_range(first_newline).unwrap(),
        TextRange::new(crlf.into(), (crlf + 2).into())
    );
}

#[test]
fn raw_doc_payload_is_static_but_other_payloads_are_opaque() {
    let raw = static_text("@doc raw\"\\n\" f\n");
    assert_eq!(raw.as_str(), "\\n");

    let cases = [
        (
            "\"value = $(x)\"\nf() = x\n",
            OpaqueDocReason::Interpolation,
        ),
        ("@doc r\"pattern\" f\n", OpaqueDocReason::UnsupportedPrefix),
        ("@doc r\"pattern\"i f\n", OpaqueDocReason::UnsupportedSuffix),
        ("@doc make_docs() f\n", OpaqueDocReason::NonString),
    ];
    for (source, expected) in cases {
        assert_eq!(attachment(source).text(), DocText::Opaque(expected));
    }
}

#[test]
fn line_continuations_are_removed_after_triple_dedent() {
    let source = "\"\"\"\n    alpha \\\n      beta\n    \"\"\"\nf() = 1\n";
    let text = static_text(source);
    assert_eq!(text.as_str(), "alpha beta\n");

    let beta = text.as_str().find("beta").unwrap() as u32;
    let source_beta = source.find("beta").unwrap() as u32;
    assert_eq!(
        text.source_map()
            .source_range(TextRange::new(beta.into(), (beta + 4).into()))
            .unwrap(),
        TextRange::new(source_beta.into(), (source_beta + 4).into())
    );
}

#[test]
fn malformed_static_escape_is_invalid() {
    assert_eq!(
        attachment("\"bad \\q\"\nf() = 1\n").text(),
        DocText::Invalid(fatou_parser::parser::StringDecodeError::BadEscape)
    );
}

#[test]
fn byte_escapes_must_form_utf8() {
    assert_eq!(static_text("\"\\xce\\xb1\"\nf() = 1\n").as_str(), "α");
    assert_eq!(
        attachment("\"\\xff\"\nf() = 1\n").text(),
        DocText::Invalid(fatou_parser::parser::StringDecodeError::BadUtf8)
    );
}

#[test]
fn empty_static_text_maps_an_empty_range_inside_the_literal() {
    let text = static_text("\"\"\nf() = 1\n");
    assert_eq!(text.as_str(), "");
    assert_eq!(
        text.source_map()
            .source_range(TextRange::new(0.into(), 0.into()))
            .unwrap(),
        TextRange::new(1.into(), 1.into())
    );
}

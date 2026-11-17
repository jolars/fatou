use fatou_formatter::verify::verify_format_with_options;
use fatou_formatter::{
    FormatOptions, FormatStyle, format_verified_with_options, format_with_options,
};
use fatou_parser::parser::{ParseOptions, SyntaxEdition, parse_with_options};

#[test]
fn formatting_and_verification_share_the_selected_edition() {
    let options = FormatOptions {
        style: FormatStyle::default(),
        parse_options: ParseOptions {
            syntax_edition: SyntaxEdition::Julia113,
        },
    };
    let input = "module A\ntypegroup=1\nend\n";
    let output = format_verified_with_options(input, options).unwrap();
    assert_eq!(output, "module A\ntypegroup = 1\nend\n");
    verify_format_with_options(input, &output, options.parse_options).unwrap();
    assert!(format_verified_with_options(input, FormatOptions::default()).is_err());
}

#[test]
fn formatting_reparses_and_is_idempotent_in_each_edition() {
    for edition in SyntaxEdition::ALL {
        for width in [24, 80] {
            for newline in ["\n", "\r\n"] {
                let options = FormatOptions {
                    style: FormatStyle {
                        line_width: width,
                        ..FormatStyle::default()
                    },
                    parse_options: ParseOptions {
                        syntax_edition: edition,
                    },
                };
                for input in [
                    "(@info \"message\" x=long_function(argument_one, argument_two))",
                    "module A\nf(x)=x+1\nend",
                    "x ? return a : y",
                    "(@m x\n - y)",
                ] {
                    let input = input.replace('\n', newline);
                    if !parse_with_options(&input, options.parse_options)
                        .diagnostics
                        .is_empty()
                    {
                        continue;
                    }
                    let output = format_with_options(&input, options).unwrap();
                    assert!(
                        parse_with_options(&output, options.parse_options)
                            .diagnostics
                            .is_empty(),
                        "{edition}: {output:?}"
                    );
                    verify_format_with_options(&input, &output, options.parse_options).unwrap();
                    assert_eq!(format_with_options(&output, options).unwrap(), output);
                }
            }
        }
    }
}

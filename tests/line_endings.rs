//! Losslessness must hold regardless of line-ending style.

use fatou::parser::reconstruct;

#[test]
fn crlf_lf_and_cr_round_trip() {
    for input in [
        "a = 1\nb = 2\n",
        "a = 1\r\nb = 2\r\n",
        "a = 1\rb = 2\r",
        "function f()\r\n    x\r\nend\r\n",
    ] {
        assert_eq!(reconstruct(input), input, "round-trip failed: {input:?}");
    }
}

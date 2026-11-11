// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn raw_and_escaped_literals_preserve_their_values_and_reject_extra_syntax() {
    let source = r###"// old generated data
pub(super) const TOP: &str = r##"quotes \"# and Unicode 🐈\n"##;
pub(super) const TEXTS: &[(&str, &str)] = &[("x", "a\\b\n\r\t\0\x21\u{1f408}"),];
pub(super) const ALIASES: &[(&str, &str)] = &[("y", "x")];
"###;
    let got = tables(source).unwrap();
    assert_eq!(got.top, "quotes \\\"# and Unicode 🐈\\n");
    assert_eq!(got.texts["x"], "a\\b\n\r\t\0!🐈");
    assert_eq!(got.aliases["y"], "x");
    assert!(tables(&format!("{source}fn side_effect() {{}}")).is_err());
    assert!(tables(&source.replace("(\"y\", \"x\")", "(\"y\", \"x\"), (\"y\", \"x\")")).is_err());
    assert!(tables(&source.replace("\\u{1f408}", "\\u{_1f408}")).is_err());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn os_strings_forward_wide_units_to_command_line_and_env_block() {
    let argv = [OsString::from("C:\\a dir\\"), OsString::from("C:\\a dir\\")];
    let cmd = build_command_line(&argv);
    let text = String::from_utf16(cmd.strip_suffix(&[0]).expect("trailing NUL")).unwrap();
    assert_eq!(text, "\"C:\\a dir\\\" \"C:\\a dir\\\\\"");
    assert_eq!(
        wide_nul(OsStr::new("ab")),
        vec![b'a' as u16, b'b' as u16, 0]
    );
    let env = [(OsString::from("Path"), OsString::from("C:\\bin"))];
    let block = build_env_block(&env);
    assert_eq!(&block[block.len() - 2..], &[0, 0]);
}

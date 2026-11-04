// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `OsStr`/`OsString` adapters onto [`crate::cmdline_units`] for ConPTY `CreateProcessW`
//! command-line quoting and environment-block encoding.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;

use crate::cmdline_units::{build_command_line_units, build_env_block_units};

/// Encode `s` as UTF-16 with a trailing NUL (the shape every `*W` API wants).
pub(crate) fn wide_nul(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Flatten `argv` into one NUL-terminated UTF-16 command line with the MSVCRT quoting
/// rules (`argv[0]` must not contain `'"'`; `CreateProcessW` takes a mutable NUL-terminated
/// buffer).
pub(crate) fn build_command_line(argv: &[OsString]) -> Vec<u16> {
    build_command_line_units(argv.iter().map(|arg| arg.encode_wide().collect::<Vec<_>>()))
}

/// The `CreateProcessW` UTF-16 environment block from `build_child_env`'s output:
/// `KEY=VAL\0 … \0\0`, deduped case-insensitively with the last occurrence winning (an
/// inherited `Path` and an injected `PATH` must yield one entry, the injected one) and
/// sorted case-insensitively by key, as `CreateProcessW` requires.
pub(crate) fn build_env_block(pairs: &[(OsString, OsString)]) -> Vec<u16> {
    let encoded: Vec<(Vec<u16>, Vec<u16>)> = pairs
        .iter()
        .map(|(k, v)| (k.encode_wide().collect(), v.encode_wide().collect()))
        .collect();
    build_env_block_units(&encoded)
}

#[cfg(test)]
mod tests {
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
}

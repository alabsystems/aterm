// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Preserve path arguments when bottle variables expand to a prefix with spaces.
//!
//! pkgconf expands variables before parsing flag words. Variable assignments must
//! remain raw paths (`--variable=libdir` is not a shell command), so quote the
//! path-bearing flag word rather than escaping the prefix assignment.

use std::path::Path;

pub(super) fn prepare(path: &Path, bytes: Vec<u8>) -> Vec<u8> {
    if path.extension().is_none_or(|extension| extension != "pc") {
        return bytes;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return bytes;
    };
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let Some((field, flags)) = line.split_once(':') else {
            out.push_str(line);
            continue;
        };
        if !matches!(
            field.trim(),
            "Libs" | "Libs.private" | "Cflags" | "Cflags.private"
        ) {
            out.push_str(line);
            continue;
        }
        out.push_str(field);
        out.push(':');
        quote_flags(flags, &mut out);
    }
    out.into_bytes()
}

fn quote_flags(flags: &str, out: &mut String) {
    let bytes = flags.as_bytes();
    let mut cursor = 0;
    let mut path_operand = false;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            out.push(char::from(bytes[cursor]));
            cursor += 1;
            continue;
        }
        let start = cursor;
        let mut quote = None;
        let mut escaped = false;
        let mut shell_syntax = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if escaped {
                escaped = false;
            } else if byte == b'\\' && quote != Some(b'\'') {
                escaped = true;
                shell_syntax = true;
            } else if quote == Some(byte) {
                quote = None;
            } else if quote.is_none() && matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
                shell_syntax = true;
            } else if quote.is_none() && byte.is_ascii_whitespace() {
                break;
            }
            cursor += 1;
        }
        let word = &flags[start..cursor];
        let attached_path = ["-L", "-I", "-F"]
            .iter()
            .any(|flag| word.starts_with(flag) && word.len() > flag.len());
        let variable = word.contains("${") || word.contains("@@HOMEBREW");
        if !shell_syntax && variable && (path_operand || attached_path) {
            out.push('"');
            out.push_str(word);
            out.push('"');
        } else {
            out.push_str(word);
        }
        path_operand = matches!(word, "-L" | "-I" | "-F");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_variables_and_flag_lists_but_protects_path_words() {
        let source = b"prefix=@@HOMEBREW_PREFIX@@\nlibdir=${prefix}/lib\nName: ${name}\nLibs: -L${libdir} -lzstd ${extra_libs}\nCflags: -I ${prefix}/include -F\"${frameworks}\" -DNAME=value\n";
        let actual = prepare(Path::new("libzstd.pc"), source.to_vec());
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            "prefix=@@HOMEBREW_PREFIX@@\nlibdir=${prefix}/lib\nName: ${name}\nLibs: \"-L${libdir}\" -lzstd ${extra_libs}\nCflags: -I \"${prefix}/include\" -F\"${frameworks}\" -DNAME=value\n"
        );
        assert_eq!(prepare(Path::new("other.conf"), source.to_vec()), source);
    }
}

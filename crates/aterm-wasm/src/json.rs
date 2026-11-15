// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Shared JSON string encoding for both web renderers.

/// Append a quoted JSON string without allocating an intermediate escaped string.
pub(crate) fn write_json_string(out: &mut String, text: &str) {
    const CONTROL: [&str; 32] = [
        r"\u0000", r"\u0001", r"\u0002", r"\u0003", r"\u0004", r"\u0005", r"\u0006", r"\u0007",
        r"\u0008", r"\t", r"\n", r"\u000b", r"\u000c", r"\r", r"\u000e", r"\u000f", r"\u0010",
        r"\u0011", r"\u0012", r"\u0013", r"\u0014", r"\u0015", r"\u0016", r"\u0017", r"\u0018",
        r"\u0019", r"\u001a", r"\u001b", r"\u001c", r"\u001d", r"\u001e", r"\u001f",
    ];
    out.push('"');
    let mut start = 0;
    for (i, &byte) in text.as_bytes().iter().enumerate() {
        if byte >= 0x20 && byte != b'"' && byte != b'\\' {
            continue;
        }
        // Only ASCII bytes delimit spans, so both ends are UTF-8 boundaries.
        if start != i {
            out.push_str(&text[start..i]);
        }
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            byte => out.push_str(CONTROL[usize::from(byte)]),
        }
        start = i + 1;
    }
    out.push_str(&text[start..]);
    out.push('"');
}

/// Return an owned quoted JSON string when the caller retains the encoded value.
pub(crate) fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    write_json_string(&mut out, text);
    out
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;

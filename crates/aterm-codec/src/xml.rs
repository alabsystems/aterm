// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XML's text escape, shared by every plist and XML document the workspace writes.

/// `text` with XML's five special characters escaped, for element text and attribute values alike.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

/// [`escape`] appending into a caller-owned buffer.
pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_covers_the_five_entities() {
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(escape("&amp;"), "&amp;amp;");
        assert_eq!(escape("plain/π"), "plain/π");
        let mut out = String::from("<string>");
        escape_into(&mut out, "x<y");
        assert_eq!(out, "<string>x&lt;y");
    }
}

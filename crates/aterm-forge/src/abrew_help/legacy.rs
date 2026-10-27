// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded reader for the old generated TOP/TEXTS/ALIASES declarations.

use super::Tables;
use std::collections::BTreeMap;

pub(super) fn tables(source: &str) -> Result<Tables, String> {
    let mut reader = Reader { remaining: source };
    reader.declaration("TOP", false)?;
    let top = reader.string()?;
    reader.token(";")?;
    reader.declaration("TEXTS", true)?;
    let texts = reader.pairs()?;
    reader.declaration("ALIASES", true)?;
    let aliases = reader.pairs()?;
    reader.space();
    if !reader.remaining.is_empty() {
        return Err("unrecognized syntax after the legacy tables".into());
    }
    Ok(Tables {
        top,
        texts,
        aliases,
    })
}

struct Reader<'a> {
    remaining: &'a str,
}

impl Reader<'_> {
    fn space(&mut self) {
        loop {
            self.remaining = self.remaining.trim_start_matches(char::is_whitespace);
            if self.remaining.starts_with("//") {
                self.remaining = self.remaining.split_once('\n').map_or("", |(_, rest)| rest);
            } else {
                break;
            }
        }
    }

    fn take(&mut self, token: &str) -> bool {
        self.space();
        if let Some(rest) = self.remaining.strip_prefix(token) {
            if token
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
                && rest
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                return false;
            }
            self.remaining = rest;
            true
        } else {
            false
        }
    }

    fn token(&mut self, token: &str) -> Result<(), String> {
        if self.take(token) {
            Ok(())
        } else {
            Err(format!("expected {token:?} in the legacy table shape"))
        }
    }

    fn declaration(&mut self, name: &str, pairs: bool) -> Result<(), String> {
        for token in ["pub", "(", "super", ")", "const", name, ":", "&"] {
            self.token(token)?;
        }
        if pairs {
            for token in ["[", "(", "&", "str", ",", "&", "str", ")", "]"] {
                self.token(token)?;
            }
        } else {
            self.token("str")?;
        }
        self.token("=")
    }

    fn pairs(&mut self) -> Result<BTreeMap<String, String>, String> {
        self.token("&")?;
        self.token("[")?;
        let mut pairs = BTreeMap::new();
        while !self.take("]") {
            self.token("(")?;
            let name = self.string()?;
            self.token(",")?;
            let value = self.string()?;
            self.take(",");
            self.token(")")?;
            if pairs.insert(name.clone(), value).is_some() {
                return Err(format!("duplicate legacy table name {name:?}"));
            }
            if !self.take(",") {
                self.token("]")?;
                break;
            }
        }
        self.token(";")?;
        Ok(pairs)
    }

    fn character(&mut self) -> Result<char, String> {
        let ch = self
            .remaining
            .chars()
            .next()
            .ok_or("unterminated Rust string")?;
        self.remaining = &self.remaining[ch.len_utf8()..];
        Ok(ch)
    }

    fn string(&mut self) -> Result<String, String> {
        self.space();
        if self.remaining.starts_with('r') {
            self.remaining = &self.remaining[1..];
            let hashes = self.remaining.bytes().take_while(|b| *b == b'#').count();
            if hashes > 255 {
                return Err("raw string fence exceeds Rust's limit".into());
            }
            self.remaining = &self.remaining[hashes..];
            if !self.remaining.starts_with('"') {
                return Err("expected a raw Rust string".into());
            }
            self.remaining = &self.remaining[1..];
            let ending = format!("\"{}", "#".repeat(hashes));
            let end = self
                .remaining
                .find(&ending)
                .ok_or("unterminated raw Rust string")?;
            let text = self.remaining[..end].to_owned();
            if text.contains('\r') {
                return Err("legacy raw strings with carriage returns are not supported".into());
            }
            self.remaining = &self.remaining[end + ending.len()..];
            return Ok(text);
        }
        if !self.remaining.starts_with('"') {
            return Err("expected a Rust string literal".into());
        }
        self.remaining = &self.remaining[1..];
        let mut text = String::new();
        loop {
            match self.character()? {
                '"' => return Ok(text),
                '\r' => return Err("bare carriage return in legacy Rust string".into()),
                '\\' => match self.character()? {
                    'n' => text.push('\n'),
                    'r' => text.push('\r'),
                    't' => text.push('\t'),
                    '0' => text.push('\0'),
                    '\\' => text.push('\\'),
                    '"' => text.push('"'),
                    '\'' => text.push('\''),
                    '\n' => {
                        self.remaining = self.remaining.trim_start_matches(char::is_whitespace);
                    }
                    'x' => {
                        let a = self.character()?.to_digit(16).ok_or("invalid hex escape")?;
                        let b = self.character()?.to_digit(16).ok_or("invalid hex escape")?;
                        let value = a * 16 + b;
                        if value > 127 {
                            return Err("non-ASCII Rust hex escape".into());
                        }
                        text.push(char::from_u32(value).ok_or("invalid hex scalar")?);
                    }
                    'u' => {
                        if self.character()? != '{' {
                            return Err("invalid Unicode escape".into());
                        }
                        let mut hex = String::new();
                        loop {
                            let ch = self.character()?;
                            if ch == '}' {
                                break;
                            }
                            if ch != '_' {
                                if !ch.is_ascii_hexdigit() || hex.len() == 6 {
                                    return Err("invalid Unicode escape digits".into());
                                }
                                hex.push(ch);
                            }
                        }
                        let value = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                        text.push(char::from_u32(value).ok_or("invalid Unicode scalar")?);
                    }
                    _ => return Err("unsupported Rust string escape".into()),
                },
                ch => text.push(ch),
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert!(
            tables(&source.replace("(\"y\", \"x\")", "(\"y\", \"x\"), (\"y\", \"x\")")).is_err()
        );
    }
}

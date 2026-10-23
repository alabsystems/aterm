// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Just enough XML for Wayland protocol descriptions.

/// One element.
#[derive(Debug, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    /// The attribute `name`, if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The children named `name`.
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
}

fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';').ok_or("an unterminated entity")?;
        out.push(match &tail[..=end] {
            "&lt;" => '<',
            "&gt;" => '>',
            "&amp;" => '&',
            "&quot;" => '"',
            "&apos;" => '\'',
            other => return Err(format!("an unknown entity {other}")),
        });
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Parse a document: its one root element.
pub fn parse(src: &str) -> Result<Element, String> {
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut i = 0;
    let b = src.as_bytes();
    while i < b.len() {
        if b[i] != b'<' {
            let end = src[i..].find('<').map_or(b.len(), |j| i + j);
            let text = unescape(&src[i..end]).map_err(|e| format!("{e} at {i}"))?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&text);
            }
            i = end;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<?") {
            i += rest
                .find("?>")
                .ok_or(format!("an unterminated declaration at {i}"))?
                + 2;
        } else if rest.starts_with("<!--") {
            i += rest
                .find("-->")
                .ok_or(format!("an unterminated comment at {i}"))?
                + 3;
        } else if let Some(close) = rest.strip_prefix("</") {
            let end = close
                .find('>')
                .ok_or(format!("an unterminated end tag at {i}"))?;
            let name = close[..end].trim();
            let done = stack.pop().ok_or(format!("a stray end tag at {i}"))?;
            if done.name != name {
                return Err(format!("</{name}> closes <{}> at {i}", done.name));
            }
            stack
                .last_mut()
                .ok_or(format!("a stray end tag at {i}"))?
                .children
                .push(done);
            i += 2 + end + 1;
        } else {
            let end = rest
                .find('>')
                .ok_or(format!("an unterminated tag at {i}"))?;
            let inner = &rest[1..end];
            let (inner, empty) = match inner.strip_suffix('/') {
                Some(s) => (s, true),
                None => (inner, false),
            };
            let element = parse_tag(inner).map_err(|e| format!("{e} at {i}"))?;
            if empty {
                stack
                    .last_mut()
                    .ok_or(format!("no parent at {i}"))?
                    .children
                    .push(element);
            } else {
                stack.push(element);
            }
            i += end + 1;
        }
    }
    let mut doc = stack.pop().ok_or("an empty document")?;
    if !stack.is_empty() {
        return Err("an unclosed element".to_owned());
    }
    match doc.children.len() {
        1 => Ok(doc.children.remove(0)),
        n => Err(format!("{n} root elements")),
    }
}

fn parse_tag(inner: &str) -> Result<Element, String> {
    let inner = inner.trim();
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let mut element = Element {
        name: inner[..name_end].to_owned(),
        ..Element::default()
    };
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest.find('=').ok_or("an attribute without a value")?;
        let key = rest[..eq].trim().to_owned();
        let after = rest[eq + 1..].trim_start();
        let quote = after.chars().next().ok_or("an attribute without a value")?;
        if quote != '"' && quote != '\'' {
            return Err("an unquoted attribute".to_owned());
        }
        let close = after[1..].find(quote).ok_or("an unterminated attribute")?;
        element.attrs.push((key, unescape(&after[1..1 + close])?));
        rest = after[close + 2..].trim_start();
    }
    Ok(element)
}

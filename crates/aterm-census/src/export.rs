// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Export file [`Selection`] and text [`rewrite`] transforms for publication content guards,
//! and the [`plaintext`] the engine reads of a compressed or BOM'd file.

use std::borrow::Cow;

use aterm_regex::Regex;

pub mod mainline;
pub mod plaintext;

/// The export's allowlist, relative to the checkout.
pub const MANIFEST: &str = "publish/manifest.txt";

/// The top-level paths the engine withholds from every export whatever a manifest says.
pub const CENTRAL_DENY: &[&str] = &["docs", "publish", ".github"];

/// The export's file selection.
#[derive(Debug)]
pub struct Selection {
    include: Vec<Spec>,
    exclude: Vec<Spec>,
}

#[derive(Debug)]
enum Spec {
    Root,
    /// A path, and whether a trailing `/` holds it to what is under it.
    Literal(String, bool),
    Glob(Regex),
}

impl Spec {
    /// `text` as the engine hands it to git, which normalizes it: an empty or
    /// `.` component drops, `..` climbs one, and a spec ending in `/` (or a `.`
    /// or `..` component) names only what is under it, so `README.md/` selects
    /// no file and `crates//x` the files under `crates/x`.
    fn new(text: &str) -> Result<Self, String> {
        if text.starts_with('/') {
            return Err(format!(
                "{text:?} is an absolute pathspec; spell `.` for the checkout root"
            ));
        }
        if text.contains(['[', '\\']) || text.starts_with(':') {
            return Err(format!(
                "{text:?} is a pathspec this guard does not read (a class, an escape or \
                 git magic), so it cannot say what the export selects"
            ));
        }
        let parts: Vec<&str> = text.split('/').collect();
        let mut kept: Vec<&str> = Vec::new();
        for part in &parts {
            match *part {
                "" | "." => {}
                ".." => {
                    kept.pop().ok_or_else(|| {
                        format!("{text:?} climbs out of the checkout, which git refuses")
                    })?;
                }
                part => kept.push(part),
            }
        }
        if kept.is_empty() {
            return Ok(Self::Root);
        }
        let dir = matches!(parts.last(), Some(&("" | "." | "..")));
        let path = kept.join("/");
        if !path.contains(['*', '?']) {
            return Ok(Self::Literal(path, dir));
        }
        let mut pattern = String::from("(?s)^");
        for c in path.chars().chain(dir.then_some('/')) {
            match c {
                '*' => pattern.push_str(".*"),
                '?' => pattern.push('.'),
                c => pattern.push_str(&aterm_regex::escape(c.encode_utf8(&mut [0; 4]))),
            }
        }
        pattern.push('$');
        Regex::new(&pattern)
            .map(Self::Glob)
            .map_err(|e| format!("{text:?}: {e}"))
    }

    fn matches(&self, rel: &str) -> bool {
        match self {
            Self::Root => true,
            Self::Literal(spec, dir) => rel
                .strip_prefix(spec.as_str())
                .is_some_and(|rest| (rest.is_empty() && !dir) || rest.starts_with('/')),
            Self::Glob(re) => re.is_match(rel),
        }
    }
}

impl Selection {
    /// Parses the manifest's lines (`#` to the end of a line is a comment). `Err`
    /// names the 1-based line of a pathspec this reading cannot follow or the
    /// engine refuses.
    ///
    /// # Errors
    /// A line holding an absolute path, class, escape or git magic, a `..` that
    /// climbs out of the checkout, or nothing once its `./` is stripped.
    pub fn parse(manifest: &str) -> Result<Self, (usize, String)> {
        let mut include = Vec::new();
        let mut exclude: Vec<Spec> = CENTRAL_DENY
            .iter()
            .map(|d| Spec::Literal((*d).to_string(), false))
            .collect();
        for (i, raw) in manifest.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (excluding, text) = match line.strip_prefix('!') {
                Some(body) => (true, body.trim()),
                None => (false, line),
            };
            let spec = text.strip_prefix("./").unwrap_or(text);
            if spec.is_empty() {
                return Err((
                    i + 1,
                    format!(
                        "{line:?} names nothing once the engine strips its `./`, so the engine \
                         refuses it (a bare `!`, or an empty pathspec git refuses)"
                    ),
                ));
            }
            let spec = Spec::new(spec).map_err(|why| (i + 1, why))?;
            if excluding {
                exclude.push(spec);
            } else {
                include.push(spec);
            }
        }
        Ok(Self { include, exclude })
    }

    /// Every path outside the central denies.
    #[must_use]
    pub fn everything() -> Self {
        Self::parse("*").unwrap_or_else(|_| unreachable!("`*` is a pathspec this reads"))
    }

    /// Whether the export carries the checkout's file at `rel` (`/`-separated).
    #[must_use]
    pub fn selects(&self, rel: &str) -> bool {
        self.include.iter().any(|s| s.matches(rel)) && !self.exclude.iter().any(|s| s.matches(rel))
    }
}

/// Where the export writes astream's files: `vendor/astream` is a gitlink the manifest
/// withholds, and the transforms write, in its place, files from the commit it pins.
pub const ASTREAM: &str = "vendor/astream";

/// Whether an astream path (relative to astream's root) is in a crate's `tests/`,
/// `examples/` or `benches/`: targets a dependency's build never reads, which stay
/// private, so the export's `vendor/astream` leaves them out.
#[must_use]
pub fn in_astream_target_dir(path: &str) -> bool {
    let Some((_, in_crate)) = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    ["tests/", "examples/", "benches/"]
        .iter()
        .any(|d| in_crate.starts_with(d))
}

/// Whether the export's `vendor/astream` can carry astream's file at `path` (relative to
/// astream's root): its root `Cargo.toml`, or a file of a crate's tree outside
/// [`in_astream_target_dir`]. The transforms write only the crates the export's manifests
/// reach, so this is what they carry of a crate they write.
#[must_use]
pub fn astream_carries(path: &str) -> bool {
    let in_a_crate = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(name, file)| !name.is_empty() && !file.is_empty());
    path == "Cargo.toml" || (in_a_crate && !in_astream_target_dir(path))
}

/// The developer account the export renames.
#[cfg(test)]
const ACCOUNT: &str = concat!("aya", "tes");
const HOME: &[u8] = concat!("/Users/", "aya", "tes").as_bytes();
const AT: &[u8] = concat!("aya", "tes", "@").as_bytes();

/// The private org, which becomes the public one.
pub(crate) const PRIVATE_ORG: &str = concat!("andrew", "yatesai");

/// The toolchain checkouts a home path names.
const TILDE_TRUST: &str = concat!("~", "/trust");
const TILDE_CLEAN: &str = concat!("~", "/clean");

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = 0;
    while let Some(rel) = haystack.get(i..)?.iter().position(|&b| b == first) {
        let pos = i + rel;
        if haystack[pos..].starts_with(needle) {
            return Some(pos);
        }
        i = pos + 1;
    }
    None
}

/// Replace every `from` in `text` with `to`, borrowing `text` when `from` is absent.
fn replace_all<'a>(text: Cow<'a, [u8]>, from: &[u8], to: &[u8]) -> Cow<'a, [u8]> {
    let Some(first) = find_subslice(&text, from) else {
        return text;
    };
    let mut out = Vec::with_capacity(text.len());
    out.extend_from_slice(&text[..first]);
    out.extend_from_slice(to);
    let mut i = first + from.len();
    while let Some(rel) = find_subslice(&text[i..], from) {
        let pos = i + rel;
        out.extend_from_slice(&text[i..pos]);
        out.extend_from_slice(to);
        i = pos + from.len();
    }
    out.extend_from_slice(&text[i..]);
    Cow::Owned(out)
}

fn double_users_slash(text: Cow<'_, [u8]>) -> Cow<'_, [u8]> {
    let mut search = 0;
    let first = loop {
        let Some(rel) = find_subslice(&text[search..], b"/Users/") else {
            return text;
        };
        let pos = search + rel;
        if text.get(pos + 7).is_some_and(u8::is_ascii_alphabetic) {
            break pos;
        }
        search = pos + 1;
    };
    let mut doubled = Vec::with_capacity(text.len() + 8);
    doubled.extend_from_slice(&text[..first]);
    doubled.extend_from_slice(b"/Users//");
    let mut i = first + 7;
    let mut search = i;
    while let Some(rel) = find_subslice(&text[search..], b"/Users/") {
        let pos = search + rel;
        if text.get(pos + 7).is_some_and(u8::is_ascii_alphabetic) {
            doubled.extend_from_slice(&text[i..pos]);
            doubled.extend_from_slice(b"/Users//");
            i = pos + 7;
            search = i;
        } else {
            search = pos + 1;
        }
    }
    doubled.extend_from_slice(&text[i..]);
    Cow::Owned(doubled)
}

/// The export's text rewrites, borrowing `text` when nothing changes.
#[must_use]
pub fn rewrite_cow(text: &[u8]) -> Cow<'_, [u8]> {
    let mut t = replace_all(Cow::Borrowed(text), PRIVATE_ORG.as_bytes(), b"alabsystems");
    t = replace_all(t, HOME, b"/Users//example");
    t = double_users_slash(t);
    t = replace_all(t, TILDE_TRUST.as_bytes(), b"$HOME/trust");
    t = replace_all(t, TILDE_CLEAN.as_bytes(), b"$HOME/clean");
    replace_all(t, AT, b"user@")
}

/// The export's text rewrites.
#[must_use]
pub fn rewrite(text: &[u8]) -> Vec<u8> {
    rewrite_cow(text).into_owned()
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;

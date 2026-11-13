// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The files a deploy uploads: `public/`, less what `hosting.ignore` names, each as the
//! gzip member [`aterm_png::gzip`] writes and the SHA-256 of that member.

use std::collections::BTreeMap;
use std::path::Path;

/// Characters an ignore pattern may not hold: minimatch reads each as syntax (a class, a
/// brace set, an extglob, a negation, an escape) that this matcher does not implement, so a
/// pattern with one is refused by name rather than matched otherwise than firebase-tools would.
const UNSUPPORTED: [char; 8] = ['[', ']', '{', '}', '(', ')', '!', '\\'];

/// One file of the site as the version serves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its path on the site: `/` and its path under `public/`.
    pub path: String,
    /// Its length before compression.
    pub bytes: u64,
    /// The gzip member uploaded for it.
    pub gz: Vec<u8>,
    /// The lowercase hex SHA-256 of [`File::gz`], which `populateFiles` names it by.
    pub hash: String,
}

impl File {
    /// `data`, served at `path`.
    #[must_use]
    pub fn new(path: String, data: &[u8]) -> File {
        let gz = aterm_png::gzip(data);
        let hash = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&gz));
        File {
            path,
            bytes: data.len() as u64,
            gz,
            hash,
        }
    }
}

/// A pattern of `hosting.ignore`, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern(Vec<String>);

impl Pattern {
    /// `text` as a pattern relative to `public/`: segments of literal characters, `*`
    /// (any run within one segment) and `?` (one character), or a whole `**` segment (any
    /// number of segments). Every pattern matches dot files, as firebase-tools' ignore does.
    /// # Errors
    /// An empty or absolute pattern, or one holding a character of [`UNSUPPORTED`].
    pub fn new(text: &str) -> Result<Pattern, String> {
        if text.is_empty() {
            return Err("hosting.ignore holds an empty pattern".into());
        }
        if text.starts_with('/') {
            return Err(format!(
                "hosting.ignore pattern {text:?} is absolute: patterns are relative to public/"
            ));
        }
        if let Some(c) = text.chars().find(|c| UNSUPPORTED.contains(c)) {
            return Err(format!(
                "hosting.ignore pattern {text:?} uses {c:?}, which ship site does not match \
                 (it reads *, ? and **)"
            ));
        }
        let trimmed = text.trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("hosting.ignore holds an empty pattern".into());
        }
        let parts: Vec<String> = trimmed.split('/').map(str::to_string).collect();
        if parts
            .iter()
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return Err(format!(
                "hosting.ignore pattern {text:?} contains an empty or dot path segment"
            ));
        }
        Ok(Pattern(parts))
    }

    /// Whether the pattern matches `path`, a `/`-joined path under `public/`.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        segments(&self.0, Some(path))
    }

    /// Whether the pattern ignores everything under the directory `dir`: it is `X/**` and
    /// `X` matches `dir` (glob's rule for an ignore pattern that ends in `/**`).
    #[must_use]
    pub fn prunes(&self, dir: &str) -> bool {
        match self.0.split_last() {
            Some((last, head)) if last == "**" && !head.is_empty() => segments(head, Some(dir)),
            _ => false,
        }
    }
}

fn split_first_seg(path: Option<&str>) -> Option<(&str, Option<&str>)> {
    let rem = path?;
    match rem.split_once('/') {
        Some((seg, rest)) => Some((seg, Some(rest))),
        None => Some((rem, None)),
    }
}

/// Whether `pattern`'s segments match `path`'s `/`-separated segments.
fn segments(pattern: &[String], path: Option<&str>) -> bool {
    match pattern.split_first() {
        None => path.is_none(),
        Some((first, rest)) if first == "**" => {
            let mut cur = path;
            loop {
                if segments(rest, cur) {
                    return true;
                }
                let Some((_, next)) = split_first_seg(cur) else {
                    return false;
                };
                cur = next;
            }
        }
        Some((first, rest)) => split_first_seg(path).is_some_and(|(seg, more)| {
            segment(first.as_bytes(), seg.as_bytes()) && segments(rest, more)
        }),
    }
}

/// Whether one pattern segment (`*` and `?` its only wildcards) matches one path segment.
fn segment(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| segment(rest, &name[skip..])),
        Some((b'?', rest)) => {
            // `?` is one character: one UTF-8 scalar, never half of one.
            let width = std::str::from_utf8(name)
                .ok()
                .and_then(|s| s.chars().next())
                .map_or(0, char::len_utf8);
            width > 0 && segment(rest, &name[width..])
        }
        Some((c, rest)) => name.first() == Some(c) && segment(rest, &name[1..]),
    }
}

/// `patterns`, each checked.
/// # Errors
/// The first [`Pattern::new`] refuses.
pub fn patterns(patterns: &[String]) -> Result<Vec<Pattern>, String> {
    patterns.iter().map(|p| Pattern::new(p)).collect()
}

/// Every file under `public` that `ignore` leaves, by its path under `public/`, sorted, with
/// its bytes: `overlay`'s for a path it names (a dry run's planned content, which the
/// listing also gains where the file does not exist yet), else the file's own.
/// # Errors
/// A directory or file that cannot be read; a symbolic link, or anything else that is not a
/// regular file or directory (a deploy never follows a link out of `public/`); a name that is
/// not UTF-8; a credential ([`super::key::holds_a_private_key`]), which the site would serve
/// to anyone.
pub fn list(
    public: &Path,
    ignore: &[Pattern],
    overlay: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let root_meta = std::fs::symlink_metadata(public)
        .map_err(|e| format!("cannot list {}: {e}", public.display()))?;
    if !root_meta.file_type().is_dir() {
        return Err(format!(
            "{} is not a plain directory (a symbolic link or file): ship site deploys the files \
             under public/ themselves",
            public.display()
        ));
    }
    let ignored = |rel: &str| ignore.iter().any(|p| p.matches(rel));
    let mut found = BTreeMap::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let at = if dir.is_empty() {
            public.to_path_buf()
        } else {
            public.join(&dir)
        };
        let entries =
            std::fs::read_dir(&at).map_err(|e| format!("cannot list {}: {e}", at.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", at.display()))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                format!(
                    "{} has a name that is not UTF-8: a site path is text",
                    entry.path().display()
                )
            })?;
            let rel = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot read {}: {e}", entry.path().display()))?;
            if kind.is_dir() {
                if !ignore.iter().any(|p| p.prunes(&rel)) {
                    dirs.push(rel);
                }
                continue;
            }
            if ignored(&rel) {
                continue;
            }
            let path = at.join(name);
            if kind.is_file() {
                let bytes = match overlay.get(&rel) {
                    Some(bytes) => {
                        if super::key::holds_a_private_key(bytes) {
                            return Err(format!(
                                "{} is a private key (a JSON key with a private_key, such as the \
                                 deployer's service-account key): the site would serve it to anyone — \
                                 move it out of public/ or name it in firebase.json's hosting.ignore",
                                path.display()
                            ));
                        }
                        bytes.clone()
                    }
                    None => {
                        let bytes = crate::durable::read_regular_bytes(&path, 64 * 1024 * 1024)
                            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                        if super::key::holds_a_private_key(&bytes) {
                            return Err(format!(
                                "{} is a private key (a JSON key with a private_key, such as the \
                                 deployer's service-account key): the site would serve it to anyone — \
                                 move it out of public/ or name it in firebase.json's hosting.ignore",
                                path.display()
                            ));
                        }
                        bytes
                    }
                };
                found.insert(rel, bytes);
            } else {
                return Err(format!(
                    "{} is not a regular file (a symbolic link or a special file): ship site \
                     deploys the files under public/ themselves",
                    path.display()
                ));
            }
        }
    }
    for (rel, bytes) in overlay {
        if !found.contains_key(rel) && !ignored(rel) {
            if super::key::holds_a_private_key(bytes) {
                return Err(format!(
                    "{} is a private key (a JSON key with a private_key, such as the \
                     deployer's service-account key): the site would serve it to anyone — \
                     move it out of public/ or name it in firebase.json's hosting.ignore",
                    public.join(rel).display()
                ));
            }
            found.insert(rel.clone(), bytes.clone());
        }
    }
    Ok(found)
}

/// `listed` as the version's files.
#[must_use]
pub fn files(listed: BTreeMap<String, Vec<u8>>) -> Vec<File> {
    listed
        .into_iter()
        .map(|(rel, bytes)| File::new(format!("/{rel}"), &bytes))
        .collect()
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

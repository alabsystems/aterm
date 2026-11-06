// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The engine's one clearance of a forbidden-content hit (`_adjudicate_forbidden`): a
//! line-numbered hit in a text file, on a line of a base64 literal that decodes to the bytes
//! of a binary file the same export carries (`_verified_payload_line_spans`). The literal is
//! then a copy of bytes the export publishes and the binary path already judges. Filename,
//! binary, decoded-stream and BOM hits are never cleared.
//!
//! This clears no more than the engine does. A literal clears only when Python's
//! `b64decode(validate=True)` accepts it: the alphabet, with whitespace removed, and a length
//! that is a multiple of four. Its bytes must equal an exported binary's, compared byte for
//! byte where the engine compares SHA-256 digests.
//!
//! It also clears what the engine clears beyond the literal, since a cut passes that too. A
//! literal's lines are the whole lines its quotes open and close on, and they are counted as
//! Python's text read counts them while a hit keeps grep's number: each lone CR before a
//! literal moves its lines one past the ones grep numbers, so the line after it clears.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use aterm_regex::Regex;

use super::org;
use crate::export::Selection;

/// The engine's `BASE64_LITERAL`: 512 or more digits and whitespace between quotes. Python's
/// `\s` (`str.isspace`) is Unicode `White_Space` and the four separators U+001C to U+001F; `\s`
/// here is the first, so the second is spelled out.
const LITERAL: &str = "['\"`]([A-Za-z0-9+/\\s\\x1c-\\x1f]{512,}={0,2})['\"`]";

/// How much of an exported file the engine reads for a NUL before it calls the file binary
/// (`_tree_binary_digests`).
const BINARY_HEAD: usize = 8192;

fn literal() -> &'static Regex {
    static LITERAL_RE: OnceLock<Regex> = OnceLock::new();
    LITERAL_RE.get_or_init(|| {
        Regex::new(LITERAL).unwrap_or_else(|e| unreachable!("the literal pattern compiles: {e}"))
    })
}

/// The binary files of the export: each one's exported length and FNV-1a, and the paths with
/// those bytes.
pub(super) struct Binaries {
    by_key: HashMap<(usize, u64), Vec<String>>,
}

impl Binaries {
    /// Every file of `export` (paths relative to `root`) the export carries whose exported
    /// head holds a NUL. A file that is gone, a link or unreadable is not one.
    pub(super) fn read(
        root: &Path,
        export: &[String],
        selection: &Selection,
        org: &org::Rewrite,
    ) -> Self {
        let mut by_key: HashMap<(usize, u64), Vec<String>> = HashMap::new();
        for rel in export.iter().filter(|rel| super::selects(selection, rel)) {
            let path = root.join(rel);
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                continue;
            }
            let Ok(bytes) = crate::tree::read_bytes(&path) else {
                continue;
            };
            let exported = super::exported(rel, &bytes, Some(org));
            if exported[..exported.len().min(BINARY_HEAD)].contains(&0) {
                by_key.entry(key(&exported)).or_default().push(rel.clone());
            }
        }
        Self { by_key }
    }

    /// Whether `bytes` are, byte for byte, an exported binary's.
    fn holds(&self, root: &Path, bytes: &[u8], org: &org::Rewrite) -> bool {
        self.by_key.get(&key(bytes)).is_some_and(|rels| {
            rels.iter().any(|rel| {
                crate::tree::read_bytes(&root.join(rel))
                    .is_ok_and(|read| *super::exported(rel, &read, Some(org)) == *bytes)
            })
        })
    }
}

fn key(bytes: &[u8]) -> (usize, u64) {
    (bytes.len(), aterm_hash::fnv1a_64(bytes))
}

/// The 1-based, inclusive line spans of the base64 literals in an exported text file that
/// decode to an exported binary ([`Binaries`]), with the text read as the engine reads it:
/// decoded lossily, `\r\n` and a lone `\r` each one line end, as Python's text mode reads them.
pub(super) fn verified_spans(
    exported: &[u8],
    root: &Path,
    binaries: &Binaries,
    org: &org::Rewrite,
) -> Vec<(usize, usize)> {
    let text = String::from_utf8_lossy(exported)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut spans = Vec::new();
    // Line ends before `counted`, so each literal's line is counted from the one before it.
    let (mut counted, mut ends) = (0, 0);
    for found in literal().find_iter(&text) {
        let quoted = found.as_str();
        // The match opens and closes with a one-byte quote; group 1 is what lies between.
        let inner = &quoted[1..quoted.len() - 1];
        let digits: String = inner
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();
        if digits.len() % 4 != 0 {
            continue;
        }
        let Ok(raw) = aterm_codec::base64::decode(&digits) else {
            continue;
        };
        if !binaries.holds(root, &raw, org) {
            continue;
        }
        ends += text[counted..found.start()].matches('\n').count();
        counted = found.start();
        let first = ends + 1;
        spans.push((first, first + quoted.matches('\n').count()));
    }
    spans
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The export scan: what the publication engine's content guards judge in aterm's export, held
//! offline to the files a change touches, before a cut rather than during one
//! (`docs/DESIGN-fast-build-release-2026-11-05.md`, B7).
//!
//! Each file the export carries ([`selects`]) is exported as the engine exports it: the
//! export's transforms ([`rewrite`]), then the engine's org rewrite ([`org`]). Its path and text
//! are then held to the forbidden-content rows ([`forbidden`]), less the hits the engine clears
//! as base64 copies of exported binaries ([`payload`]), and searched for the private org
//! ([`private_refs`]), and the exported bytes are written where gitleaks reads them
//! ([`scan`]'s `into`). gitleaks itself, its config policy and the adjudication of its report
//! are `aterm-verify`'s `export-scan` stage, which runs this scan, because this crate spawns
//! nothing and parses neither TOML nor JSON.
//!
//! The engine's guards stay where they are: `pub stage` still runs them on every cut, over the
//! whole export, as the backstop no repository can disable. This scan only finds their reds
//! earlier, and a finding here names a file and line, never the text.

pub mod rows;

mod forbidden;
mod org;
mod payload;

use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::export::{ASTREAM, MANIFEST, PRIVATE_ORG, Selection, astream_carries, rewrite_cow};
use rows::EXTRA;

/// Which of the engine's guards a finding is the verdict of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Guard {
    /// The export's manifest, which says what the export carries.
    Manifest,
    /// `scan_forbidden`: the export's rows and the engine's baseline.
    Forbidden,
    /// `scan_private_refs`: the private org, after the org rewrite.
    PrivateRefs,
    /// gitleaks, its config policy and the engine's adjudication of its report.
    Gitleaks,
}

impl Guard {
    /// The guard as a finding names it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Forbidden => "forbidden content",
            Self::PrivateRefs => "private reference",
            Self::Gitleaks => "gitleaks",
        }
    }
}

/// One thing the engine's guards would refuse the export for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub guard: Guard,
    /// The exported file, relative to the export's root, `/`-separated.
    pub path: String,
    /// 1-based line; 0 when the finding is about the file as a whole.
    pub line: usize,
    /// What was seen, never the text it matched.
    pub detail: String,
}

/// What the scan reads from the publication engine's checkout.
pub struct Engine {
    /// Its forbidden-content baseline ([`rows::BASELINE`]): where it was read, and its text.
    pub baseline: (String, String),
    /// Its registry's repository names, which its org rewrite follows.
    pub names: Vec<String>,
}

/// What [`scan`] found.
pub struct Scanned {
    /// The files the export carries among those listed, which were judged.
    pub files: usize,
    pub findings: Vec<Finding>,
    /// The forbidden-content hits cleared as the engine clears them: each on a line of a base64
    /// copy of a binary the export carries ([`payload`]).
    pub cleared: usize,
}

/// What the export's transforms delete from it whatever the manifest selects: astream's
/// submodule entry, since the export writes astream's files in its place.
const DELETED: &str = ".gitmodules";

/// Whether the export carries the checkout's `rel` under `selection`: what the manifest
/// selects, less what the transforms delete ([`DELETED`]), and in place of the
/// `vendor/astream` gitlink the manifest withholds, the files astream's pin gives it
/// ([`astream_carries`]).
fn selects(selection: &Selection, rel: &str) -> bool {
    rel != DELETED
        && rel
            .strip_prefix(ASTREAM)
            .and_then(|rest| rest.strip_prefix('/'))
            .map_or_else(|| selection.selects(rel), astream_carries)
}

pub(super) fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = needle.first()?.to_ascii_lowercase();
    let upper = first.to_ascii_uppercase();
    let len = needle.len();
    let mut i = 0;
    while i + len <= haystack.len() {
        let rel = haystack[i..haystack.len() - len + 1]
            .iter()
            .position(|&b| b == first || b == upper)?;
        let pos = i + rel;
        if haystack[pos..pos + len].eq_ignore_ascii_case(needle) {
            return Some(pos);
        }
        i = pos + 1;
    }
    None
}

fn private_ref_needles() -> &'static [(Vec<u8>, &'static str); 5] {
    static NEEDLES: OnceLock<[(Vec<u8>, &'static str); 5]> = OnceLock::new();
    NEEDLES.get_or_init(|| {
        let wide = |unit: usize, little: bool| -> Vec<u8> {
            let mut out = Vec::with_capacity(PRIVATE_ORG.len() * unit);
            for b in PRIVATE_ORG.bytes() {
                let start = out.len();
                out.resize(start + unit, 0);
                out[start + if little { 0 } else { unit - 1 }] = b;
            }
            out
        };
        [
            (PRIVATE_ORG.as_bytes().to_vec(), ""),
            (wide(2, true), " in UTF-16LE"),
            (wide(2, false), " in UTF-16BE"),
            (wide(4, true), " in UTF-32LE"),
            (wide(4, false), " in UTF-32BE"),
        ]
    })
}

/// Every place `data` (the exported file `rel`) names the private org, as the engine's
/// `scan_private_refs` finds it: in the path, any case; in the bytes, ASCII case folded, in
/// UTF-8 and in UTF-16 and UTF-32 of either byte order, each at the line its first byte is on.
fn private_refs(rel: &str, data: &[u8], out: &mut Vec<Finding>) {
    let finding = |line, detail: &str| Finding {
        guard: Guard::PrivateRefs,
        path: rel.to_string(),
        line,
        detail: detail.to_string(),
    };
    if find_ignore_ascii_case(rel.as_bytes(), PRIVATE_ORG.as_bytes()).is_some() {
        out.push(finding(0, "its path names the private org"));
    }
    let needles = private_ref_needles();
    let count = if data.contains(&0) { needles.len() } else { 1 };
    for (needle, how) in &needles[..count] {
        let mut start = 0;
        while let Some(at) = find_ignore_ascii_case(&data[start..], needle) {
            let pos = start + at;
            let line = data[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
            out.push(finding(
                line,
                &format!(
                    "as exported, it names the private org{how}, which the org rewrite leaves \
                     only in a git pin, a lockfile, a file over 5 MB or one that is not UTF-8; \
                     name the public org"
                ),
            ));
            start = pos + needle.len();
        }
    }
}

/// The export's bytes of a file whose checkout bytes are `bytes`, borrowed when the export
/// changes nothing: the transforms leave one that holds a NUL as it is, and the engine's org
/// rewrite follows.
fn exported<'a>(rel: &str, bytes: &'a [u8], org: Option<&org::Rewrite>) -> Cow<'a, [u8]> {
    let transformed = if bytes.contains(&0) {
        Cow::Borrowed(bytes)
    } else {
        rewrite_cow(bytes)
    };
    let Some(org) = org else {
        return transformed;
    };
    let size = transformed.len() as u64;
    org.file(rel, &transformed, size)
        .map_or(transformed, Cow::Owned)
}

/// What [`scan`] reads from the checkout.
pub struct Listing<'a> {
    /// The checkout.
    pub root: &'a Path,
    /// The files to judge, relative to `root` (a missing one or a directory is passed over).
    pub paths: &'a [String],
    /// The checkout's gitlinks among them (all of them when `whole`): the engine refuses an
    /// export whose manifest selects one.
    pub gitlinks: &'a [String],
    /// Whether `paths` is the whole checkout, so a manifest selecting none of it is a finding.
    pub whole: bool,
    /// The files a cut writes byte for byte, read only when a hit may be cleared: among them
    /// the export's binaries, whose base64 copies the engine clears ([`payload`]). A file
    /// listed here that the cut does not write could clear a hit the cut refuses.
    pub export: &'a (dyn Fn() -> Vec<String> + Sync),
}

/// Each of the listed paths the export carries, judged as the engine's content guards judge it
/// once exported, its exported bytes written under `into` when given. Without the engine's
/// checkout (`engine` is `None`) a file is held to the export's own rows alone and no private
/// reference is judged, because the engine's registry decides what its org rewrite leaves.
#[must_use]
pub fn scan(listing: &Listing<'_>, engine: Option<&Engine>, into: Option<&Path>) -> Scanned {
    let Listing {
        root,
        paths: listed,
        gitlinks,
        whole,
        export,
    } = *listing;
    let mut findings = Vec::new();
    let manifest_finding = |line, detail: String| Finding {
        guard: Guard::Manifest,
        path: MANIFEST.to_string(),
        line,
        detail,
    };
    let read = |rel: &str| match crate::tree::read_to_string(&root.join(rel)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Finding {
            guard: if rel == MANIFEST {
                Guard::Manifest
            } else {
                Guard::Forbidden
            },
            path: rel.to_string(),
            line: 0,
            detail: format!("unreadable ({e}), so the scan cannot run"),
        }),
    };
    let (manifest, extra) = match (read(MANIFEST), read(EXTRA)) {
        (Ok(manifest), Ok(extra)) => (manifest, extra.unwrap_or_default()),
        (manifest, extra) => {
            findings.extend(manifest.err().into_iter().chain(extra.err()));
            return Scanned {
                files: 0,
                findings,
                cleared: 0,
            };
        }
    };
    let selection = match manifest
        .as_deref()
        .map_or_else(|| Ok(Selection::everything()), Selection::parse)
    {
        Ok(selection) => selection,
        Err((line, why)) => {
            findings.push(manifest_finding(line, why));
            return Scanned {
                files: 0,
                findings,
                cleared: 0,
            };
        }
    };
    let files: Vec<&str> = listed
        .iter()
        .map(String::as_str)
        .filter(|rel| selects(&selection, rel))
        .collect();
    findings.extend(gitlinks.iter().filter(|g| selection.selects(g)).map(|g| {
        manifest_finding(
            0,
            format!(
                "it selects the gitlink {g}, so the engine refuses the export (a gitlink \
                         may reference a private repository)"
            ),
        )
    }));
    if whole && manifest.is_some() && files.is_empty() {
        findings.push(manifest_finding(
            0,
            "it selects no file of the checkout, so the engine refuses the export".into(),
        ));
        return Scanned {
            files: 0,
            findings,
            cleared: 0,
        };
    }
    let baseline = engine.map(|e| (e.baseline.0.as_str(), e.baseline.1.as_str()));
    let (rules, refused) = forbidden::Rules::new(&extra, EXTRA, baseline);
    findings.extend(refused);
    let held = rules.held();
    let org = engine.map(|e| org::Rewrite::new(PRIVATE_ORG, &e.names));
    let judging = Judging {
        root,
        held: &held,
        org: org.as_ref(),
        into,
        selection: &selection,
        export,
        binaries: OnceLock::new(),
        cleared: AtomicUsize::new(0),
    };
    let judged = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let found: Vec<Vec<Finding>> = std::thread::scope(|s| {
        let running: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut found = Vec::new();
                    while let Some(rel) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if judging.judge(rel, &mut found) {
                            judged.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    found
                })
            })
            .collect();
        running
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    findings.extend(found.into_iter().flatten());
    findings.sort();
    Scanned {
        files: judged.into_inner(),
        findings,
        cleared: judging.cleared.into_inner(),
    }
}

/// What judging a file reads beside it.
struct Judging<'a> {
    root: &'a Path,
    held: &'a forbidden::Held<'a>,
    org: Option<&'a org::Rewrite>,
    into: Option<&'a Path>,
    selection: &'a Selection,
    export: &'a (dyn Fn() -> Vec<String> + Sync),
    /// The export's binaries, read once, when the first hit that may be cleared is found.
    binaries: OnceLock<payload::Binaries>,
    cleared: AtomicUsize,
}

impl Judging<'_> {
    /// One listed file, judged and written under `into`; false when it is no file to export
    /// (gone, or a directory such as a gitlink).
    fn judge(&self, rel: &str, out: &mut Vec<Finding>) -> bool {
        let unreadable = |e: std::io::Error| Finding {
            guard: Guard::Forbidden,
            path: rel.to_string(),
            line: 0,
            detail: format!("unreadable: {e}"),
        };
        let path = self.root.join(rel);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => return false,
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
            Err(e) => {
                out.push(unreadable(e));
                return true;
            }
        };
        forbidden::judge_path(rel, self.held, out);
        if meta.file_type().is_symlink() {
            match std::fs::read_link(&path) {
                Ok(target) => {
                    if self.org.is_some() {
                        private_refs(rel, target.as_os_str().as_encoded_bytes(), out);
                    }
                    if let Some(into) = self.into
                        && let Err(e) = link(&target, &into.join(rel))
                    {
                        out.push(unwritten(rel, &e));
                    }
                }
                Err(e) => out.push(unreadable(e)),
            }
            return true;
        }
        let bytes = match crate::tree::read_bytes(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                out.push(unreadable(e));
                return true;
            }
        };
        let exported = exported(rel, &bytes, self.org);
        let mut eligible = Vec::new();
        forbidden::judge(rel, &exported, self.held, out, &mut eligible);
        self.clear(rel, &exported, &eligible, out);
        if self.org.is_some() {
            private_refs(rel, &exported, out);
        }
        if let Some(into) = self.into
            && let Err(e) = write(
                &path,
                &into.join(rel),
                match &exported {
                    Cow::Borrowed(_) => None,
                    Cow::Owned(bytes) => Some(bytes),
                },
            )
        {
            out.push(unwritten(rel, &e));
        }
        true
    }

    /// Removes from `out` the findings at `eligible` (positions in `out`, ascending) that sit on
    /// a line of a base64 copy of an exported binary, as the engine clears them ([`payload`]).
    /// Nothing is cleared without the engine, whose org rewrite decides an exported binary's
    /// bytes, nor in a file whose path holds a `:` or a line end, which the engine's reading of
    /// a grep record (`^(.*?):([1-9][0-9]*):`) may split elsewhere.
    fn clear(&self, rel: &str, exported: &[u8], eligible: &[usize], out: &mut Vec<Finding>) {
        let Some(org) = self.org else {
            return;
        };
        if eligible.is_empty() || rel.contains([':', '\n', '\r']) {
            return;
        }
        let binaries = self.binaries.get_or_init(|| {
            payload::Binaries::read(self.root, &(self.export)(), self.selection, org)
        });
        let spans = payload::verified_spans(exported, self.root, binaries, org);
        if spans.is_empty() {
            return;
        }
        for &at in eligible.iter().rev() {
            let line = out[at].line;
            if spans
                .iter()
                .any(|&(first, last)| first <= line && line <= last)
            {
                out.remove(at);
                self.cleared.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn unwritten(rel: &str, e: &std::io::Error) -> Finding {
    Finding {
        guard: Guard::Gitleaks,
        path: rel.to_string(),
        line: 0,
        detail: format!("could not write its exported copy for gitleaks: {e}"),
    }
}

/// The symbolic link `to`, pointing where the checkout's link points, as the export carries it:
/// gitleaks reads a link in the tree as the engine's run reads it.
fn link(target: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, to)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, to);
        Ok(())
    }
}

/// The exported copy of `from` at `to`: the bytes the export `changed` it to, else a copy (a
/// clone where the volume makes one).
fn write(from: &Path, to: &Path, changed: Option<&[u8]>) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match changed {
        None => std::fs::copy(from, to).map(drop),
        Some(bytes) => std::fs::write(to, bytes),
    }
}

#[cfg(test)]
#[path = "export_scan/tests.rs"]
mod tests;

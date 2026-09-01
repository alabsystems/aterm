// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `TODO(mirror-config-split)` DISCHARGED — `cargo forge mirror config`.
//!
//! # The split, and why it is a separate file
//!
//! Cargo reads source replacement from ONE place: a `config.toml` on the
//! config search path. In this repository that is `.cargo/config.toml`, and
//! that file cannot ship. VERIFIED against the real file rather than the plan
//! text (2026-09-01): it carries
//! `[target.'cfg(trust_verify)'] rustflags = ["-Ztrust-verify=off", "--cfg",
//! "clean_islands"]`, the same pair as `rustdocflags`, and
//! `[build] rustdoc = "trustdoc"` — three Trust-toolchain-only settings, and
//! `publish/manifest.txt` does not list the file, while
//! `publish/transforms.sh` line 82 replaces `rust-toolchain.toml` with
//! `publish/public-rust-toolchain.toml` (stock `1.97.1`). So the public
//! snapshot is a stock-Rust clone that never sees `.cargo/config.toml`, by
//! construction and on purpose.
//!
//! The consequence is the whole reason this module exists: **the mirror's
//! `[source]` block cannot ride in the file that carries those flags**, so it
//! is generated here as a standalone fragment that ships on its own row.
//!
//! # THE PLAN'S CLAIM, CORRECTED
//!
//! `docs/THIRD_PARTY_SURFACE_PLAN.md` line 744 says that if the mirror config
//! does not ship, "the public build silently compiles *different bytes* under
//! byte-identical lock lines." **That overstates it, and the correction
//! matters because it decides what this gate can honestly promise.** For any
//! entry that has a `checksum` in `Cargo.lock` — and `mirror.rs` refuses a
//! registry entry that does not — cargo verifies the sha256 of the downloaded
//! `.crate` against the lock before unpacking it. A crates.io build and a
//! mirror build therefore compile the SAME third-party bytes or one of them
//! fails loudly. Byte divergence under an identical lock is not the reachable
//! failure.
//!
//! What IS reachable, and what this module actually defends:
//!
//! 1. **A network dependency.** Without the fragment the public build must
//!    reach crates.io. That is availability, not integrity — and it is the
//!    mirror's real purpose.
//! 2. **The yank cliff.** `mirror.rs` deliberately rewrites `"yanked":true` to
//!    `false` so a later upstream yank cannot brick a shipped lock. A public
//!    clone without the mirror does not get that protection; the divergence is
//!    one this repository CHOSE, and it lives in the mirror, not upstream.
//! 3. **Different FIRST-PARTY bytes.** `--cfg clean_islands` is absent from the
//!    public build, so aterm's own `clean { … }` islands compile out. That is
//!    real, it is about `.cargo/config.toml`, and mirroring does not touch it —
//!    which is exactly why the flags and the `[source]` block must not be
//!    conflated into one shipped file.
//! 4. **A fragment that lies.** A shipped `[source]` block naming a package set
//!    that is not the lock's. That one IS silent, and it is what `[OB-16]`
//!    makes impossible.
//!
//! # The anchor
//!
//! The fragment carries `registry-lock-sha256`: sha256 over the canonical
//! rendering of `Cargo.lock`'s registry slice (`name\tversion\tchecksum` per
//! line, sorted). Deliberately NOT the digest of the whole lock — workspace
//! members are path packages with no source, so a release version bump or any
//! first-party churn leaves this number untouched, and it moves exactly when
//! WHAT IS MIRRORED moves.
//!
//! # This module flips NO default
//!
//! It writes `tools/cargo-mirror-config.toml` and nothing else. Cargo does not
//! read that path, so the fragment is inert until an operator installs it —
//! and installing it is the delivery step, which needs bytes this repository
//! does not carry. MEASURED (2026-09-01) that cargo accepts the fragment: with
//! it as `$CARGO_HOME/config.toml` over an emitted mirror,
//! `cargo metadata --locked --offline` exits 0 with zero warnings, leaves
//! `Cargo.lock` byte-identical, and unpacks all 495 packages from the local
//! registry. The `[aterm-mirror]` anchor table is inert to cargo (its config
//! reader resolves known paths on demand and never strict-deserializes the
//! tree) — measured in the same run.

use crate::Outcome;
use crate::mirror::{self, RegistryPkg};
use crate::mirror_bundle::registry_slice_digest;
use aterm_toml::edit::{DocumentMut, Item};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// The shippable fragment. NOT a path cargo reads — that is the point.
pub const FRAGMENT_FILE: &str = "tools/cargo-mirror-config.toml";
/// The repository file that DOES flip the default, checked so a flip cannot
/// land without the bytes behind it.
pub const CARGO_CONFIG_FILE: &str = ".cargo/config.toml";
/// The publish allowlist the fragment must appear in.
pub const PUBLISH_MANIFEST: &str = "publish/manifest.txt";
/// Where the delivered mirror is expected to land, relative to the repository
/// root. MEASURED: a relative `local-registry` in a config file resolves
/// against the directory ABOVE the one holding it — `<repo>/.cargo/config.toml`
/// resolves `"mirror"` to `<repo>/mirror`, and `$CARGO_HOME/config.toml`
/// resolves it against `$CARGO_HOME`'s parent (both probed 2026-09-01).
pub const MIRROR_DIR: &str = "mirror";
/// The source name the fragment defines. Anything else and cargo's
/// `replace-with` would dangle.
pub const SOURCE_NAME: &str = "aterm-mirror";

/// What a fragment on disk claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub format: i64,
    pub registry_packages: i64,
    pub registry_lock_sha256: String,
    pub local_registry: String,
    /// `[source.crates-io] replace-with`.
    pub replace_with: String,
    /// `[source.<replace_with>] local-registry`.
    pub source_local_registry: String,
}

/// Render the fragment for the lock at `root`. Deterministic: same lock, same
/// bytes.
pub fn render(pkgs: &[RegistryPkg]) -> String {
    let digest = registry_slice_digest(pkgs);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\
# GENERATED by `cargo forge mirror config --write`. Do not hand-edit — the
# anchor below is compared against Cargo.lock by `[OB-16]` in
# `cargo forge check`, and an edit that drifts from the lock fails the gate.
#
# WHAT THIS IS. The `[source]` half of aterm's Lane 1 local-registry mirror,
# split OUT of `.cargo/config.toml` (docs/THIRD_PARTY_SURFACE_PLAN.md, Lane 1,
# `TODO(mirror-config-split)`). That file carries `-Ztrust-verify=off`,
# `--cfg clean_islands` and `rustdoc = \"trustdoc\"` — Trust-toolchain-only
# settings a stock-Rust clone cannot survive — and `publish/manifest.txt`
# deliberately does not export it. So the source replacement had to become a
# file of its own, and this is that file.
#
# IT IS INERT WHERE IT SITS. Cargo reads source replacement only from a
# `config.toml` on its config search path; `tools/cargo-mirror-config.toml` is
# not one. Nothing about a normal build or a public clone changes because this
# file exists.
#
# TO USE IT (operator step, after the mirror has been delivered and verified):
#     cargo forge mirror unbundle --file <bundle> --out mirror
#     cargo forge mirror verify --dir mirror
#     cp tools/cargo-mirror-config.toml .cargo/config.toml   # public clone
#     # in THIS repository, append the two [source] tables to .cargo/config.toml
#     # instead — its rustflags tables must not be lost.
# The relative `local-registry` below resolves against the directory ABOVE the
# one holding the config file, so `.cargo/config.toml` + `\"{MIRROR_DIR}\"` means
# `<repo>/{MIRROR_DIR}` (measured, cargo 1.99.0-dev).
#
# WHAT IT DOES NOT DO. It does not make the build offline on its own (the
# mirror directory must be present), it is not signed, and it says nothing
# about the compiler flags above — a public clone still compiles first-party
# code WITHOUT `--cfg clean_islands`, mirror or no mirror."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "\
# The anchor. `registry-lock-sha256` is sha256 over the canonical rendering of
# Cargo.lock's registry slice: one `name<TAB>version<TAB>checksum` line per
# registry-sourced package, sorted, newline-terminated. Reproduce it with
# `cargo forge mirror config` and diff. Cargo ignores this table."
    );
    let _ = writeln!(s, "[aterm-mirror]");
    let _ = writeln!(s, "format = 1");
    let _ = writeln!(s, "registry-packages = {}", pkgs.len());
    let _ = writeln!(s, "registry-lock-sha256 = \"{digest}\"");
    let _ = writeln!(s, "local-registry = \"{MIRROR_DIR}\"");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "\
# Source replacement is ALL-OR-NOTHING for crates.io, which is why the mirror
# carries every registry-sourced lock entry and why `[OB-16]` fails on a
# partial one."
    );
    let _ = writeln!(s, "[source.crates-io]");
    let _ = writeln!(s, "replace-with = \"{SOURCE_NAME}\"");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "\
# `local-registry`, not `directory`: cargo re-hashes every `.crate` in a local
# registry against the index cksum on every build, and never re-hashes a
# `directory` source at all."
    );
    let _ = writeln!(s, "[source.{SOURCE_NAME}]");
    let _ = writeln!(s, "local-registry = \"{MIRROR_DIR}\"");
    s
}

/// Read the anchor out of a fragment's text.
pub fn parse(text: &str, at: &Path) -> Result<Anchor, String> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", at.display()))?;
    let table = |name: &str| -> Result<&Item, String> {
        doc.get(name)
            .ok_or_else(|| format!("{}: no `[{name}]` table", at.display()))
    };
    let anchor = table("aterm-mirror")?;
    let string = |item: &Item, table_name: &str, key: &str| -> Result<String, String> {
        item.get(key)
            .and_then(Item::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "{}: `[{table_name}] {key}` is missing or not a string",
                    at.display()
                )
            })
    };
    let integer = |item: &Item, table_name: &str, key: &str| -> Result<i64, String> {
        item.get(key).and_then(Item::as_integer).ok_or_else(|| {
            format!(
                "{}: `[{table_name}] {key}` is missing or not an integer",
                at.display()
            )
        })
    };
    let format = integer(anchor, "aterm-mirror", "format")?;
    let registry_packages = integer(anchor, "aterm-mirror", "registry-packages")?;
    let registry_lock_sha256 = string(anchor, "aterm-mirror", "registry-lock-sha256")?;
    mirror::validate_checksum(&registry_lock_sha256)
        .map_err(|why| format!("{}: `registry-lock-sha256` {why}", at.display()))?;
    let local_registry = string(anchor, "aterm-mirror", "local-registry")?;

    let sources = table("source")?;
    let crates_io = sources
        .get("crates-io")
        .ok_or_else(|| format!("{}: no `[source.crates-io]` table", at.display()))?;
    let replace_with = string(crates_io, "source.crates-io", "replace-with")?;
    let replacement = sources.get(&replace_with).ok_or_else(|| {
        format!(
            "{}: `[source.crates-io] replace-with = \"{replace_with}\"` names a source with no \
             `[source.{replace_with}]` table",
            at.display()
        )
    })?;
    let source_local_registry = string(
        replacement,
        &format!("source.{replace_with}"),
        "local-registry",
    )?;
    Ok(Anchor {
        format,
        registry_packages,
        registry_lock_sha256,
        local_registry,
        replace_with,
        source_local_registry,
    })
}

/// `cargo forge mirror config [--write]`.
pub fn run_config(root: &Path, write: bool) -> Result<Outcome, String> {
    let pkgs = mirror::locked_registry_packages(root)?;
    let text = render(&pkgs);
    let mut log = String::new();
    if write {
        let path = root.join(FRAGMENT_FILE);
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        mirror::atomic_replace(&path, |file| {
            use std::io::Write as _;
            file.write_all(text.as_bytes())
                .map_err(|e| format!("cannot write {}: {e}", path.display()))
        })?;
        let _ = writeln!(log, "mirror config — wrote {}", path.display());
        let _ = writeln!(
            log,
            "  {} registry package(s); registry-lock-sha256 {}",
            pkgs.len(),
            registry_slice_digest(&pkgs)
        );
        let _ = writeln!(
            log,
            "  NO DEFAULT CHANGED — cargo does not read this path. Installing it is the \
             delivery step."
        );
        let _ = writeln!(log, "  PASS");
    } else {
        log.push_str(&text);
    }
    Ok(Outcome { ok: true, log })
}

// ---------------------------------------------------------------------------
// the gate half
// ---------------------------------------------------------------------------

/// One line the gate prints, with a verdict attached.
pub enum Finding {
    Fail(String),
    Note(String),
}

/// Directories the manifest's own header says are dropped whatever the rows
/// say: "Central policy excludes publish/, docs/, and .github/ regardless of
/// this allowlist." Modelled so that MOVING the fragment under one of them
/// would be caught, rather than trusted to never happen.
const CENTRALLY_EXCLUDED: &[&str] = &["publish", "docs", ".github"];

/// What `publish/manifest.txt` says about one repository path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Export {
    /// An inclusion row covers it and no `!` row takes it back.
    Yes,
    /// No row covers it at all.
    NoRow,
    /// An inclusion covered it and this `!` row excluded it again.
    Excluded(String),
    /// Central policy drops the whole directory, whatever the rows say.
    Centrally(&'static str),
}

/// Does one allowlist row cover `path`?
///
/// A row is a repository path and covers itself and everything beneath it —
/// `crates` is how the whole crate tree ships. A `*` SEGMENT matches exactly
/// one path segment, which is the only glob shape the file uses
/// (`!vendor/*/.github`). Any other use of `*` is REFUSED rather than guessed:
/// a reader that quietly mis-implements the engine's glob rules would judge
/// export coverage on a fiction.
fn manifest_row_covers(row: &str, path: &str) -> Result<bool, String> {
    let mut segments = path.split('/');
    for part in row.split('/') {
        if part.contains('*') && part != "*" {
            return Err(format!(
                "{PUBLISH_MANIFEST} row `{row}` uses `*` inside a path component. This reader \
                 implements whole-segment `*` only — the shape every row in the file uses — and \
                 will not guess the rest of the publish engine's glob syntax."
            ));
        }
        let Some(here) = segments.next() else {
            return Ok(false);
        };
        if part != "*" && part != here {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Read `publish/manifest.txt` the way the publish engine does, NEGATIONS
/// INCLUDED. `Ok(None)` means the file is not in this tree.
///
/// **SEMANTICS INFERRED, NOT EXECUTED.** The engine itself lives in the
/// sibling `publication` repository, which is absent here, so nothing in this
/// function was run against it. The rules below are derived from the file's
/// own 11 negation rows and the comments above them: every `!` row names a
/// path INSIDE a subtree an inclusion row already carries
/// (`!crates/aterm-spec-models/proofs` under `crates`, `!vendor/*/.github`
/// under `vendor`), and each comment says that path must not ship. A `!` row
/// is therefore the allowlist's EXCLUSION mechanism and an exclusion beats the
/// inclusion that would otherwise carry the path, whatever their order — the
/// reading under which the file's own comments are true, and the conservative
/// one, since a reader that must guess should guess toward "does not ship". The previous reader ignored `!` rows as
/// decoration, and one appended `!tools/cargo-mirror-config.toml` left this
/// gate GREEN while the fragment stopped shipping.
fn publish_manifest_export(root: &Path, wanted: &str) -> Result<Option<Export>, String> {
    let path = root.join(PUBLISH_MANIFEST);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if let Some(top) = wanted.split('/').next()
        && let Some(excluded) = CENTRALLY_EXCLUDED.iter().find(|d| **d == top)
    {
        return Ok(Some(Export::Centrally(excluded)));
    }
    let mut included = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.strip_prefix('!') {
            Some(row) => {
                if manifest_row_covers(row.trim(), wanted)? {
                    return Ok(Some(Export::Excluded(line.to_string())));
                }
            }
            None => included |= manifest_row_covers(line, wanted)?,
        }
    }
    Ok(Some(if included { Export::Yes } else { Export::NoRow }))
}

/// Whether the repository's own cargo config flips the default source, and to
/// what. `Ok(None)` means it does not.
fn repo_default_replacement(root: &Path) -> Result<Option<(String, Option<String>)>, String> {
    let path = root.join(CARGO_CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    let Some(sources) = doc.get("source") else {
        return Ok(None);
    };
    let Some(replace_with) = sources
        .get("crates-io")
        .and_then(|t| t.get("replace-with"))
        .and_then(Item::as_str)
    else {
        return Ok(None);
    };
    let local = sources
        .get(replace_with)
        .and_then(|t| t.get("local-registry"))
        .and_then(Item::as_str)
        .map(str::to_string);
    Ok(Some((replace_with.to_string(), local)))
}

/// `[OB-16]`'s evidence. Never runs cargo, never touches the network, and
/// never writes.
pub fn audit(root: &Path, row_anchor: &mirror::RowAnchor) -> Vec<Finding> {
    let mut out = Vec::new();
    let pkgs = match mirror::locked_registry_packages(root) {
        Ok(pkgs) => pkgs,
        Err(why) => {
            out.push(Finding::Fail(format!(
                "Cargo.lock's registry slice could not be read, so nothing about the mirror can \
                 be judged: {why}"
            )));
            return out;
        }
    };
    let live_digest = registry_slice_digest(&pkgs);
    let rendered = render(&pkgs);

    // --- the shippable fragment ------------------------------------------
    let fragment_path = root.join(FRAGMENT_FILE);
    let fragment = match std::fs::read_to_string(&fragment_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            out.push(Finding::Fail(format!(
                "cannot read {}: {e}",
                fragment_path.display()
            )));
            None
        }
    };
    let mut mirror_dirs: Vec<(String, String)> = Vec::new();
    let fragment_findings_before = out.len();
    match &fragment {
        None => out.push(Finding::Note(format!(
            "no `{FRAGMENT_FILE}` in this tree — the mirror `[source]` block is not shipped, so \
             a public clone builds from crates.io. Write it with `cargo forge mirror config \
             --write` when the mirror is delivered."
        ))),
        Some(text) => match parse(text, &fragment_path) {
            Err(why) => out.push(Finding::Fail(format!(
                "the shipped mirror config is unreadable: {why}. Fix: `cargo forge mirror \
                 config --write`."
            ))),
            Ok(anchor) => {
                if anchor.format != 1 {
                    out.push(Finding::Fail(format!(
                        "{FRAGMENT_FILE} declares `format = {}`; this build understands 1",
                        anchor.format
                    )));
                }
                if anchor.registry_lock_sha256 != live_digest
                    || anchor.registry_packages != pkgs.len() as i64
                {
                    out.push(Finding::Fail(format!(
                        "{FRAGMENT_FILE} and Cargo.lock DISAGREE about what is mirrored: the \
                         file anchors {} package(s) at registry-lock-sha256 {}, the lock has {} \
                         at {live_digest}. A public snapshot would ship a source replacement for \
                         a package set that is not this one. Fix: `cargo forge mirror config \
                         --write` and commit the file.",
                        anchor.registry_packages,
                        anchor.registry_lock_sha256,
                        pkgs.len()
                    )));
                }
                if anchor.replace_with != SOURCE_NAME {
                    out.push(Finding::Note(format!(
                        "{FRAGMENT_FILE} replaces crates.io with `{}` rather than \
                         `{SOURCE_NAME}` — legal, but the delivery instructions name the \
                         latter.",
                        anchor.replace_with
                    )));
                }
                if anchor.local_registry != anchor.source_local_registry {
                    out.push(Finding::Fail(format!(
                        "{FRAGMENT_FILE} names two different mirror directories: the anchor says \
                         `{}`, `[source.{}]` says `{}`. Cargo would use the second and the gate \
                         would audit the first. Fix: `cargo forge mirror config --write`.",
                        anchor.local_registry, anchor.replace_with, anchor.source_local_registry
                    )));
                }
                if Path::new(&anchor.source_local_registry).is_absolute()
                    || anchor.source_local_registry.contains("..")
                {
                    // NOT added to `mirror_dirs`: the loop below stats and
                    // walks every path in it, and a path this branch has just
                    // refused as escaping is the last one to hand to the
                    // filesystem.
                    out.push(Finding::Fail(format!(
                        "{FRAGMENT_FILE} points `local-registry` at `{}` — a shipped fragment \
                         must name a path inside the clone, not an absolute or escaping one. \
                         Nothing at that path was inspected.",
                        anchor.source_local_registry
                    )));
                } else {
                    mirror_dirs.push((FRAGMENT_FILE.to_string(), anchor.source_local_registry));
                }
            }
        },
    }

    // The fragment is GENERATED, so the honest question is not "does it parse"
    // but "is it the file `--write` produces for this lock". That one rule
    // covers every way a shipped fragment can lie about the mirror at once —
    // the digest, the package count, the source name, and the directory the
    // `[source]` table points cargo at. Reported only when the parsed checks
    // above found nothing, so a drifted anchor is named once and precisely.
    if let Some(text) = &fragment
        && out.len() == fragment_findings_before
        && *text != rendered
    {
        let at = text
            .lines()
            .zip(rendered.lines())
            .position(|(a, b)| a != b)
            .map_or_else(
                || {
                    format!(
                        "it has {} line(s), `--write` produces {}",
                        text.lines().count(),
                        rendered.lines().count()
                    )
                },
                |i| format!("first difference at line {}", i + 1),
            );
        out.push(Finding::Fail(format!(
            "{FRAGMENT_FILE} is not the file `cargo forge mirror config --write` produces for \
             this lock ({at}) — a hand-edited fragment can point cargo's `local-registry` at a \
             directory the delivery never creates, or rename the source, while every anchor \
             number still agrees. Fix: `cargo forge mirror config --write` and commit it."
        )));
    }

    // Export coverage, judged whether or not the fragment parsed — a dangling
    // allowlist row is as wrong as a missing one.
    match publish_manifest_export(root, FRAGMENT_FILE) {
        Ok(Some(Export::Yes)) if fragment.is_some() => {}
        Ok(Some(Export::Yes)) => out.push(Finding::Fail(format!(
            "{PUBLISH_MANIFEST} exports `{FRAGMENT_FILE}`, but that file is NOT in this tree — \
             the allowlist row is dangling, and the public snapshot would ship Cargo.lock with \
             no `[source]` fragment to reach the mirror it was cut against. Fix: \
             `cargo forge mirror config --write` and commit the file, or delete the row."
        ))),
        Ok(Some(Export::NoRow)) if fragment.is_some() => out.push(Finding::Fail(format!(
            "{FRAGMENT_FILE} exists but has NO row in {PUBLISH_MANIFEST}, which is an \
             allowlist — only listed paths ship. The public snapshot would carry Cargo.lock \
             with no way to reach the mirror it was cut against. Fix: add `{FRAGMENT_FILE}` to \
             {PUBLISH_MANIFEST}."
        ))),
        Ok(Some(Export::NoRow)) => out.push(Finding::Note(format!(
            "neither this tree nor {PUBLISH_MANIFEST} carries `{FRAGMENT_FILE}` — consistent, \
             and the mirror is simply not delivered here."
        ))),
        Ok(Some(Export::Excluded(row))) => out.push(Finding::Fail(format!(
            "{PUBLISH_MANIFEST} takes `{FRAGMENT_FILE}` back out with `{row}` — a `!` row is \
             how that allowlist EXCLUDES a path it would otherwise carry (it holds 11 of them), \
             so the fragment does not ship and the public snapshot would carry Cargo.lock with \
             no way to reach its mirror. Fix: delete that row."
        ))),
        Ok(Some(Export::Centrally(dir))) => out.push(Finding::Fail(format!(
            "{FRAGMENT_FILE} sits under `{dir}/`, which {PUBLISH_MANIFEST} says central policy \
             excludes regardless of this allowlist — it cannot ship from there."
        ))),
        Ok(None) => out.push(Finding::Note(format!(
            "{PUBLISH_MANIFEST} is absent from this tree — export coverage of {FRAGMENT_FILE} \
             not judged."
        ))),
        Err(why) => out.push(Finding::Fail(why)),
    }

    // --- has the repository's own default been flipped? -------------------
    match repo_default_replacement(root) {
        Err(why) => out.push(Finding::Fail(why)),
        Ok(None) => out.push(Finding::Note(format!(
            "{CARGO_CONFIG_FILE} does not replace the crates.io source — this repository still \
             builds from crates.io, which is the correct state until the mirror is DELIVERED."
        ))),
        Ok(Some((name, local))) => match local {
            Some(local) => {
                out.push(Finding::Note(format!(
                    "{CARGO_CONFIG_FILE} REPLACES crates.io with `{name}` at `{local}` — the \
                     default is flipped, so the directory below is load-bearing for every build \
                     in this tree."
                )));
                mirror_dirs.push((CARGO_CONFIG_FILE.to_string(), local));
            }
            None => out.push(Finding::Fail(format!(
                "{CARGO_CONFIG_FILE} replaces crates.io with `{name}`, but `[source.{name}]` \
                 declares no `local-registry` — cargo would fail to resolve and this gate cannot \
                 audit what it cannot name."
            ))),
        },
    }

    // --- does a present mirror actually cover the lock? -------------------
    // Grouped by PATH, never deduplicated by first-declarer: the fragment and
    // `.cargo/config.toml` normally name the same directory, and an absent one
    // is a NOTE for the first and a FAILURE for the second. Dropping the second
    // claim because the path was already seen would silently discard the
    // strictest rule this audit has.
    let mut by_path: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (declared_by, relative) in mirror_dirs {
        by_path.entry(relative).or_default().push(declared_by);
    }
    for (relative, declarers) in by_path {
        let dir = root.join(&relative);
        let load_bearing = declarers.iter().any(|d| d == CARGO_CONFIG_FILE);
        let declared_by = declarers.join(" and ");
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if load_bearing {
                    out.push(Finding::Fail(format!(
                        "{CARGO_CONFIG_FILE} points cargo at `{relative}`, which does not exist \
                         — the default was flipped without the bytes behind it, so every build \
                         in this tree fails to resolve."
                    )));
                } else {
                    out.push(Finding::Note(format!(
                        "`{relative}` (named by {declared_by}) is not present — the mirror is \
                         not delivered in this tree, so coverage is not judged here. Deliver it \
                         with `mirror unbundle`, then `mirror verify --dir {relative}`."
                    )));
                }
            }
            Err(e) => out.push(Finding::Fail(format!(
                "cannot inspect `{relative}` (named by {declared_by}): {e}"
            ))),
            Ok(_) => match mirror::verify(root, &dir, row_anchor) {
                Err(why) => out.push(Finding::Fail(format!(
                    "the mirror at `{relative}` (named by {declared_by}) could not be verified: \
                     {why}"
                ))),
                Ok(verdict) => {
                    if verdict.ok {
                        out.push(Finding::Note(format!(
                            "the mirror at `{relative}` covers all {} registry lock entries and \
                             every cksum agrees{}.",
                            pkgs.len(),
                            match row_anchor.why_absent() {
                                None =>
                                    ", and every index row is upstream's own bytes — `deps` and \
                                     `features` included"
                                        .to_string(),
                                Some(why) => format!(
                                    ". Row CONTENT was NOT anchored ({why}), so `deps` and \
                                     `features` are proven only as far as Cargo.lock's resolved \
                                     dependency edges reach"
                                ),
                            }
                        )));
                    } else {
                        let drift: Vec<&str> = verdict
                            .log
                            .lines()
                            .map(str::trim)
                            .filter(|l| l.starts_with("DRIFT:"))
                            .take(8)
                            .collect();
                        out.push(Finding::Fail(format!(
                            "the mirror at `{relative}` (named by {declared_by}) does NOT match \
                             Cargo.lock. First drift line(s): {}. Fix: `cargo forge mirror emit \
                             --out {relative}`, then `cargo forge mirror verify --dir \
                             {relative}`.",
                            drift.join(" | ")
                        )));
                    }
                }
            },
        }
    }
    out
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{index_rel_path, sha256_hex};
    use std::path::PathBuf;

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Fixture {
        fn root(&self) -> PathBuf {
            self.0.join("ws")
        }
    }

    const PKGS: &[(&str, &str, &[u8])] = &[
        ("alpha-config-test", "1.2.3", b"alpha crate bytes"),
        ("abc", "0.1.0", b"three-char name bytes"),
    ];

    /// A workspace with a lock, a publish allowlist that exports the fragment,
    /// and a `.cargo/config.toml` that does NOT flip the default — the state
    /// this repository is actually in.
    fn fixture(tag: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("aterm-forge-mconfig-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fx = Fixture(dir);
        std::fs::create_dir_all(fx.root().join("tools")).unwrap();
        std::fs::create_dir_all(fx.root().join("publish")).unwrap();
        std::fs::create_dir_all(fx.root().join(".cargo")).unwrap();
        let mut lock = String::from(
            "version = 4\n\n[[package]]\nname = \"aterm-thing\"\nversion = \"0.1.0\"\n",
        );
        for (name, vers, bytes) in PKGS {
            let _ = writeln!(
                lock,
                "\n[[package]]\nname = \"{name}\"\nversion = \"{vers}\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{}\"",
                sha256_hex(bytes)
            );
        }
        std::fs::write(fx.root().join("Cargo.lock"), lock).unwrap();
        std::fs::write(
            fx.root().join(PUBLISH_MANIFEST),
            format!("Cargo.toml\nCargo.lock\ncrates\n{FRAGMENT_FILE}\n"),
        )
        .unwrap();
        std::fs::write(
            fx.root().join(CARGO_CONFIG_FILE),
            "[target.'cfg(trust_verify)']\nrustflags = [\"-Ztrust-verify=off\"]\n",
        )
        .unwrap();
        fx
    }

    fn write_fragment(fx: &Fixture) {
        let pkgs = mirror::locked_registry_packages(&fx.root()).unwrap();
        std::fs::write(fx.root().join(FRAGMENT_FILE), render(&pkgs)).unwrap();
    }

    /// Build a mirror that matches the fixture lock exactly.
    fn write_mirror(fx: &Fixture, relative: &str) {
        let dir = fx.root().join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, vers, bytes) in PKGS {
            std::fs::write(dir.join(format!("{name}-{vers}.crate")), bytes).unwrap();
            let ipath = dir.join("index").join(index_rel_path(name).unwrap());
            std::fs::create_dir_all(ipath.parent().unwrap()).unwrap();
            std::fs::write(
                ipath,
                format!(
                    "{{\"name\":\"{name}\",\"vers\":\"{vers}\",\"deps\":[],\"cksum\":\"{}\",\
                     \"features\":{{}},\"yanked\":false}}\n",
                    sha256_hex(bytes)
                ),
            )
            .unwrap();
        }
    }

    fn fail_lines(fx: &Fixture) -> Vec<String> {
        audit(&fx.root(), &mirror::RowAnchor::absent("test fixture"))
            .into_iter()
            .filter_map(|f| match f {
                Finding::Fail(why) => Some(why),
                Finding::Note(_) => None,
            })
            .collect()
    }

    fn note_lines(fx: &Fixture) -> Vec<String> {
        audit(&fx.root(), &mirror::RowAnchor::absent("test fixture"))
            .into_iter()
            .filter_map(|f| match f {
                Finding::Note(what) => Some(what),
                Finding::Fail(_) => None,
            })
            .collect()
    }

    // --- the shape of the thing -------------------------------------------

    #[test]
    fn the_fragment_renders_deterministically_and_parses_back() {
        let fx = fixture("render");
        let pkgs = mirror::locked_registry_packages(&fx.root()).unwrap();
        let a = render(&pkgs);
        assert_eq!(
            a,
            render(&pkgs),
            "the fragment must be a function of the lock"
        );
        let anchor = parse(&a, Path::new(FRAGMENT_FILE)).unwrap();
        assert_eq!(anchor.format, 1);
        assert_eq!(anchor.registry_packages, PKGS.len() as i64);
        assert_eq!(anchor.replace_with, SOURCE_NAME);
        assert_eq!(anchor.local_registry, MIRROR_DIR);
        assert_eq!(anchor.source_local_registry, MIRROR_DIR);
        assert_eq!(
            anchor.registry_lock_sha256,
            crate::mirror_bundle::registry_slice_digest(&pkgs)
        );
    }

    /// The hard boundary, as a test: this verb must not be able to flip a
    /// default. It writes ONE path, and that path is not one cargo reads.
    #[test]
    fn writing_the_fragment_flips_no_default() {
        let fx = fixture("nodefault");
        let before = std::fs::read_to_string(fx.root().join(CARGO_CONFIG_FILE)).unwrap();
        let outcome = run_config(&fx.root(), true).unwrap();
        assert!(outcome.ok, "{}", outcome.log);
        assert!(fx.root().join(FRAGMENT_FILE).is_file());
        assert_eq!(
            std::fs::read_to_string(fx.root().join(CARGO_CONFIG_FILE)).unwrap(),
            before,
            "mirror config must never touch .cargo/config.toml"
        );
        // Cargo reads `config.toml` / `config` on its search path and nothing
        // else, so the generated path being neither is the whole guarantee.
        let leaf = Path::new(FRAGMENT_FILE).file_name().unwrap();
        assert_ne!(leaf, "config.toml");
        assert_ne!(leaf, "config");
        assert!(!FRAGMENT_FILE.starts_with(".cargo/"));
    }

    #[test]
    fn a_green_tree_has_no_failures_and_says_why_it_is_not_judging_coverage() {
        let fx = fixture("green");
        write_fragment(&fx);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        let notes = note_lines(&fx);
        assert!(
            notes.iter().any(|n| n.contains("is not present")),
            "an absent mirror must be named, not silently passed: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("does not replace the crates.io source")),
            "{notes:?}"
        );
    }

    // --- the obligation's teeth --------------------------------------------

    #[test]
    fn a_fragment_that_disagrees_with_the_lock_fails_and_names_the_fix() {
        let fx = fixture("drift");
        write_fragment(&fx);
        // The lock moves — a third-party bump, the one edit this anchor exists
        // to notice.
        let lock = fx.root().join("Cargo.lock");
        let text = std::fs::read_to_string(&lock).unwrap();
        std::fs::write(
            &lock,
            text.replacen("version = \"1.2.3\"", "version = \"1.2.4\"", 1),
        )
        .unwrap();
        let fails = fail_lines(&fx);
        assert_eq!(fails.len(), 1, "{fails:?}");
        assert!(
            fails[0].contains("DISAGREE about what is mirrored"),
            "{}",
            fails[0]
        );
        assert!(
            fails[0].contains("cargo forge mirror config --write"),
            "a refusal must name the command that fixes it: {}",
            fails[0]
        );
        // Regenerating clears it — the ratchet is closable, not a dead end.
        write_fragment(&fx);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
    }

    /// The registry slice is the anchor precisely so FIRST-PARTY churn does not
    /// fire it. A release version bump touches every workspace member and must
    /// leave this gate silent.
    #[test]
    fn a_first_party_version_bump_is_not_mirror_drift() {
        let fx = fixture("firstparty");
        write_fragment(&fx);
        let lock = fx.root().join("Cargo.lock");
        let text = std::fs::read_to_string(&lock).unwrap();
        std::fs::write(
            &lock,
            text.replacen(
                "name = \"aterm-thing\"\nversion = \"0.1.0\"",
                "name = \"aterm-thing\"\nversion = \"0.2.0\"",
                1,
            ),
        )
        .unwrap();
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
    }

    #[test]
    fn a_fragment_absent_from_the_publish_allowlist_fails() {
        let fx = fixture("unshipped");
        write_fragment(&fx);
        let manifest = fx.root().join(PUBLISH_MANIFEST);
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, text.replace(&format!("{FRAGMENT_FILE}\n"), "")).unwrap();
        let fails = fail_lines(&fx);
        assert_eq!(fails.len(), 1, "{fails:?}");
        assert!(fails[0].contains("NO row in"), "{}", fails[0]);
        assert!(fails[0].contains("allowlist"), "{}", fails[0]);
    }

    #[test]
    fn a_fragment_whose_two_paths_disagree_fails() {
        let fx = fixture("twopaths");
        write_fragment(&fx);
        let path = fx.root().join(FRAGMENT_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        // Change only the `[source.aterm-mirror]` copy — cargo would use this
        // one and the anchor would describe the other.
        let (head, tail) = text.rsplit_once("local-registry = \"mirror\"").unwrap();
        std::fs::write(&path, format!("{head}local-registry = \"elsewhere\"{tail}")).unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("two different mirror directories")),
            "{fails:?}"
        );
    }

    #[test]
    fn an_absolute_or_escaping_local_registry_fails() {
        for bad in ["/opt/mirror", "../outside/mirror"] {
            let fx = fixture(&format!("abs{}", bad.len()));
            write_fragment(&fx);
            let path = fx.root().join(FRAGMENT_FILE);
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, text.replace("\"mirror\"", &format!("\"{bad}\""))).unwrap();
            let fails = fail_lines(&fx);
            assert!(
                fails
                    .iter()
                    .any(|f| f.contains("must name a path inside the clone")),
                "`{bad}`: {fails:?}"
            );
        }
    }

    #[test]
    fn a_missing_fragment_is_a_note_not_a_failure() {
        let fx = fixture("absent");
        // …when the allowlist does not claim it either. A tree with neither
        // is simply a tree where the mirror is not delivered.
        std::fs::write(
            fx.root().join(PUBLISH_MANIFEST),
            "Cargo.toml\nCargo.lock\ncrates\n",
        )
        .unwrap();
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        assert!(
            note_lines(&fx)
                .iter()
                .any(|n| n.contains("builds from crates.io")),
            "{:?}",
            note_lines(&fx)
        );
    }

    /// An allowlist row for a file that is not there is as wrong as a file
    /// with no row: the snapshot would ship `Cargo.lock` and a dangling
    /// export. One of the three shapes that used to leave `[OB-16]` GREEN.
    #[test]
    fn an_allowlist_row_naming_an_absent_fragment_fails() {
        let fx = fixture("dangling");
        let fails = fail_lines(&fx);
        assert!(fails.iter().any(|f| f.contains("dangling")), "{fails:?}");
    }

    /// The publish allowlist's negation rows are its EXCLUSION mechanism —
    /// `publish/manifest.txt` carries 11 of them — so one appended `!` row
    /// stops the fragment shipping. Reading them as decoration left the gate
    /// GREEN while the public snapshot lost the file.
    #[test]
    fn one_negation_row_defeats_the_export_and_is_named() {
        let fx = fixture("negated");
        write_fragment(&fx);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        let manifest = fx.root().join(PUBLISH_MANIFEST);
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, format!("{text}!{FRAGMENT_FILE}\n")).unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("takes") && f.contains(&format!("!{FRAGMENT_FILE}"))),
            "{fails:?}"
        );

        // A `!` row on a DIRECTORY above it excludes it just the same — that
        // is how the real file's `!vendor/*/.github` rows work.
        std::fs::write(&manifest, format!("{text}!tools\n")).unwrap();
        let fails = fail_lines(&fx);
        assert!(fails.iter().any(|f| f.contains("`!tools`")), "{fails:?}");

        // And a row this reader cannot judge is REFUSED, never guessed.
        std::fs::write(&manifest, format!("{text}!tools/cargo-*.toml\n")).unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails.iter().any(|f| f.contains("inside a path component")),
            "{fails:?}"
        );
    }

    /// A hand-edited fragment can point cargo at a directory the delivery
    /// never creates while every anchor number still agrees. The file is
    /// generated, so the rule is byte equality with what `--write` produces.
    #[test]
    fn a_repointed_local_registry_fails_even_with_a_correct_anchor() {
        let fx = fixture("repointed");
        write_fragment(&fx);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        let path = fx.root().join(FRAGMENT_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace(
                &format!("local-registry = \"{MIRROR_DIR}\""),
                "local-registry = \"never/going/to/exist\"",
            ),
        )
        .unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails.iter().any(|f| f.contains("is not the file")),
            "{fails:?}"
        );
    }

    #[test]
    fn an_unparseable_fragment_fails_rather_than_being_ignored() {
        let fx = fixture("garbage");
        std::fs::write(fx.root().join(FRAGMENT_FILE), "this is not toml = = =\n").unwrap();
        let fails = fail_lines(&fx);
        assert!(fails.iter().any(|f| f.contains("unreadable")), "{fails:?}");
    }

    // --- a mirror that IS present ------------------------------------------

    #[test]
    fn a_present_mirror_must_cover_the_lock() {
        let fx = fixture("present");
        write_fragment(&fx);
        write_mirror(&fx, MIRROR_DIR);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        assert!(
            note_lines(&fx)
                .iter()
                .any(|n| n.contains("covers all 2 registry lock entries")),
            "{:?}",
            note_lines(&fx)
        );

        // Delete one tarball: the mirror is present but incomplete.
        std::fs::remove_file(
            fx.root()
                .join(MIRROR_DIR)
                .join(format!("{}-{}.crate", PKGS[0].0, PKGS[0].1)),
        )
        .unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("does NOT match Cargo.lock")),
            "{fails:?}"
        );
        assert!(
            fails.iter().any(|f| f.contains("cargo forge mirror emit")),
            "{fails:?}"
        );
    }

    #[test]
    fn a_present_mirror_with_a_drifted_tarball_fails() {
        let fx = fixture("drifted");
        write_fragment(&fx);
        write_mirror(&fx, MIRROR_DIR);
        let crate_path = fx
            .root()
            .join(MIRROR_DIR)
            .join(format!("{}-{}.crate", PKGS[0].0, PKGS[0].1));
        std::fs::write(&crate_path, b"alpha crate byteS").unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("does NOT match Cargo.lock")),
            "{fails:?}"
        );
    }

    /// The tripwire on the hard boundary: someone flips the default in
    /// `.cargo/config.toml` without delivering the bytes. Cargo would fail to
    /// resolve; the gate says so first, and says WHY.
    #[test]
    fn a_flipped_default_without_the_mirror_fails() {
        let fx = fixture("flipped");
        write_fragment(&fx);
        let config = fx.root().join(CARGO_CONFIG_FILE);
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            &config,
            format!(
                "{text}\n[source.crates-io]\nreplace-with = \"aterm-mirror\"\n\n\
                 [source.aterm-mirror]\nlocal-registry = \"mirror\"\n"
            ),
        )
        .unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("the default was flipped without the bytes")),
            "{fails:?}"
        );

        // Deliver the bytes and the same flip is fine — the gate objects to the
        // GAP, not to the delivery.
        write_mirror(&fx, MIRROR_DIR);
        assert!(fail_lines(&fx).is_empty(), "{:?}", fail_lines(&fx));
        assert!(
            note_lines(&fx)
                .iter()
                .any(|n| n.contains("the default is flipped")),
            "{:?}",
            note_lines(&fx)
        );
    }

    #[test]
    fn a_flip_to_a_source_with_no_local_registry_is_named() {
        let fx = fixture("dangling");
        write_fragment(&fx);
        let config = fx.root().join(CARGO_CONFIG_FILE);
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            &config,
            format!("{text}\n[source.crates-io]\nreplace-with = \"nowhere\"\n"),
        )
        .unwrap();
        let fails = fail_lines(&fx);
        assert!(
            fails
                .iter()
                .any(|f| f.contains("declares no `local-registry`")),
            "{fails:?}"
        );
    }
}

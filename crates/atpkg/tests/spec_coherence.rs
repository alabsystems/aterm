// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Spec<->index<->seed coherence for the atpkg publish lane, enforced.
//!
//! Every rule here used to be operator discipline alone — prose in
//! `tools/atpkg-programs.spec`'s header notes with no refusal anywhere. aterm
//! has no CI by owner decision (tools/verify.sh); the merge contract's Test
//! stage runs this file, so the discipline now fails a merge instead of a
//! fleet. The invariants span three artifacts nothing else compares:
//!
//!   * `tools/atpkg-programs.spec` — the committed table of the LIVE published
//!     index's pins ("tracks the live index, never a wish");
//!   * `tools/atpkg-refresh-seed.sh` — the seed refresher whose pack lanes
//!     decide what a release cut seals into aterm.app (§9.1);
//!   * the root `Cargo.toml`'s `[workspace.metadata.atpkg]` — the pack surface
//!     and the compiled-in default index account every shipped client resolves
//!     (`crate::discovery::resolve_account`, stamped by aterm-update-core's
//!     build.rs).
//!
//! A divergence between any two publishes silently: a spec row without a
//! signed pack pins the unpublishable and wedges its whole coherence group on
//! every client (§7); a spec row the seed lanes never pack drops out of fresh
//! installs; a placeholder build column signs an index pinning build 0 with no
//! refusal anywhere. Hermetic by construction: reads committed files via
//! `CARGO_MANIFEST_DIR` only — no network, no keys, no subprocess.
//!
//! The textual parses deliberately MIRROR the consumers' own readers
//! (atpkg-index.sh's `read -r name repo policy build group` loop; the
//! refresher's `PLAIN_PROGRAMS`/`RUSTC_PROGRAMS` defaults and its literal
//! `PROG=` bundle lane). If a consumer's grammar moves, this file must move in
//! the same change — that forced co-review is the point, so a shape this
//! parser no longer finds is a hard failure, never a silent skip.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The trust verifier tuple locked to the trust rustc fork — the spec's
/// `rustc` coherence group moves all-or-nothing (§7). One source of truth for
/// tests (c) and (d); the spec's COHERENCE GROUP note is the prose twin.
const RUSTC_TUPLE: [&str; 4] = ["trust", "trust-ir", "trust-cg", "trust-vc"];

/// Every known unpacked org system on the roadmap. Each must stay named in the
/// spec's FUTURE MEMBERS / NOT-YET-PUBLISHABLE notes until it graduates to an
/// active row — a name that silently vanishes from both is a roadmap loss no
/// one decided.
const ROADMAP: [&str; 5] = ["orca-alab", "ty", "astream", "amail", "trust-wp"];

/// `crates/atpkg` -> repo root, verified by the presence of the spec itself so
/// a layout move fails loudly here instead of as a confusing read error below.
fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR has a workspace root two levels up")
        .to_path_buf();
    assert!(
        root.join("tools/atpkg-programs.spec").is_file(),
        "repo root {} has no tools/atpkg-programs.spec — if the spec moved, \
         update crates/atpkg/tests/spec_coherence.rs to follow it",
        root.display()
    );
    root
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel))
        .unwrap_or_else(|e| panic!("cannot read {rel} from the repo root: {e}"))
}

/// One active (non-comment) spec row, in atpkg-index.sh's own grammar:
/// `name repo policy build [coherence_group]`.
struct SpecRow {
    line_no: usize,
    name: String,
    policy: String,
    build: u64,
    group: Option<String>,
}

/// Parse the active table the way atpkg-index.sh's `read -r` loop does
/// (comment/blank skip; a missing trailing newline still yields the last row
/// via `lines()`), but REFUSE shapes the loop would silently tolerate: a row
/// with the wrong field count is drift in the file the indexer signs from.
fn active_rows(spec: &str) -> Vec<SpecRow> {
    let mut rows = Vec::new();
    for (idx, raw) in spec.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line_no = idx + 1;
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert!(
            fields.len() == 4 || fields.len() == 5,
            "tools/atpkg-programs.spec:{line_no}: active row has {} fields, \
             not the documented `name repo policy build [coherence_group]` — \
             fix the row (atpkg-index.sh reads exactly those columns and \
             silently ignores extras)",
            fields.len()
        );
        let build = fields[3].parse::<u64>().unwrap_or_else(|_| {
            panic!(
                "tools/atpkg-programs.spec:{line_no}: build column {:?} for \
                 {:?} is not a number — refresh it from the PACK-SPEC line \
                 atpkg-pack.sh prints at publish time (spec BUILD COLUMN note)",
                fields[3], fields[0]
            )
        });
        rows.push(SpecRow {
            line_no,
            name: fields[0].to_string(),
            policy: fields[2].to_string(),
            build,
            // atpkg-index.sh treats `-` as "no group" (its column-5 read).
            group: match fields.get(4) {
                Some(&"-") | None => None,
                Some(g) => Some((*g).to_string()),
            },
        });
    }
    assert!(
        !rows.is_empty(),
        "tools/atpkg-programs.spec has no active rows — an empty table would \
         sign an index that pins nothing; restore the published program rows"
    );
    rows
}

fn active_names(rows: &[SpecRow]) -> BTreeSet<String> {
    rows.iter().map(|r| r.name.clone()).collect()
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Whole-token search: `name` bounded by non-name characters on both sides, so
/// `ty` never matches inside `authenticity` and `trust-wp` never matches
/// inside `trust-wp-rustc`. ASCII names only; the haystack may carry UTF-8
/// (the spec's `§` cross-references) — byte-boundary checks on continuation
/// bytes are safely non-word.
fn mentions_token(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = text[from..].find(name) {
        let start = from + pos;
        let end = start + name.len();
        let pre_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let post_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if pre_ok && post_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Literal `PROG=<name>` assignments on a non-comment shell line — the bundle
/// lane's `env PROG=trust …`. `PROG="$prog"` (the per-program loops) starts
/// with a quote, so it never yields a token here; those loops are covered by
/// the `PLAIN_PROGRAMS`/`RUSTC_PROGRAMS` defaults instead.
fn literal_prog_assignments(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = line[from..].find("PROG=") {
        let start = from + pos;
        let val_start = start + "PROG=".len();
        let bounded = start == 0 || !is_word_byte(bytes[start - 1]);
        if bounded {
            let val: String = line[val_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !val.is_empty() {
                out.push(val);
            }
        }
        from = val_start;
    }
    out
}

/// The full program set tools/atpkg-refresh-seed.sh packs on an unnarrowed
/// run: the PLAIN_PROGRAMS default, the RUSTC_PROGRAMS siblings, and every
/// literal `PROG=` lane (the trust bundle). Parsed textually against the
/// script's committed shapes; a shape this cannot find is a hard failure so
/// the script and this test can only move together.
fn seed_pack_lanes(script: &str) -> BTreeSet<String> {
    const PLAIN_PREFIX: &str = "PLAIN_PROGRAMS=\"${PROGRAMS_ONLY:-";
    const RUSTC_PREFIX: &str = "RUSTC_PROGRAMS=\"";
    let mut lanes = BTreeSet::new();
    let mut plain_seen = false;
    let mut rustc_seen = false;
    for raw in script.lines() {
        let line = raw.trim_start();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(PLAIN_PREFIX) {
            let list = rest.split('}').next().unwrap_or("");
            lanes.extend(list.split_whitespace().map(str::to_string));
            plain_seen = true;
        }
        if let Some(rest) = line.strip_prefix(RUSTC_PREFIX) {
            let list = rest.split('"').next().unwrap_or("");
            lanes.extend(list.split_whitespace().map(str::to_string));
            rustc_seen = true;
        }
        lanes.extend(literal_prog_assignments(line));
    }
    assert!(
        plain_seen,
        "tools/atpkg-refresh-seed.sh no longer carries the \
         `{PLAIN_PREFIX}…}}\"` default this test parses — if the plain pack \
         lane changed shape, update seed_pack_lanes() in the same change so \
         spec<->seed coverage stays enforced"
    );
    assert!(
        rustc_seen,
        "tools/atpkg-refresh-seed.sh no longer carries the \
         `{RUSTC_PREFIX}…\"` default this test parses — if the rustc tuple \
         lane changed shape, update seed_pack_lanes() in the same change so \
         spec<->seed coverage stays enforced"
    );
    lanes
}

/// (a) Every active row parses under the indexer's grammar with a REAL
/// (non-placeholder) build, a policy the client's manifest schema knows, and a
/// unique name — a duplicate would emit two `[programs.<name>]` tables and the
/// signed index would fail every client's TOML parse.
#[test]
fn active_rows_parse_with_published_build_numbers() {
    let root = repo_root();
    let rows = active_rows(&read(&root, "tools/atpkg-programs.spec"));
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for row in &rows {
        assert!(
            row.build != 0,
            "tools/atpkg-programs.spec:{}: {:?} has placeholder build 0 — \
             publishing it would sign an index pinning build 0 for every \
             client; refresh the row from the PACK-SPEC line atpkg-pack.sh \
             prints at publish time (spec BUILD COLUMN note)",
            row.line_no,
            row.name
        );
        assert!(
            matches!(row.policy.as_str(), "prebuilt-only" | "prebuilt-or-build"),
            "tools/atpkg-programs.spec:{}: policy {:?} for {:?} is not one \
             the client parses (crates/atpkg/src/manifest.rs: prebuilt-only | \
             prebuilt-or-build) — fix the row's policy column",
            row.line_no,
            row.policy,
            row.name
        );
        assert!(
            seen.insert(row.name.as_str()),
            "tools/atpkg-programs.spec:{}: duplicate row for {:?} — \
             atpkg-index.sh would emit two [programs.{}] tables and every \
             client would refuse the signed index as malformed TOML; delete \
             one row",
            row.line_no,
            row.name,
            row.name
        );
    }
}

/// (b) trust-wp must NOT be active: its `trust-wp-rustc` is linked with
/// absolute rpaths into hash-named private sysroots (the fix lives in the
/// trust-wp repo), and a pinned-but-unfetchable rustc-group member aborts the
/// WHOLE trust tuple on every client (§7).
#[test]
fn trust_wp_is_not_an_active_row() {
    let root = repo_root();
    let rows = active_rows(&read(&root, "tools/atpkg-programs.spec"));
    assert!(
        !active_names(&rows).contains("trust-wp"),
        "tools/atpkg-programs.spec lists trust-wp as an active row — it is \
         NOT relocatable (absolute rpaths; spec NOT-YET-PUBLISHABLE note) and \
         pinning it wedges the entire rustc coherence group on every client \
         (§7). Delete the row; it rejoins only when a relocatable build has a \
         signed pkg-trust-wp-<build>.toml"
    );
}

/// (c) The `rustc` coherence group is exactly the trust verifier tuple. A
/// missing member version-splits the tuple (it stops moving with its
/// siblings); an extra member that cannot stage aborts the whole group on
/// every client (§7).
#[test]
fn rustc_coherence_group_is_exactly_the_trust_tuple() {
    let root = repo_root();
    let rows = active_rows(&read(&root, "tools/atpkg-programs.spec"));
    let got: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.group.as_deref() == Some("rustc"))
        .map(|r| r.name.as_str())
        .collect();
    let want: BTreeSet<&str> = RUSTC_TUPLE.into_iter().collect();
    assert_eq!(
        got, want,
        "tools/atpkg-programs.spec: rustc coherence_group must be exactly \
         {RUSTC_TUPLE:?} (spec COHERENCE GROUP note, §7) — a missing member \
         version-splits the tuple, an extra unstageable member aborts the \
         whole group on every client. Fix the group column of the drifted \
         row(s); a NEW tuple member also needs a lane in \
         tools/atpkg-refresh-seed.sh (RUSTC_PROGRAMS)"
    );
}

/// (d) Seed<->spec coverage, both directions: an unnarrowed
/// tools/atpkg-refresh-seed.sh run must pack exactly the active set. A spec
/// row the seed never packs drops out of fresh installs (the refresher's own
/// 2b gate then refuses every refresh); a seed lane the spec omits seals pins
/// the published index does not carry, and the two lanes' shared counter
/// (spec INDEX COUNTER note) makes that a real client-visible split.
#[test]
fn seed_refresher_packs_exactly_the_active_set() {
    let root = repo_root();
    let spec = active_names(&active_rows(&read(&root, "tools/atpkg-programs.spec")));
    let seed = seed_pack_lanes(&read(&root, "tools/atpkg-refresh-seed.sh"));
    let unpacked: Vec<&String> = spec.difference(&seed).collect();
    assert!(
        unpacked.is_empty(),
        "tools/atpkg-refresh-seed.sh has no pack lane for active spec \
         program(s) {unpacked:?} — every fresh install's seed would omit them \
         while the index pins them. Add each to the script's PLAIN_PROGRAMS \
         default (or RUSTC_PROGRAMS for a trust-tuple member), or remove the \
         row from tools/atpkg-programs.spec if it was never published"
    );
    let unindexed: Vec<&String> = seed.difference(&spec).collect();
    assert!(
        unindexed.is_empty(),
        "tools/atpkg-refresh-seed.sh packs program(s) {unindexed:?} that \
         tools/atpkg-programs.spec does not list — the seed would seal pins \
         the published index never carries. Land the spec row in the same \
         change (after the FUTURE MEMBERS runway: pack, UPLOAD the signed \
         pkg, take the real build), or drop the extra lane from the script"
    );
}

/// (e) Roadmap conservation: every known unpacked org system stays named in
/// the spec's notes until it graduates to an active row, so no future member
/// can vanish from the plan as the side effect of a comment rewrite.
#[test]
fn roadmap_names_cannot_vanish_from_the_spec_notes() {
    let root = repo_root();
    let spec = read(&root, "tools/atpkg-programs.spec");
    let active = active_names(&active_rows(&spec));
    let comments: String = spec
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for name in ROADMAP {
        if active.contains(name) {
            // Graduated: a packed, published row supersedes the note.
            continue;
        }
        assert!(
            mentions_token(&comments, name),
            "tools/atpkg-programs.spec no longer names {name:?} anywhere in \
             its notes and it is not an active row — the roadmap just lost a \
             system silently. Restore it under FUTURE MEMBERS (or \
             NOT-YET-PUBLISHABLE with its blocking condition), or graduate it \
             to a real row via the FUTURE MEMBERS runway"
        );
    }
}

/// (f) The root Cargo.toml's `[workspace.metadata.atpkg]` still ships ONE
/// command and the public index account. `expose` is the PATH surface every
/// install gets (one-binary collapse: everything else is an argv0 symlink);
/// `account` is the compiled-in default index owner behind
/// `crate::discovery::resolve_account` (env ATPKG_ACCOUNT > `[packages].account`
/// config > this) — it must stay the PUBLIC alabsystems org or a tokenless
/// fresh install can never reach an index (the `[workspace.package]`
/// repository owner is the private staging repo, which 404s anonymously).
#[test]
fn workspace_metadata_pins_the_shipped_surface_and_public_account() {
    let root = repo_root();
    let manifest: toml::Value = read(&root, "Cargo.toml")
        .parse()
        .expect("root Cargo.toml parses as TOML");
    let meta = manifest
        .get("workspace")
        .and_then(|w| w.get("metadata"))
        .and_then(|m| m.get("atpkg"))
        .unwrap_or_else(|| {
            panic!(
                "root Cargo.toml has no [workspace.metadata.atpkg] block — \
                 tools/atpkg-pack.sh falls back to its name-token guess and \
                 the compiled default index account falls back to the PRIVATE \
                 repository owner; restore the block (expose/bundle/account)"
            )
        });
    let expose: Vec<&str> = meta
        .get("expose")
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        expose,
        vec!["aterm"],
        "root Cargo.toml [workspace.metadata.atpkg] expose must be exactly \
         [\"aterm\"] — ONE command on PATH is the one-binary collapse's \
         contract (ctl/pkg/fleet/drive are in-process verbs; siblings ride as \
         argv0 symlinks). Restore `expose = [\"aterm\"]`"
    );
    let account = meta.get("account").and_then(toml::Value::as_str);
    assert_eq!(
        account,
        Some("alabsystems"),
        "root Cargo.toml [workspace.metadata.atpkg] account must pin the \
         PUBLIC org \"alabsystems\" — it is the compiled-in default index \
         account (stamped by crates/aterm-update-core/build.rs into \
         ATERM_ATPKG_INDEX_OWNER); without it the default falls back to the \
         private `repository` owner, which 404s for every tokenless install. \
         Repointing is a HOST change, never a trust change (same pinned root \
         key) — see the spec's ACCOUNT operator note before touching it"
    );
}

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
//! (atpkg-index.sh's `read -r name repo policy build group flags` loop and its
//! `spec_flags_toml`; the refresher's `PLAIN_PROGRAMS`/`RUSTC_PROGRAMS`/
//! `VENDOR_PROGRAMS` defaults and its literal `PROG=` bundle lane;
//! atpkg-publish-lib.sh's `ATPKG_VENDOR_HOSTS`). If a consumer's grammar moves,
//! this file must move in the same change — that forced co-review is the
//! point, so a shape this parser no longer finds is a hard failure, never a
//! silent skip.
//!
//! The VENDOR-FETCHED members (codex, claude, gh, emacs — `protocol =
//! "https"` rows, authored by tools/atpkg-author-vendor.sh) and the
//! OS-INSTALLED members (clt — `softwareupdate`; brew — `pkg`; the same
//! script) add three rules: their rows carry the owner-decided flags (`extra`
//! for the two agent CLIs, `system=<bin>` for gh and emacs, nothing for clt,
//! `system=brew,requires=clt` for brew), they are EXEMPT from the seed pack
//! lanes (never packed, never sealed — the refresher's `VENDOR_PROGRAMS` line
//! is the exemption and must never gain a lane), and the authoring side's host
//! allow-list must equal the client's (`crates/atpkg/src/vendor.rs`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The trust verifier tuple locked to the trust rustc fork — the spec's
/// `rustc` coherence group moves all-or-nothing (§7). One source of truth for
/// tests (c) and (d); the spec's COHERENCE GROUP note is the prose twin.
const RUSTC_TUPLE: [&str; 4] = ["trust", "trust-ir", "trust-cg", "trust-vc"];

/// The owner's decisions (2026-08-26) for the vendor-fetched members: the two
/// agent CLIs are EXTRAS — listed and pinned, installed only on request (the
/// typed-name consent stub) — and `gh`/`emacs` are default-set members that a
/// system install of the same name satisfies. Every one of these must be a
/// spec row (active or pending) carrying exactly these flags, and the set is
/// exactly the refresher's `VENDOR_PROGRAMS` exemption.
const VENDOR_EXTRAS: [&str; 2] = ["codex", "claude"];
const VENDOR_SYSTEM: [(&str, &str); 2] = [("gh", "gh"), ("emacs", "emacs")];

/// What Homebrew requires first: the Command Line Tools.
const BREW_REQUIRES: &[&str] = &["clt"];

/// The owner's direction (2026-08-27) for the OS-INSTALLED members, applied by
/// the OS's own installer with elevation: the Command Line Tools carry NO flag
/// (proven only by their own path — a system git never satisfies them), and
/// Homebrew is satisfied by a `brew` on PATH and REQUIRES the Command Line
/// Tools first (its pkg refuses to install without them). Same exemption from
/// the seed as the vendor-fetched members.
const OS_INSTALLED: [(&str, &[Flag]); 2] = [
    ("clt", &[]),
    (
        "brew",
        &[
            Flag::System(std::borrow::Cow::Borrowed("brew")),
            Flag::Requires(std::borrow::Cow::Borrowed(BREW_REQUIRES)),
        ],
    ),
];

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

/// One flag of the spec's optional 6th column, as atpkg-index.sh's
/// `spec_flags_toml` renders it into the signed `[programs.<name>]` block.
/// (`Cow`, so the owner-decision tables above can be `const`.)
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flag {
    /// `extra` -> `extra = true`: not a default-set member.
    Extra,
    /// `system=<bin>` -> `system = "<bin>"`: a PATH binary of that name satisfies it.
    System(std::borrow::Cow<'static, str>),
    /// `requires=<a>+<b>` -> `requires = ["a", "b"]`: installed before it.
    Requires(std::borrow::Cow<'static, [&'static str]>),
}

/// A bare program name, as atpkg-index.sh admits one in `requires=`/`system=`.
fn bare_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// One spec row, in atpkg-index.sh's own grammar:
/// `name repo policy build [coherence_group|-] [flags]`.
struct SpecRow {
    line_no: usize,
    name: String,
    policy: String,
    build: u64,
    group: Option<String>,
    flags: Vec<Flag>,
}

const POLICIES: [&str; 2] = ["prebuilt-only", "prebuilt-or-build"];

/// Parse ONE row's fields under the indexer's grammar, refusing every shape
/// the shell loop would silently tolerate or mis-sign: a wrong field count, a
/// flag token sitting in the group column (it would sign a coherence group
/// literally named `extra`), an unknown or duplicated flag, a `system=` value
/// that is not a bare executable name.
fn parse_row(fields: &[&str], line_no: usize, what: &str) -> SpecRow {
    assert!(
        (4..=6).contains(&fields.len()),
        "tools/atpkg-programs.spec:{line_no}: {what} row has {} fields, not \
         the documented `name repo policy build [coherence_group|-] [flags]` \
         — fix the row (atpkg-index.sh reads exactly those columns)",
        fields.len()
    );
    let build = fields[3].parse::<u64>().unwrap_or_else(|_| {
        panic!(
            "tools/atpkg-programs.spec:{line_no}: build column {:?} for {:?} \
             is not a number — refresh it from the PACK-SPEC line the pack \
             lane prints at publish time (spec BUILD COLUMN note)",
            fields[3], fields[0]
        )
    });
    let group = match fields.get(4) {
        Some(&"-") | None => None,
        Some(g) => {
            assert!(
                *g != "extra" && !g.starts_with("system=") && !g.starts_with("requires="),
                "tools/atpkg-programs.spec:{line_no}: {:?} has the flag {g:?} \
                 in the coherence_group column — the columns are positional; \
                 write `-` for the group first (atpkg-index.sh refuses this \
                 too, but only at publish time)",
                fields[0]
            );
            Some((*g).to_string())
        }
    };
    let mut flags = Vec::new();
    if let Some(col) = fields.get(5)
        && *col != "-"
    {
        for f in col.split(',') {
            let flag = if f == "extra" {
                Flag::Extra
            } else if let Some(bin) = f.strip_prefix("system=") {
                assert!(
                    !bin.is_empty()
                        && bin != "."
                        && bin != ".."
                        && bin
                            .bytes()
                            .all(|b| { b.is_ascii_alphanumeric() || b"._+-".contains(&b) }),
                    "tools/atpkg-programs.spec:{line_no}: {:?}: system=<bin> \
                     needs a bare executable name ([A-Za-z0-9._+-]), got {bin:?}",
                    fields[0]
                );
                Flag::System(std::borrow::Cow::Owned(bin.to_string()))
            } else if let Some(list) = f.strip_prefix("requires=") {
                let names: Vec<&'static str> = list
                    .split('+')
                    .map(|n| {
                        assert!(
                            bare_name_ok(n) && n != fields[0],
                            "tools/atpkg-programs.spec:{line_no}: {:?}: requires=<name> \
                             needs a bare program name other than itself \
                             ([A-Za-z0-9._-], + joins several), got {n:?}",
                            fields[0]
                        );
                        // Leaked on purpose: a handful of names per test process.
                        &*Box::leak(n.to_string().into_boxed_str())
                    })
                    .collect();
                Flag::Requires(std::borrow::Cow::Owned(names))
            } else {
                panic!(
                    "tools/atpkg-programs.spec:{line_no}: {:?}: unknown spec \
                     flag {f:?} (known: extra, system=<bin>, \
                     requires=<name>[+<name>]) — atpkg-index.sh refuses it; \
                     nothing may silently drop a token from the column the \
                     index is signed from",
                    fields[0]
                );
            };
            assert!(
                !flags.contains(&flag),
                "tools/atpkg-programs.spec:{line_no}: {:?}: flag {f:?} given twice",
                fields[0]
            );
            flags.push(flag);
        }
    }
    SpecRow {
        line_no,
        name: fields[0].to_string(),
        policy: fields[2].to_string(),
        build,
        group,
        flags,
    }
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
        let fields: Vec<&str> = line.split_whitespace().collect();
        rows.push(parse_row(&fields, idx + 1, "active"));
    }
    assert!(
        !rows.is_empty(),
        "tools/atpkg-programs.spec has no active rows — an empty table would \
         sign an index that pins nothing; restore the published program rows"
    );
    rows
}

/// The PENDING rows: a row commented with a BARE leading `#` (no space after
/// it — the spec header documents the shape) is a member the index does not
/// pin yet, kept grammatical so going live is deleting one byte and taking the
/// build from the PACK-SPEC line. A bare-`#` line that looks like a row
/// (a known policy in column 3, a number in column 4) is parsed under the
/// FULL grammar, so a pending row cannot rot into something the indexer would
/// refuse — or worse, mis-sign — on the day it is uncommented.
fn pending_rows(spec: &str) -> Vec<SpecRow> {
    let mut rows = Vec::new();
    for (idx, raw) in spec.lines().enumerate() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        if rest.starts_with(' ') || rest.starts_with('#') || rest.starts_with('!') {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let looks_like_row = fields.len() >= 4
            && POLICIES.contains(&fields[2])
            && fields[3].bytes().all(|b| b.is_ascii_digit());
        if !looks_like_row {
            continue;
        }
        rows.push(parse_row(&fields, idx + 1, "pending (commented)"));
    }
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

/// The refresher's `VENDOR_PROGRAMS="…"` line: the members it NEVER packs
/// and NEVER seals (their bytes are the vendor's; the DMG must not carry
/// them). It is the seed exemption for test (d) and is pinned to the owner
/// decisions by test (g).
fn seed_vendor_exemption(script: &str) -> BTreeSet<String> {
    const PREFIX: &str = "VENDOR_PROGRAMS=\"";
    let mut out = None;
    for raw in script.lines() {
        let line = raw.trim_start();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(PREFIX) {
            let list = rest.split('"').next().unwrap_or("");
            out = Some(list.split_whitespace().map(str::to_string).collect());
        }
    }
    out.unwrap_or_else(|| {
        panic!(
            "tools/atpkg-refresh-seed.sh no longer carries the `{PREFIX}…\"` \
             line this test parses — it is the https-protocol seed EXEMPTION \
             (never packed, never sealed); restore it or update \
             seed_vendor_exemption() in the same change"
        )
    })
}

/// A shell `NAME="a b c"` list on a non-comment line of `script`.
fn shell_list(script: &str, name: &str, rel: &str) -> BTreeSet<String> {
    let prefix = format!("{name}=\"");
    script
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .map(|rest| {
            rest.split('"')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| panic!("{rel} no longer carries a `{prefix}…\"` line this test parses"))
}

/// The quoted members of `pub const NAME: &[&str] = &[ … ];` in a Rust source.
fn rust_str_slice_const(src: &str, name: &str, rel: &str) -> BTreeSet<String> {
    let start = src
        .find(&format!("pub const {name}: &[&str] = &["))
        .unwrap_or_else(|| panic!("{rel} no longer declares `pub const {name}: &[&str] = &[…]`"));
    let body = &src[start..];
    let end = body.find("];").expect("the slice literal closes with `];`");
    let body = &body[..end];
    let mut out = BTreeSet::new();
    let mut rest = body;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let close = after.find('"').expect("a quoted string closes");
        out.insert(after[..close].to_string());
        rest = &after[close + 1..];
    }
    out
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
            POLICIES.contains(&row.policy.as_str()),
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
    let refresher = read(&root, "tools/atpkg-refresh-seed.sh");
    let seed = seed_pack_lanes(&refresher);
    let vendor = seed_vendor_exemption(&refresher);
    // A vendor-fetched member is EXEMPT: never packed, never sealed — the
    // client fetches its bytes from the vendor. The exemption is a list the
    // refresher carries on purpose, so an unpacked non-vendor row still fails.
    let unpacked: Vec<&String> = spec
        .difference(&seed)
        .filter(|n| !vendor.contains(*n))
        .collect();
    assert!(
        unpacked.is_empty(),
        "tools/atpkg-refresh-seed.sh has no pack lane for active spec \
         program(s) {unpacked:?} — every fresh install's seed would omit them \
         while the index pins them. Add each to the script's PLAIN_PROGRAMS \
         default (or RUSTC_PROGRAMS for a trust-tuple member; or \
         VENDOR_PROGRAMS if it is a vendor-fetched member that must never be \
         sealed), or remove the row from tools/atpkg-programs.spec if it was \
         never published"
    );
    let sealed_vendor: Vec<&String> = vendor.intersection(&seed).collect();
    assert!(
        sealed_vendor.is_empty(),
        "tools/atpkg-refresh-seed.sh packs vendor-fetched program(s) \
         {sealed_vendor:?} — their bytes are the vendor's and must NEVER be \
         sealed into the DMG (Claude Code's license forbids redistribution; \
         the rest are not ours to ship). Drop the lane; VENDOR_PROGRAMS is \
         the exemption, not a pack set"
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
    let manifest: aterm_toml::Value = read(&root, "Cargo.toml")
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
        .and_then(aterm_toml::Value::as_array)
        .map(|a| a.iter().filter_map(aterm_toml::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        expose,
        vec!["aterm"],
        "root Cargo.toml [workspace.metadata.atpkg] expose must be exactly \
         [\"aterm\"] — ONE command on PATH is the one-binary collapse's \
         contract (ctl/pkg/fleet/drive are in-process verbs; siblings ride as \
         argv0 symlinks). Restore `expose = [\"aterm\"]`"
    );
    let account = meta.get("account").and_then(aterm_toml::Value::as_str);
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

/// (g) The vendor-fetched members carry the OWNER'S decisions, in the table:
/// codex and claude are extras (`extra`), gh and emacs are system-satisfied
/// default-set members (`system=<bin>`); each is an active or a PENDING row
/// (the FUTURE MEMBERS rule keeps it commented until a signed pkg exists),
/// parsed under the full grammar so the day it goes live is a one-byte edit;
/// and the refresher's `VENDOR_PROGRAMS` exemption is exactly this set — an
/// exemption wider than the decisions would let an ordinary program skip the
/// seed, one narrower would seal a vendor's bytes.
#[test]
fn vendor_members_carry_the_owner_decisions_and_the_seed_exemption() {
    let root = repo_root();
    let spec = read(&root, "tools/atpkg-programs.spec");
    let mut rows = active_rows(&spec);
    rows.extend(pending_rows(&spec));
    let find = |name: &str| -> &SpecRow {
        rows.iter().find(|r| r.name == name).unwrap_or_else(|| {
            panic!(
                "tools/atpkg-programs.spec has no row (active or `#`-pending) \
                 for the vendor-fetched member {name:?} — restore it under \
                 the PENDING rows at the foot of the table (spec \
                 VENDOR-FETCHED MEMBERS note)"
            )
        })
    };
    for name in VENDOR_EXTRAS {
        let row = find(name);
        assert_eq!(
            row.flags,
            vec![Flag::Extra],
            "tools/atpkg-programs.spec:{}: {name:?} must carry exactly the \
             `extra` flag (owner decision: listed + pinned, installed only on \
             request through the typed-name consent stub)",
            row.line_no
        );
        assert_eq!(
            row.repo(&spec),
            name,
            "tools/atpkg-programs.spec:{}: {name:?} is a manifest-only \
             release host named after the program",
            row.line_no
        );
    }
    for (name, bin) in VENDOR_SYSTEM {
        let row = find(name);
        assert_eq!(
            row.flags,
            vec![Flag::System(std::borrow::Cow::Borrowed(bin))],
            "tools/atpkg-programs.spec:{}: {name:?} must carry exactly \
             `system={bin}` (owner decision: a system install on PATH \
             satisfies it; vendor-fetched otherwise)",
            row.line_no
        );
    }
    for (name, flags) in OS_INSTALLED {
        let row = find(name);
        assert_eq!(
            row.flags,
            flags.to_vec(),
            "tools/atpkg-programs.spec:{}: {name:?} must carry exactly {flags:?} \
             (owner direction 2026-08-27: clt is proven only by its own path; \
             brew is satisfied by a system brew and requires clt first)",
            row.line_no
        );
    }
    // brew requires clt, so clt must be a row (active or pending) whenever brew is:
    // an index pinning brew without clt would make every brew install wait on a
    // requirement the index cannot name.
    let brew_line = find("brew").line_no;
    let clt_line = find("clt").line_no;
    assert!(
        clt_line < brew_line,
        "tools/atpkg-programs.spec: the clt row (line {clt_line}) must precede the brew \
         row (line {brew_line}) — brew requires clt, and the spec reads top to bottom"
    );
    let decided: BTreeSet<String> = VENDOR_EXTRAS
        .iter()
        .copied()
        .chain(VENDOR_SYSTEM.iter().map(|(n, _)| *n))
        .chain(OS_INSTALLED.iter().map(|(n, _)| *n))
        .map(str::to_string)
        .collect();
    let exemption = seed_vendor_exemption(&read(&root, "tools/atpkg-refresh-seed.sh"));
    assert_eq!(
        exemption, decided,
        "tools/atpkg-refresh-seed.sh VENDOR_PROGRAMS must be exactly the \
         vendor-fetched and OS-installed members this test pins ({decided:?}) — \
         wider lets an ordinary program skip the seed, narrower seals a vendor's \
         bytes"
    );
    // The pending rows are exactly the not-yet-published vendor and OS-installed
    // members: a pending row for anything else has no authoring lane that prints a
    // PACK-SPEC line for it.
    for row in pending_rows(&spec) {
        assert!(
            decided.contains(&row.name),
            "tools/atpkg-programs.spec:{}: pending row for {:?}, which is not \
             a vendor-fetched or OS-installed member — only \
             tools/atpkg-author-vendor.sh's programs are staged as `#`-pending \
             rows; land anything else via the FUTURE MEMBERS runway",
            row.line_no,
            row.name
        );
    }
    // Every `requires=` names a program the spec carries (active or pending): the
    // index would otherwise pin a member requiring a name it cannot resolve.
    let names: BTreeSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    for row in &rows {
        for flag in &row.flags {
            if let Flag::Requires(deps) = flag {
                for dep in deps.iter() {
                    assert!(
                        names.contains(dep),
                        "tools/atpkg-programs.spec:{}: {:?} requires {dep:?}, which is not \
                         a row of this spec",
                        row.line_no,
                        row.name
                    );
                }
            }
        }
    }
    // And the relation is acyclic — over programs, and THROUGH a coherence group (the
    // client refuses either at index parse, `manifest::validate_requires`, so a cycle
    // here would sign an index every client throws away).
    let requires_of = |name: &str| -> Vec<&str> {
        rows.iter()
            .filter(|r| r.name == name)
            .flat_map(|r| r.flags.iter())
            .filter_map(|f| match f {
                Flag::Requires(deps) => Some(deps.iter().copied().collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect()
    };
    let group_of = |name: &str| -> Option<String> {
        rows.iter()
            .find(|r| r.name == name)
            .and_then(|r| r.group.clone())
    };
    for row in &rows {
        // Walk every path from the row; a return to the row, or to a member of the
        // row's group after leaving it, is a cycle.
        let mut stack: Vec<(String, Vec<String>, bool)> =
            vec![(row.name.clone(), vec![row.name.clone()], false)];
        let mut seen: BTreeSet<(String, bool)> = BTreeSet::new();
        while let Some((node, path, left)) = stack.pop() {
            if !seen.insert((node.clone(), left)) {
                continue;
            }
            for dep in requires_of(&node) {
                let same_group = group_of(dep).is_some() && group_of(dep) == group_of(&row.name);
                let back = dep == row.name || (same_group && left);
                assert!(
                    !back,
                    "tools/atpkg-programs.spec:{}: requires cycle {} → {dep} (the client \
                     refuses an index carrying one)",
                    row.line_no,
                    path.join(" → ")
                );
                let mut next = path.clone();
                next.push(dep.to_string());
                let leaves = !(same_group || dep == row.name);
                stack.push((dep.to_string(), next, left || leaves));
            }
        }
    }
}

/// (i) The authoring script's program roster equals the members this test pins:
/// `tools/atpkg-author-vendor.sh <name>` must exist for every pending vendor or
/// OS-installed row (it is the only lane that prints their PACK-SPEC line), and
/// must not offer a program the spec never mentions.
#[test]
fn the_authoring_script_serves_exactly_the_vendor_and_os_installed_members() {
    let root = repo_root();
    let script = read(&root, "tools/atpkg-author-vendor.sh");
    let line = script
        .lines()
        .find(|l| l.trim_start().starts_with("case \"$PROG\" in"))
        .and_then(|_| {
            script
                .lines()
                .skip_while(|l| !l.trim_start().starts_with("case \"$PROG\" in"))
                .nth(1)
        })
        .expect("tools/atpkg-author-vendor.sh dispatches on \"$PROG\"");
    let pattern = line.trim().split(')').next().unwrap_or("");
    let offered: BTreeSet<String> = pattern.split('|').map(str::to_string).collect();
    let decided: BTreeSet<String> = VENDOR_EXTRAS
        .iter()
        .copied()
        .chain(VENDOR_SYSTEM.iter().map(|(n, _)| *n))
        .chain(OS_INSTALLED.iter().map(|(n, _)| *n))
        .map(str::to_string)
        .collect();
    assert_eq!(
        offered, decided,
        "tools/atpkg-author-vendor.sh's program roster (the first `case \"$PROG\"` \
         arm) must be exactly the vendor-fetched + OS-installed members"
    );
}

impl SpecRow {
    /// The row's `repo` column, re-read from the spec line (the struct keeps
    /// the columns the index signs from; repo is asserted only for the
    /// vendor rows, whose repo is a manifest-only release host).
    fn repo(&self, spec: &str) -> String {
        let line = spec.lines().nth(self.line_no - 1).expect("line exists");
        let line = line.trim().trim_start_matches('#');
        line.split_whitespace().nth(1).unwrap_or("").to_string()
    }
}

/// (h3) The AUTHORING side's `shim_env` rule equals the CLIENT's (design S7): the
/// entry cap (`ATPKG_SHIM_ENV_MAX` = `shim_env::MAX_SHIM_ENV`), the entry length
/// (`ATPKG_SHIM_ENV_ENTRY_MAX` = `shim_env::MAX_ENTRY_BYTES`) and the names a shim
/// never sets (`ATPKG_SHIM_ENV_NEVER` = `shim_env::NEVER_SET`,
/// `ATPKG_SHIM_ENV_NEVER_PREFIXES` = `shim_env::NEVER_SET_PREFIXES`). The client's
/// rule is the authority — a manifest breaking it is refused whole at parse;
/// atpkg-publish-lib.sh's copy only lets the ceremony refuse the list before it is
/// signed.
#[test]
fn shim_env_rule_matches_the_client() {
    let root = repo_root();
    let lib = read(&root, "tools/atpkg-publish-lib.sh");
    let max = lib
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix("ATPKG_SHIM_ENV_MAX="))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .expect("tools/atpkg-publish-lib.sh carries an `ATPKG_SHIM_ENV_MAX=<n>` line");
    assert_eq!(
        max,
        atpkg::shim_env::MAX_SHIM_ENV,
        "tools/atpkg-publish-lib.sh ATPKG_SHIM_ENV_MAX must equal \
         crates/atpkg/src/shim_env.rs MAX_SHIM_ENV"
    );
    let entry_max = lib
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix("ATPKG_SHIM_ENV_ENTRY_MAX="))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .expect("tools/atpkg-publish-lib.sh carries an `ATPKG_SHIM_ENV_ENTRY_MAX=<n>` line");
    assert_eq!(
        entry_max,
        atpkg::shim_env::MAX_ENTRY_BYTES,
        "tools/atpkg-publish-lib.sh ATPKG_SHIM_ENV_ENTRY_MAX must equal \
         crates/atpkg/src/shim_env.rs MAX_ENTRY_BYTES"
    );
    let never = shell_list(&lib, "ATPKG_SHIM_ENV_NEVER", "tools/atpkg-publish-lib.sh");
    let client: BTreeSet<String> = atpkg::shim_env::NEVER_SET
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    assert_eq!(
        never, client,
        "tools/atpkg-publish-lib.sh ATPKG_SHIM_ENV_NEVER must equal shim_env::NEVER_SET"
    );
    let prefixes = shell_list(
        &lib,
        "ATPKG_SHIM_ENV_NEVER_PREFIXES",
        "tools/atpkg-publish-lib.sh",
    );
    let client_prefixes: BTreeSet<String> = atpkg::shim_env::NEVER_SET_PREFIXES
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    assert_eq!(
        prefixes, client_prefixes,
        "tools/atpkg-publish-lib.sh ATPKG_SHIM_ENV_NEVER_PREFIXES must equal \
         shim_env::NEVER_SET_PREFIXES"
    );
    // The authored claude row's entry is one the client admits, and the fix-line it
    // earns is the self-update one — the whole point of the key.
    let env = atpkg::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    assert_eq!(
        env.fix_line().as_deref(),
        Some("self-update off (DISABLE_AUTOUPDATER=1)")
    );
    assert!(
        read(&root, "tools/atpkg-author-vendor.sh").contains("SHIM_ENV=\"DISABLE_AUTOUPDATER=1\""),
        "tools/atpkg-author-vendor.sh authors claude with SHIM_ENV=DISABLE_AUTOUPDATER=1"
    );
}

/// (h2) The AUTHORING side's manager table equals the CLIENT's: the names
/// (`ATPKG_MANAGERS` = `vendor::MANAGERS`, the table's name column) and the
/// system-wide subset (`ATPKG_MANAGERS_ELEVATED` = every table row with
/// `elevated: true`). The client's table is the authority — a signed row naming
/// any other manager is refused before anything runs; atpkg-publish-lib.sh's
/// copy only lets the ceremony refuse the row before it is signed. Adding a
/// manager is one table row there, then one word here.
#[test]
fn manager_table_matches_the_client() {
    let root = repo_root();
    let lib = read(&root, "tools/atpkg-publish-lib.sh");
    let shell = shell_list(&lib, "ATPKG_MANAGERS", "tools/atpkg-publish-lib.sh");
    let shell_elevated = shell_list(
        &lib,
        "ATPKG_MANAGERS_ELEVATED",
        "tools/atpkg-publish-lib.sh",
    );
    let client: BTreeSet<String> = atpkg::MANAGERS.iter().map(|m| (*m).to_string()).collect();
    assert_eq!(
        shell, client,
        "tools/atpkg-publish-lib.sh ATPKG_MANAGERS must equal crates/atpkg/src/vendor.rs \
         MANAGERS — the client's table is the authority; update the shell copy in the \
         same change"
    );
    let client_elevated: BTreeSet<String> = atpkg::MANAGER_TABLE
        .iter()
        .filter(|m| m.elevated)
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(
        shell_elevated, client_elevated,
        "tools/atpkg-publish-lib.sh ATPKG_MANAGERS_ELEVATED must equal the client's \
         system-wide managers (MANAGER_TABLE rows with elevated: true)"
    );
    assert!(
        shell_elevated.is_subset(&shell),
        "every elevated manager is a manager"
    );
    // The shell copy of each manager's package-id charset (atpkg_package_id_ok) is
    // pinned by the tooling test against the ids the spec rows use; here, the names
    // the client refuses are the names the shell must refuse.
    for name in ["yum", "npm", "uv", "APT", ""] {
        assert!(
            !client.contains(name),
            "{name:?} is not a manager yet — if it just became one, the shell copy must \
             follow in the same change"
        );
    }
}

/// (h) The AUTHORING side's vendor host allow-list equals the CLIENT's. The
/// client's `vendor::VENDOR_HOSTS` is the authority (a signed row naming any
/// other host is refused before a byte moves); atpkg-publish-lib.sh carries
/// a copy so a bad row is refused before it is signed. Two lists that drift
/// either sign rows every client refuses, or let the ceremony think a host
/// is refused when clients would accept it.
#[test]
fn vendor_host_allow_list_matches_the_client() {
    let root = repo_root();
    let shell = shell_list(
        &read(&root, "tools/atpkg-publish-lib.sh"),
        "ATPKG_VENDOR_HOSTS",
        "tools/atpkg-publish-lib.sh",
    );
    let client = rust_str_slice_const(
        &read(&root, "crates/atpkg/src/vendor.rs"),
        "VENDOR_HOSTS",
        "crates/atpkg/src/vendor.rs",
    );
    assert!(
        !client.is_empty(),
        "crates/atpkg/src/vendor.rs VENDOR_HOSTS is empty"
    );
    assert_eq!(
        shell, client,
        "tools/atpkg-publish-lib.sh ATPKG_VENDOR_HOSTS must equal \
         crates/atpkg/src/vendor.rs VENDOR_HOSTS — the client's list is the \
         authority; update the shell copy in the same change"
    );
    for host in &client {
        assert!(
            !host.is_empty()
                && !host.contains('/')
                && !host.contains(':')
                && !host.contains('@')
                && host.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'.'
                    || b == b'-'),
            "VENDOR_HOSTS member {host:?} is not a bare lowercase host name"
        );
    }
}

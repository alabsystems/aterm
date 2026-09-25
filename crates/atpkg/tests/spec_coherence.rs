// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Spec<->index coherence for the atpkg publish lane, enforced.
//!
//! Every rule here used to be operator discipline alone — prose in
//! `tools/atpkg-programs.spec`'s header notes with no refusal anywhere. aterm
//! has no CI by owner decision (tools/verify.sh); the merge contract's Test
//! stage runs this file, so the discipline now fails a merge instead of a
//! fleet. The invariants span artifacts nothing else compares:
//!
//!   * `tools/atpkg-programs.spec` — the committed table of the LIVE published
//!     index's pins ("tracks the live index, never a wish"), and the program set
//!     every index lane reads;
//!   * the root `Cargo.toml`'s `[workspace.metadata.atpkg]` — the pack surface
//!     and the compiled-in default index account every shipped client resolves
//!     (`crate::discovery::resolve_account`, stamped by aterm-update-core's
//!     build.rs);
//!   * the authoring scripts' tables and the client's compiled ones they mirror.
//!
//! A divergence between any two publishes silently: a spec row without a
//! signed pack pins the unpublishable and wedges its whole coherence group on
//! every client (§7); a placeholder build column signs an index pinning build 0
//! with no refusal anywhere. Hermetic by construction: reads committed files via
//! `CARGO_MANIFEST_DIR` only — no network, no keys, no subprocess.
//!
//! The textual parses deliberately MIRROR the consumers' own readers
//! (atpkg-index.sh's `read -r name repo policy build group flags` loop and its
//! `spec_flags_toml`; atpkg-publish-lib.sh's `ATPKG_VENDOR_HOSTS`). If a
//! consumer's grammar moves, this file must move in the same change — that
//! forced co-review is the point, so a shape this parser no longer finds is a
//! hard failure, never a silent skip.
//!
//! The VENDOR-FETCHED members (codex, claude — followed on their vendors' own channels,
//! `crates/atpkg/src/vendor_direct`) add three rules: their rows carry no flag, each
//! stays pinned at a legacy index build no newer than the client's compiled legacy
//! ceiling — the pin is a floor nothing re-authors (owner ruling 2026-09-23) — and the
//! publish lane's host allow-list must equal the client's (`crates/atpkg/src/vendor.rs`).
//! (The `-` build and the `vendor-direct` flag, which listed a program without pinning
//! it, went on 2026-09-23; the pending gh/emacs rows and the OS-installed clt/brew rows,
//! with the `extra` and `requires=` flags, went on 2026-09-24 — design 2026-09-22
//! §5.3(b)/(c) — and so did the seed refresher whose `VENDOR_PROGRAMS` line exempted the
//! vendor-fetched members from the seed pack lanes. With both, the authoring ceremony
//! tools/atpkg-author-vendor.sh has no member left to author.)

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The trust verifier tuple locked to the trust rustc fork — the spec's
/// `rustc` coherence group moves all-or-nothing (§7). The spec's COHERENCE GROUP
/// note is the prose twin.
const RUSTC_TUPLE: [&str; 4] = ["trust", "trust-ir", "trust-cg", "trust-vc"];

/// The vendor-fetched members: the two agent CLIs, DEFAULT-SET members (2026-09-10 —
/// aterm is their version manager), carrying NO flag. Each must be a spec row, and the
/// set is exactly the client's compiled roster (`atpkg::stub::AGENT_PROGRAMS`).
const VENDOR_AGENTS: [&str; 2] = ["codex", "claude"];

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
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flag {
    /// `system=<bin>` -> `system = "<bin>"`: a PATH binary of that name satisfies it.
    System(String),
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
/// literally named `system=<bin>`), an unknown or duplicated flag, a `system=`
/// value that is not a bare executable name, a build that is not a number.
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
                !g.starts_with("system="),
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
            let flag = if let Some(bin) = f.strip_prefix("system=") {
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
                Flag::System(bin.to_string())
            } else {
                panic!(
                    "tools/atpkg-programs.spec:{line_no}: {:?}: unknown spec \
                     flag {f:?} (known: system=<bin>; extra, requires= and vendor-direct \
                     are retired) — atpkg-index.sh refuses it; nothing may silently drop a \
                     token from the column the index is signed from",
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
         row(s)"
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
/// `crate::discovery::resolve_account` (a development build's `[packages].account`
/// config > this; a shipped binary reads this alone) — it must stay the PUBLIC alabsystems org or a tokenless
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

/// (g) The vendor-fetched members carry the OWNER'S decisions, in the table: codex
/// and claude are default-set rows carrying no flag, riding the index repo.
#[test]
fn vendor_members_carry_the_owner_decisions() {
    let root = repo_root();
    let spec = read(&root, "tools/atpkg-programs.spec");
    let rows = active_rows(&spec);
    let find = |name: &str| -> &SpecRow {
        rows.iter().find(|r| r.name == name).unwrap_or_else(|| {
            panic!(
                "tools/atpkg-programs.spec has no row for the vendor-fetched member \
                 {name:?} — restore it (spec VENDOR-FETCHED MEMBERS note)"
            )
        })
    };
    let mut agents: Vec<&str> = VENDOR_AGENTS.to_vec();
    agents.sort_unstable();
    let mut compiled: Vec<&str> = atpkg::stub::AGENT_PROGRAMS.to_vec();
    compiled.sort_unstable();
    assert_eq!(
        agents, compiled,
        "the spec's agent set and the client's compiled AGENT_PROGRAMS roster must agree"
    );
    for name in VENDOR_AGENTS {
        let row = find(name);
        assert_eq!(
            row.flags,
            vec![],
            "tools/atpkg-programs.spec:{}: {name:?} must carry NO flag (owner decision \
             2026-09-10: a default-set member — aterm is its version manager — never \
             `extra`)",
            row.line_no
        );
        // Owner direction 2026-09-08: the manifest-only releases of a vendor member
        // ride the INDEX repo. A repo named after the vendor (`alabsystems/claude`)
        // would be a Claude Code distribution channel in everything but bytes.
        assert_eq!(
            row.repo(&spec),
            "aterm",
            "tools/atpkg-programs.spec:{}: {name:?}'s manifest-only releases \
             ride the index repo (`aterm`), never a repo named after the vendor \
             (owner direction 2026-09-08 — we host a signed pointer, not the \
             program)",
            row.line_no
        );
    }
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

/// Why [`vendor_direct_programs_stay_pinned_as_floors`] refuses `rows`: one line per
/// vendor-direct program with no active row, with a row whose build is not a legacy index
/// build, or with a legacy build above its compiled legacy ceiling.
fn vendor_direct_pin_problems(rows: &[SpecRow]) -> Vec<String> {
    let mut problems = Vec::new();
    for vendor in atpkg::vendor_direct::VENDORS {
        match rows.iter().find(|r| r.name == vendor.program) {
            None => problems.push(format!(
                "no active row for the vendor-direct program {:?} — its pin stays, as a \
                 floor; retiring it is ALLOW_OMIT on a publish, never a deleted row",
                vendor.program
            )),
            Some(row) if atpkg::vendor_direct::is_vendor_build(row.build) => {
                problems.push(format!(
                    "tools/atpkg-programs.spec:{}: {} pins {}, a vendor-direct build id — \
                     the index is never a byte source for one; the row keeps the legacy \
                     index build the live index pins",
                    row.line_no, row.name, row.build
                ));
            }
            Some(row) if row.build > vendor.legacy_ceiling => {
                problems.push(format!(
                    "tools/atpkg-programs.spec:{}: {} build {} is above its compiled legacy \
                     ceiling {} (crates/atpkg/src/vendor_direct/table.rs) — raise the ceiling, \
                     and the floors with it, only once that build's signed version is known \
                     to be at or below the floor",
                    row.line_no, row.name, row.build, vendor.legacy_ceiling
                ));
            }
            Some(_) => {}
        }
    }
    problems
}

/// THE VENDOR-DIRECT PROGRAMS STAY PINNED, AS FLOORS (owner ruling 2026-09-23). A client
/// carrying the vendor-direct lane never plans them from the index and holds the newer of
/// an installed pin and the vendor's head; an older client keeps installing the pin. So
/// each is an active row at a legacy index build: there is no `-` (listed, not pinned)
/// grammar any more — the parser refuses it as the indexer does — and a vendor build id
/// would ask the index to serve bytes it never carries.
///
/// THE LEGACY CEILING (design §1.3) is at or above every one of those pins: an unread
/// legacy build is replaced with no person asking only up to it, because every pin up to
/// it carries a version at or below the floor. A row above it is a legacy pin the compiled
/// ceiling never saw. Controls: a spec without the claude row, a claude row at a vendor
/// build id, a claude row one build above its ceiling, and a `-` build are each refused.
#[test]
fn vendor_direct_programs_stay_pinned_as_floors() {
    let root = repo_root();
    let rows = active_rows(&read(&root, "tools/atpkg-programs.spec"));
    let problems = vendor_direct_pin_problems(&rows);
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    let without_claude: Vec<SpecRow> = rows
        .iter()
        .filter(|r| r.name != "claude")
        .map(|r| {
            parse_row(
                &[&r.name, "aterm", &r.policy, &r.build.to_string()],
                r.line_no,
                "control",
            )
        })
        .collect();
    let problems = vendor_direct_pin_problems(&without_claude);
    assert!(
        problems.len() == 1 && problems[0].contains("\"claude\""),
        "a spec without the claude row must be refused by name: {problems:?}"
    );
    let vendor_id = atpkg::vendor_direct::VENDOR_BUILD_BASE + 2_000_001_000_280;
    let mut at_vendor_id = without_claude;
    at_vendor_id.push(parse_row(
        &["claude", "aterm", "prebuilt-only", &vendor_id.to_string()],
        1,
        "control",
    ));
    let problems = vendor_direct_pin_problems(&at_vendor_id);
    assert!(
        problems.len() == 1 && problems[0].contains("vendor-direct build id"),
        "a claude row at a vendor build id must be refused: {problems:?}"
    );
    let above = atpkg::vendor_direct::spec("claude")
        .expect("claude is a vendor-direct program")
        .legacy_ceiling
        + 1;
    at_vendor_id.pop();
    at_vendor_id.push(parse_row(
        &["claude", "aterm", "prebuilt-only", &above.to_string()],
        1,
        "control",
    ));
    let problems = vendor_direct_pin_problems(&at_vendor_id);
    assert!(
        problems.len() == 1 && problems[0].contains("above its compiled legacy ceiling"),
        "a claude row above its legacy ceiling must be refused: {problems:?}"
    );
    let dash = std::panic::catch_unwind(|| {
        parse_row(&["claude", "aterm", "prebuilt-only", "-"], 1, "control")
    });
    assert!(
        dash.is_err(),
        "a `-` build column must be refused, as the indexer refuses it"
    );
}

/// The body of one `name() { … }` shell function, from its opening line to the first line
/// that is exactly `}` — strict on purpose: these helpers never close a brace at column 0.
fn shell_fn_body(text: &str, name: &str) -> String {
    let head = format!("{name}() {{");
    let start = text
        .find(&head)
        .unwrap_or_else(|| panic!("no shell function {name}() in the file"));
    let rest = &text[start..];
    let end = rest
        .lines()
        .scan(0usize, |acc, l| {
            let at = *acc;
            *acc += l.len() + 1;
            Some((at, l))
        })
        .find(|(at, l)| *at > 0 && *l == "}")
        .map(|(at, _)| at)
        .unwrap_or_else(|| panic!("shell function {name}() is never closed at column 0"));
    rest[..end].to_string()
}

/// (i) The public mirror carries every target's pins. A signed index pins builds in two keys
/// — the platform-agnostic `pin` and the per-target `pin_by_target` overlay — and what
/// clients across all targets resolve is the union of the two (`manifest::tests`'s
/// `the_builds_clients_resolve_are_pin_union_every_overlay` is that law). A mirror whose work
/// list comes from `pin` alone silently skips a toolchain sealed on a triple that does not
/// own `pin` and so publishes entirely as an overlay: signed, publicly pinned, never
/// mirrored, with every public client on that triple asking for a release that does not
/// exist. No host sees it locally, so it is asserted from the source: one reader of those two
/// keys (atpkg-publish-lib.sh), producer and mirror both through it, no private scraper.
/// tools/test-atpkg-mirror-extras.sh is the behavioural twin (it runs the mirror).
#[test]
fn the_public_mirror_walks_the_per_target_pin_overlay() {
    let root = repo_root();
    let lib = read(&root, "tools/atpkg-publish-lib.sh");
    let mirror = read(&root, "tools/atpkg-mirror-public.sh");
    let indexer = read(&root, "tools/atpkg-index.sh");

    // The one grammar, and it really is a union: `pin` folded together with the
    // overlay. A union that dropped either half is the outage in both directions —
    // without the overlay the seal triple 404s, without `pin` every OTHER target does.
    for reader in [
        "atpkg_index_pin_entries",
        "atpkg_index_target_pin_entries",
        "atpkg_index_pin_union",
    ] {
        assert!(
            lib.contains(&format!("{reader}() {{")),
            "tools/atpkg-publish-lib.sh no longer defines {reader}() — the index's two \
             pin keys must have exactly one reader, shared by the producer and the mirror"
        );
    }
    let union = shell_fn_body(&lib, "atpkg_index_pin_union");
    for half in ["atpkg_index_pin_entries", "atpkg_index_target_pin_entries"] {
        assert!(
            union.contains(half),
            "atpkg_index_pin_union no longer folds in {half} — it is not a union, and a \
             publisher walking it would miss releases clients resolve"
        );
    }

    // The key the shell reads is the key the client parses. A rename on either side
    // that does not move both is an index whose overlay nothing finds.
    let manifest = read(&root, "crates/atpkg/src/manifest.rs");
    assert!(
        manifest.contains("pub pin_by_target: BTreeMap<String, BTreeMap<String, u64>>"),
        "crates/atpkg/src/manifest.rs no longer declares `pin_by_target` — if the overlay \
         key was renamed, tools/atpkg-publish-lib.sh's readers must be renamed with it"
    );
    assert!(
        shell_fn_body(&lib, "atpkg_index_target_pin_entries").contains("pin_by_target"),
        "atpkg_index_target_pin_entries must read the `pin_by_target` key by name"
    );

    // The mirror: its work list is that union and nothing narrower.
    assert!(
        mirror.contains("PINS=\"$(atpkg_index_pin_union "),
        "tools/atpkg-mirror-public.sh must derive PINS from atpkg_index_pin_union — the \
         public lane serves every target at once, so one target's pins are never its work \
         list"
    );
    // …and it keeps no private reader of either key. The mirror only ever reads an index,
    // so any `pin`-key text outside a comment is a scraper — the shape that missed the
    // overlay.
    for (n, line) in mirror.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        assert!(
            !t.contains("pin = {") && !t.contains("pin_by_target"),
            "tools/atpkg-mirror-public.sh:{}: reads a pin key itself ({t:?}) — go through \
             atpkg_index_pin_union / atpkg_index_target_pin_entries, or the two readers \
             drift and the mirror ships less than the producer signed",
            n + 1
        );
    }
    // A per-target overlay gives one program several pinned builds, so the mirror's staging
    // download dir must be keyed on the tag: keyed on the program, build B's download lands
    // on build A's files and every one of them reads as an "unverifiable extra".
    assert!(
        mirror.contains("\td=\"$WORK/$tag\"") && !mirror.contains("\td=\"$WORK/$prog\""),
        "tools/atpkg-mirror-public.sh must stage each package under $WORK/$tag: with a \
         per-target overlay one program has several pinned builds, and a per-PROGRAM dir \
         mixes them"
    );

    // The producer reads the same two keys through the same functions — the drift this
    // whole class is made of is two dialects of one grammar.
    for (func, reader) in [
        ("baseline_build", "atpkg_index_pin_entries"),
        ("baseline_target_entries", "atpkg_index_target_pin_entries"),
    ] {
        assert!(
            shell_fn_body(&indexer, func).contains(reader),
            "tools/atpkg-index.sh {func}() must read the baseline through \
             atpkg-publish-lib.sh's {reader}, not a private copy of the grammar"
        );
    }
}

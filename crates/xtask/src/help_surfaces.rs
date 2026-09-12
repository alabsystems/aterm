// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `xtask gate help-surfaces` — every CLI help surface in the workspace has been
//! READ against its handler, and a surface that appears or changes afterwards is
//! refused until it is read again.
//!
//! WHY THIS EXISTS. Between 2026-08-31 and 2026-09-10, 138 help/usage claims
//! across the alab repos were found to contradict the code that implements them
//! — rosters hand-typed beside derived sets, exit codes that no longer happen,
//! remedies naming verbs that do not do the thing (in this workspace: the ctl
//! catalog, atpkg's usage and doctor, the fleet/drive CLIs, aterm-dev, the
//! release cutter). Every one was found by reading a help surface against its
//! handler. That read is a dated fact about ONE version of the text, and nothing
//! made it durable: a new binary's usage text, or an edit to an existing one,
//! stayed unread until somebody thought to sweep again. This gate turns "all help
//! surfaces are read-verified" from a snapshot into an invariant of every green
//! `gate all` / `cargo test -p xtask`.
//!
//! WHAT A HELP SURFACE IS. A source file that carries user-facing usage/help
//! text, discovered mechanically: a string literal with a line that starts a
//! usage block (`Usage:`, `OPTIONS:`, `COMMANDS:`, `VERBS:`, …) or code that
//! exists only to print help (`fn usage`/`print_usage`/`help_text`/`cmd_help`, a
//! `USAGE`/`HELP`-segment const, a clap `#[command(`/`about =`/`long_about`).
//! [`NOT_HELP`] classifies what discovery over-finds (each with a reason), so
//! every discovered file is accounted for one way or the other.
//!
//! WHAT IS HASHED. The file's PROSE: every string literal plus every doc comment
//! (`///`, `//!`, `/** */`), in source order, with plain comments, char literals
//! and code dropped. A code-only edit leaves a verified row alone; any change to
//! text a user can read invalidates it.
//!
//! REFUSALS, each naming its remedy:
//!   R1 UNROSTERED  a discovered surface has no row in [`SURFACES`] or [`NOT_HELP`]
//!   R2 CHANGED     a surface's prose differs from the hash recorded when it was read
//!   R3 GONE        a rostered path no longer exists
//!   R4 STALE       a row's date is malformed or older than [`MAX_AGE_DAYS`]
//!   R5 DEAD_ALLOW  a [`NOT_HELP`] entry names a file that is gone or no longer discovered
//!   R6 DUPLICATE   a path appears twice
//!   R7 PROGRAM     a binary entry point (`src/main.rs`, `src/bin/*.rs`,
//!                  `src/bin/*/main.rs`, a `[[bin]] path`) under [`SCAN_ROOTS`] is in
//!                  neither list — every PROGRAM is accounted for, even one whose
//!                  help lives elsewhere (say where, in [`NOT_HELP`]) or that has none
//!
//! The remedy for R1/R2 is a READ, not a hash bump: the refusal line carries the
//! row to paste, dated today. Pasting it is the assertion that the text was read
//! against its handler on that date.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One read-verified surface: workspace-relative path, prose hash
/// ([`prose_hash`], FNV-1a 64 as 16 hex digits), the `YYYY-MM-DD` it was read,
/// and by what method.
pub(crate) type SurfaceRow<'a> = (&'a str, &'a str, &'a str, &'a str);
/// A file discovery over-finds: path and why it is not a help surface.
pub(crate) type NotHelpRow<'a> = (&'a str, &'a str);

/// A read older than this is stale: the text may still be byte-identical, but
/// the code under it has had half a year to move.
const MAX_AGE_DAYS: i64 = 180;
/// Where discovery walks (workspace-relative). Test, example and bench trees are
/// skipped by [`excluded_path`].
const SCAN_ROOTS: &[&str] = &["crates"];

/// The roster. Every entry is a dated assertion; see the module doc.
const SURFACES: &[SurfaceRow<'static>] = &[
    // HELP_SURFACES_ROSTER_BEGIN
    (
        "crates/aterm-agent/src/supervise/run.rs",
        "9c99932fd8fd0c48",
        "2026-09-11",
        "read against render_phase/render_prompt/render_result and the supervise loop as the handler of DRIVE_HELP's phase/await-turn/supervise sections by the 2026-09-10 round-3 reader; no findings; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-forge/src/budget.rs",
        "897726cf5623198d",
        "2026-09-12",
        "re-read in full against loc::package_dir/survey_cell, resolve::default_cells, policy::patch_entries and a live `cargo forge budget` run (28 rows GREEN) on 2026-09-12; two contradicted claims fixed in the commit that updated this row — ScopeDetail::vendored_measured still described the registry-before-vendor resolution order that 44324b41d reversed, and the module doc put 27 third-party build scripts in the macOS shipped graph against a measured 10; the seed/seed_from_live docs now name `unarmed` as what `--update` actually writes into an empty checkout",
    ),
    (
        "crates/aterm-forge/src/attest.rs",
        "0c5bfe1b75bda4ec",
        "2026-09-12",
        "re-read in full against report and ob1..ob10, provenance.rs, direct_vendor::review and a live `cargo forge attest` run (PASS: 5 vendored forks, 8 first-party patch targets, 1 roster row) on 2026-09-12; two contradicted claims fixed in the commit that updated this row — the [OB-1] direct-bundle line printed `It remains in forge's third-party totals` while provenance::is_first_party takes every vendor/astream crate OUT of them (measured: survey mac-arm reads 121 resolved, 74 workspace, 47 third-party, no astream row), and pristine_dir's doc still said loc::package_dir depends on the same missing pristine copy; the [OB-1] bullet and the What-this-does-NOT-do note now name the three vendor/ shapes and the roster that grants the first-party class",
    ),
    (
        "crates/aterm-agent/src/fleet_cli.rs",
        "e20833cc05228f40",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-agent/src/lib.rs",
        "c2e08bb9343652eb",
        "2026-09-11",
        "re-read against drive_cli.rs, supervise/run.rs and supervise/classify.rs by the 2026-09-10 round-3 reader after the round-2 merge changed DRIVE_HELP; seven findings (box shape, `OK skipped`, python globs, word count, the governor's refusal, two doc slips) fixed in the commit that updated this row; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-cli/src/lib.rs",
        "a0a478ffae64bb9c",
        "2026-09-10",
        "cursor-home integration: privacy text read against async checks and unanswered-card reoffer; corrected pending, refresh and notice wording",
    ),
    (
        "crates/aterm-cli/src/manual.rs",
        "d252491ae70315dd",
        "2026-09-11",
        "whole page set read against its handlers by aterm-help-surfaces-read on 2026-09-10; the fabric page rewrite (aterm link broker/mint, provisioning, eight grants, fabric attach argv form) read against aterm-link cli.rs and control.rs dispatch_fabric_verb by verify:F1/verify:F3 of aterm-fabric-attach-round-3 on 2026-09-10; merged page re-read by the orchestrator against those handlers and the pin tests on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-cli/src/windowing.rs",
        "fd8be119f5c354f8",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-control/src/selection.rs",
        "7269abf2d2722432",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-ctl/src/conn.rs",
        "90ea0edfcaad43d2",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-ctl/src/lib.rs",
        "d9e3b037e5cf0993",
        "2026-09-11",
        "re-read against control_query.rs cmd_cell, control_verbs.rs framing_of, exchange, discovery_targets, control_session.rs turn, control_input.rs take_leading_options and aterm-update-core http.rs by the 2026-09-10 round-3 reader after a8a895f0b; the `cell` reply shape, the response-framing paragraph, the synopsis' `--timeout`, the `$ATERM_CONTROL_SOCK` isolation claim, the dangling [`run`] link, `leading_options`' superset rule and the EXCHANGE_DEADLINE rationale fixed in the commit that updated this row; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-dev/src/main.rs",
        "e81d6e1a4ed79f2f",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-forge/src/cli.rs",
        "082ffd0b7dab9c05",
        "2026-09-10",
        "direct-source integration: read attest help against all ten fork obligations and the named direct-bundle inventory/metadata handler",
    ),
    (
        "crates/aterm-forge/src/lib.rs",
        "a0850f3a07d4c45f",
        "2026-09-10",
        "direct-source integration: re-read survey counts and ownership/verification wording against measured rows, resolver, and unchanged compiler flags",
    ),
    (
        "crates/aterm-gui/src/cli.rs",
        "16c0afe8d32a66ef",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/control.rs",
        "242dacef16ec40cc",
        "2026-09-11",
        "read in full against its dispatch by aterm-help-surfaces-read on 2026-09-10; the fabric attach/status and hold additions read against dispatch_fabric_verb / dispatch_hold_verb / the audit path by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/control_input.rs",
        "b2dbb205d461aece",
        "2026-09-11",
        "re-read against control.rs's cross-session `key` arms, post_input_reply and lib.rs's Wake::Input arm, input_if_row_matches, pty_idem KEYED_VERBS, cmd_scroll and parse_tab by the 2026-09-10 round-3 reader after the round-2 merge; the cross-session `key` usage (code, control.rs, pinned by a test), the `mouse` fire-and-forget claim, the `sole encoder caller` claim, GUARDED_VERBS' `no id=` claim, the `hello id=1` count, and the `scroll`/`tab` grammar lines fixed in the commit that updated this row; parse_tab's doc sentence still omits `close`/`move`; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/control_media.rs",
        "ea2c2d61bf5816bb",
        "2026-09-10",
        "cursor-home integration: changed prose read against merged fx, typed-turn/momentum and asynchronous privacy handlers; pending/baseline/coverage/interval claims corrected",
    ),
    (
        "crates/aterm-gui/src/control_privacy.rs",
        "eef3956e2f690501",
        "2026-09-11",
        "the row hashed a checkout behind 0a8275760/acc20c5fd/da5d75e33; re-read against parse_consent_timeout, covers_split and PrivacySnapshot::lines by the 2026-09-10 round-3 reader, the SERVICES doc (`never uncovered` vs NEVER_COVERED) fixed in 33311eda0; re-read against observer_fda_value/inert_fda_probe, ConsentState::fda -> ConsentCache::get_or_probe, app_settings.rs macos_access_projection and consent_policy by the round-3 fixer; the `live` field's `observer row's third value` claim (the row renders the inert labels as `off`, pinned at the fda=off/responsible=off asserts), the consent_panel_facts/session_consent `no syscall` claims (the cached probe's one open(TCC.db) on a miss), ConsentPanelFacts' `no covers= list` claim (the panel takes covers_split directly) and the consent_policy doc line displaced onto consent_probe_interval fixed in the commit that updated this row; the catalog's `covers=`/`uncovered=` split in control_verbs.rs still omits `unmeasured=` — that surface's own row; NOTE's doc now says its `covers is empty` clause is written for the UNMEASURED evidence read_privacy hard-codes and must turn evidence-conditional when §7 S4 lands (the measured arm of lines, tests-only today, renders a covers= list above it), the one-line fix for the round-3 reader's note finding; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/control_query.rs",
        "da9fd9cddb983f52",
        "2026-09-11",
        "the row predates ffda97813 (`text tail=`/`rows=`), which the round-2 merge carried; re-read against cmd_modes, cmd_cell, cmd_metrics/cmd_metrics_json by the 2026-09-10 round-3 reader and against text_args/TextShape::select/frame_rows_reply/cmd_text_opt by the round-3 fixer (that delta matches its code); the `modes` frame (`OK <n>` and twelve keys, not `OK` and seven — pinned by modes_frames_its_count_and_twelve_keys), the `cell` attrs vocabulary (`wide`/`wide_cont`, pinned by cell_attrs_carry_the_width_markers) and the JSON `percentiles` doc (it now names the reflow quartet the text form carries and the JSON body omits; the code gap stands) fixed in the commit that updated this row; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/control_session.rs",
        "d8f5af6b89891d55",
        "2026-09-10",
        "cursor-home integration: zero/low momentum floors and timeout precedence read against real snapping metric, watch parking and conformance tests",
    ),
    (
        "crates/aterm-gui/src/fabric.rs",
        "979242538c41a9cf",
        "2026-09-11",
        "module doc, HALT_EXEMPT and hold-origin docs read against fabric_launch::arm, dispatch_hold_verb and the inbox header by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; the rest as the 2026-09-10 sweep read it; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/hwkey.rs",
        "efa13637e047662f",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/menu.rs",
        "bad7bda51956a5f9",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/operator_host.rs",
        "f4682d40f4cdb0f4",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-A-ctl-verb-usages); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/pty_idem.rs",
        "4ac6ab2f17c9cd57",
        "2026-09-11",
        "read pty_idem.rs (module/Realm/PRODUCER_CAP/dup_reply/guarded/record_in_doubt docs, USAGE, KEYED_VERBS, test docs) against control.rs dispatch + run_feed_bin_routed, control_input.rs guarded press/take_leading_options, control_session.rs cmd_turn_guarded, control_verbs.rs catalog + framing_of, aterm-ctl stream_count/malformed header, aterm-link bridge feed key by workflow aterm-help-surfaces-read on 2026-09-10; 4 low findings left; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-link/src/cli.rs",
        "23bcc255bc86c2cc",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-link/src/hook.rs",
        "65672845d630a261",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-link/src/mirror.rs",
        "e0c1f9f7a10f2426",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-link/src/notify.rs",
        "887108b75779e9d3",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-link/src/tui.rs",
        "24f9b1b08fee982e",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-nest/src/main.rs",
        "50a498270b151279",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-primer/src/lib.rs",
        "00e6d1d59ce06e0c",
        "2026-09-11",
        "FABRIC_NOTE (hold=1 wording, both origins, the 4300-byte block budget) read against dispatch_hold_verb and the fabric_note pin test by verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; the rest as the 2026-09-10 sweep read it; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-release/src/cli.rs",
        "6bf90388f4a926d9",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-types/src/control_verbs.rs",
        "878599a5f0dce7da",
        "2026-09-12",
        "catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records",
    ),
    (
        "crates/aterm-verify/src/cli.rs",
        "59e85f8eaac4e19b",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-verify/src/lib.rs",
        "8c813af689c52829",
        "2026-09-11",
        "the row hashed a checkout behind 51b8f88c8; re-read against run/toolchain_header_line by the 2026-09-10 round-3 reader; run's doc block, displaced onto toolchain_header_line, put back at the row's previous update, and its `only the ladder` claim (run writes the toolchain header first) fixed in the commit that updated this row; the residual `nothing else` claim fixed by the round-3 fixer — run also writes the prelude rungs, pin_hooks' `hooks pinned:` note and the verdict or the gate-defect FAIL/COULD NOT RUN lines; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/atpkg-keys/src/main.rs",
        "a491ce402d28ccc7",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-D-keys-xtask-misc); findings fixed in the commit that added this row",
    ),
    (
        "crates/atpkg/src/cli.rs",
        "984872e6a3a6bd12",
        "2026-09-12",
        "read against its parser/dispatch by the 2026-09-10 sweep (findings fixed in 412cf3acd); the 2026-09-11 marker/announcement change re-read IN FULL on 2026-09-12 against mutator_store_lock's Contended arm, emit_marker_line, announcement::MARKERS/is_open, answer_announcement at the tail of cmd_seed/cmd_update_all/cmd_install_default_set, and aterm-gui's parse_seed_line (every one of the eleven markers is matched there); VERBS/VERB_USAGE/VERB_TIERS re-checked as a 23-verb partition of dispatch and the noindex grammar against parse_noindex; the rest of the file sampled rather than re-read. One contradicted claim fixed in the commit that updated this row — the marker block called every marker but SEED_STARTING a terminal, while NET_STARTING opens an announcement too (announcement::MARKERS pins exactly two openers, and aterm-gui's stale `nine markers` count was corrected with it)",
    ),
    (
        "crates/xtask/src/gate.rs",
        "ca0215e182b6ad14",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-D-keys-xtask-misc); findings fixed in the commit that added this row",
    ),
    (
        "crates/xtask/src/main.rs",
        "18b948623e8fc0aa",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-D-keys-xtask-misc); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-census/src/main.rs",
        "15f0c30a18130a47",
        "2026-09-10",
        "usage text written 2026-09-10 exact to its parser (root default `.`, five selection flags and aliases, last-wins, exit 0/1/2) in the program-entry pass; read against main() the same day",
    ),
    // HELP_SURFACES_ROSTER_END
];

/// Files discovery finds that carry no user-facing help text.
const NOT_HELP: &[NotHelpRow<'static>] = &[
    // HELP_SURFACES_NOT_HELP_BEGIN
    (
        "crates/xtask/src/help_surfaces.rs",
        "this gate's own module: its strings are the refusal lines and the roster rows; a row for it would change its own prose and refuse itself forever — the module is reviewed with the roster it carries",
    ),
    (
        "crates/aterm-gpu/src/metal/ffi.rs",
        "Metal Objective-C FFI bindings for a library crate (no fn main, no println!/eprintln!, no usage/help string). The USAGE hits are the MTLTextureUsage bitmask constants TEXTURE_USAGE_SHADER_READ / TEXTURE_USAGE_RENDER_TARGET / TEXTURE_USAGE_PIXEL_FORMAT_VIEW (lines 573-578) and the `setUsage:` / `usage` selectors (846-847, 1586).",
    ),
    (
        "crates/aterm-gui/src/accesskit_tree.rs",
        "Pure accessibility-tree projection (SettingsState/grid -> accesskit TreeUpdate). Its strings are screen-reader labels/descriptions and node-id docs, not CLI help or usage text; nothing here is printed to a terminal.",
    ),
    (
        "crates/aterm-http/src/verifier/apple.rs",
        "SecTrust TLS chain verifier for a library crate (no fn main, no printed text). The USAGE hit is the OSStatus constant ERR_SEC_INVALID_EXTENDED_KEY_USAGE = -67609 (line 107), mapped to an EKU rejection at line 534.",
    ),
    (
        "crates/aterm-http/src/verifier/windows.rs",
        "CryptoAPI/schannel TLS chain verifier for a library crate (no fn main, no printed text). The USAGE hits are the Win32 imports CERT_USAGE_MATCH / CTL_USAGE / USAGE_MATCH_TYPE_AND (lines 76-81, 228-230) and the HRESULT constant CERT_E_WRONG_USAGE (line 99); the module doc's mention of CERT_USAGE_MATCH (line 27) is an implementation note, not operator help.",
    ),
    (
        "crates/aterm-types/src/app_inspection.rs",
        "Wire-grammar parser for the `inspect app/v1` / `act app/v1` / `open app` control-socket verbs. Its ParseError::Usage strings are ERR-line payloads returned over the socket (surfaced via error.to_string() in aterm-gui/src/app_control.rs:114-139), not a --help surface. They were checked anyway and match parse_inspect/parse_act/parse_open_app exactly; no findings.",
    ),
    (
        "crates/aterm-agent/src/bin/aterm-drive.rs",
        "a one-line bin shim: its help is DRIVE_HELP in crates/aterm-agent/src/lib.rs (rostered)",
    ),
    (
        "crates/aterm-agent/src/bin/aterm-fleet.rs",
        "a bin shim: its help is crates/aterm-agent/src/fleet_cli.rs (rostered)",
    ),
    (
        "crates/aterm-ctl/src/main.rs",
        "a bin shim: its help is HELP_PROSE in crates/aterm-ctl/src/lib.rs (rostered)",
    ),
    (
        "crates/aterm-forge/src/main.rs",
        "a bin shim over crates/aterm-forge/src/cli.rs (rostered); its two own strings (--root unresolvable, no workspace Cargo.toml above cwd) read against canonical_root/discover_root 2026-09-10",
    ),
    (
        "crates/aterm-gui/src/bin/aterm-redraw-conformance.rs",
        "takes no arguments and reads no env; a conformance driver whose exit codes are owned by aterm_gui::control_redraw_conformance",
    ),
    (
        "crates/aterm-gui/src/main.rs",
        "a bin shim: the window's --help is HELP_HEAD in crates/aterm-gui/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-link/src/main.rs",
        "an argv0-symlink shim: its help is crates/aterm-link/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-release/src/main.rs",
        "a bin shim: its help is USAGE in crates/aterm-release/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-scrollback/fuzz/fuzz_targets/lz4_decompress.rs",
        "a libfuzzer target, not a program",
    ),
    (
        "crates/aterm-verify/src/main.rs",
        "a bin shim over crates/aterm-verify/src/cli.rs (rostered); its own root error names exactly the two files locate_root requires and the --root > ATERM_VERIFY_ROOT > walk precedence holds (read 2026-09-10)",
    ),
    (
        "crates/aterm/src/main.rs",
        "the front door delegates its --help to HELP_HEAD in crates/aterm-cli/src/lib.rs (rostered); its own printed strings (update status|check usage, the windowing warning, the reroute and staged-update notices, the never-checked line) were read against their code 2026-09-10 and one false platform claim fixed",
    ),
    (
        "crates/atpkg/src/main.rs",
        "a bin shim: its help is VERB_USAGE and cmd_help in crates/atpkg/src/cli.rs (rostered)",
    ),
    // HELP_SURFACES_NOT_HELP_END
];

/// `xtask gate help-surfaces`: the verdict, with the transcript on stderr.
pub(crate) fn gate_help_surfaces() -> bool {
    let (ok, log) = help_surfaces_report(&crate::workspace_root());
    eprint!("{log}");
    ok
}

/// The verb over an arbitrary root, returning the verdict plus the transcript.
pub(crate) fn help_surfaces_report(root: &Path) -> (bool, String) {
    let refusals = help_surfaces_check(
        root,
        SCAN_ROOTS,
        SURFACES,
        NOT_HELP,
        MAX_AGE_DAYS,
        today_days(),
    );
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== gate help-surfaces (every CLI help surface read against its handler) ==="
    );
    if refusals.is_empty() {
        let _ = writeln!(
            log,
            "gate help-surfaces: GREEN — {} surface(s) read-verified, {} classified not-help, \
             max age {MAX_AGE_DAYS} days",
            SURFACES.len(),
            NOT_HELP.len()
        );
        return (true, log);
    }
    for r in &refusals {
        let _ = writeln!(log, "gate help-surfaces: {r}");
    }
    let _ = writeln!(
        log,
        "gate help-surfaces: FAILED — {} refusal(s). Read the named text against its handler, \
         then record the row it prints in crates/xtask/src/help_surfaces.rs.",
        refusals.len()
    );
    (false, log)
}

/// The pure check: refusal lines (empty = green). `today` is days since the Unix
/// epoch, so a fixture can pin the calendar.
pub(crate) fn help_surfaces_check<'a>(
    root: &Path,
    scan_roots: &[&str],
    surfaces: &[SurfaceRow<'a>],
    not_help: &[NotHelpRow<'a>],
    max_age_days: i64,
    today: i64,
) -> Vec<String> {
    let mut out = Vec::new();
    let today_iso = civil_from_days(today);

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (p, ..) in surfaces {
        *seen.entry(p).or_insert(0) += 1;
    }
    for (p, _) in not_help {
        *seen.entry(p).or_insert(0) += 1;
    }
    for (p, k) in &seen {
        if *k > 1 {
            out.push(format!(
                "R6 DUPLICATE  {p} appears {k} times across SURFACES/NOT_HELP — keep one row"
            ));
        }
    }

    let discovered = discover(root, scan_roots);
    let rostered: BTreeMap<&str, &SurfaceRow<'a>> = surfaces.iter().map(|r| (r.0, r)).collect();
    let allowed: BTreeMap<&str, &str> = not_help.iter().copied().collect();

    for (rel, reasons) in &discovered {
        if rostered.contains_key(rel.as_str()) || allowed.contains_key(rel.as_str()) {
            continue;
        }
        let hash = read_prose_hash(&root.join(rel)).unwrap_or_default();
        out.push(format!(
            "R1 UNROSTERED {rel} carries help text ({}) and no row says it was read — read it \
             against its handler, then add: (\"{rel}\", \"{hash}\", \"{today_iso}\", \"<who read it against what>\"),",
            reasons.join(", ")
        ));
    }

    for (rel, hash, verified, method) in surfaces {
        let path = root.join(rel);
        if !path.is_file() {
            out.push(format!(
                "R3 GONE       {rel} is rostered but not on disk — drop the row or fix the path"
            ));
            continue;
        }
        let current = read_prose_hash(&path).unwrap_or_default();
        if current != *hash {
            out.push(format!(
                "R2 CHANGED    {rel}: help/doc text changed since it was read on {verified} — re-read \
                 it against its handler, then replace the row with: (\"{rel}\", \"{current}\", \
                 \"{today_iso}\", \"{method}\"),"
            ));
        }
        match days_from_iso(verified) {
            None => out.push(format!(
                "R4 STALE      {rel}: verified date {verified:?} is not YYYY-MM-DD"
            )),
            Some(d) if today - d > max_age_days => out.push(format!(
                "R4 STALE      {rel}: read {} days ago (limit {max_age_days}) — re-read it and re-date the row",
                today - d
            )),
            Some(_) => {}
        }
    }

    let programs = program_entry_points(root, scan_roots);
    for (rel, _) in not_help {
        if !root.join(rel).is_file() {
            out.push(format!(
                "R5 DEAD_ALLOW {rel} is in NOT_HELP but not on disk — prune it"
            ));
        } else if !discovered.contains_key(*rel) && !programs.contains(*rel) {
            out.push(format!(
                "R5 DEAD_ALLOW {rel} is in NOT_HELP but discovery no longer finds it — prune it"
            ));
        }
    }
    for rel in &programs {
        if rostered.contains_key(rel.as_str()) || allowed.contains_key(rel.as_str()) {
            continue;
        }
        let hash = read_prose_hash(&root.join(rel)).unwrap_or_default();
        out.push(format!(
            "R7 PROGRAM    {rel} is a binary entry point in neither list — read its help against \
             its parser and add: (\"{rel}\", \"{hash}\", \"{today_iso}\", \"<who read it against \
             what>\"), or record in NOT_HELP where its help lives (or that it takes no arguments)"
        ));
    }
    out
}

/// Every binary entry point under `scan_roots`: the conventional `src/main.rs`,
/// `src/bin/*.rs` and `src/bin/*/main.rs` of each Cargo package, plus any explicit
/// `[[bin]] path = "…"`. Test, example, bench and target trees are skipped as
/// everywhere else, and so is `vendor/`.
fn program_entry_points(root: &Path, scan_roots: &[&str]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut manifests = Vec::new();
    for sr in scan_roots {
        collect_named(&root.join(sr), "Cargo.toml", &mut manifests);
    }
    manifests.sort();
    for toml in manifests {
        let Some(dir) = toml.parent() else {
            continue;
        };
        let Ok(rel_dir) = dir.strip_prefix(root) else {
            continue;
        };
        let rel_dir = rel_dir.to_string_lossy().replace('\\', "/");
        if excluded_path(&format!("{rel_dir}/x.rs")) || format!("/{rel_dir}/").contains("/vendor/")
        {
            continue;
        }
        let mut candidates = vec![dir.join("src/main.rs")];
        let mut bins = Vec::new();
        collect_rs(&dir.join("src/bin"), &mut bins);
        bins.sort();
        for b in bins {
            let depth = b
                .strip_prefix(dir.join("src/bin"))
                .map_or(0, |p| p.components().count());
            if depth == 1 || (depth == 2 && b.file_name().is_some_and(|n| n == "main.rs")) {
                candidates.push(b);
            }
        }
        if let Ok(text) = std::fs::read_to_string(&toml) {
            let mut in_bin = false;
            for line in text.lines() {
                let l = line.trim();
                if l.starts_with('[') {
                    in_bin = l == "[[bin]]";
                    continue;
                }
                if in_bin
                    && let Some(rest) = l.strip_prefix("path")
                    && let Some(v) = rest.trim_start().strip_prefix('=')
                {
                    candidates.push(dir.join(v.trim().trim_matches('"')));
                }
            }
        }
        for c in candidates {
            if !c.is_file() {
                continue;
            }
            let Ok(rel) = c.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !excluded_path(&rel) {
                out.insert(rel);
            }
        }
    }
    out
}

fn collect_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_named(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

// ------------------------------------------------------------------ discovery

/// Workspace-relative path -> why it was discovered, for every `.rs` file under
/// `scan_roots` that carries help text.
fn discover(root: &Path, scan_roots: &[&str]) -> BTreeMap<String, Vec<&'static str>> {
    let mut found = BTreeMap::new();
    for sr in scan_roots {
        let mut files = Vec::new();
        collect_rs(&root.join(sr), &mut files);
        files.sort();
        for f in files {
            let Ok(rel) = f.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if excluded_path(&rel) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let reasons = discovery_reasons(&src);
            if !reasons.is_empty() {
                found.insert(rel, reasons);
            }
        }
    }
    found
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Test, example and bench trees are not shipped surfaces.
fn excluded_path(rel: &str) -> bool {
    let s = format!("/{rel}");
    ["/examples/", "/benches/", "/tests/", "/target/"]
        .iter()
        .any(|d| s.contains(d))
        || s.ends_with("_test.rs")
        || s.ends_with("_tests.rs")
        || s.ends_with("/tests.rs")
        || s.ends_with("/kani_proofs.rs")
}

fn discovery_reasons(src: &str) -> Vec<&'static str> {
    let (strings, _docs) = prose_of_rust(src);
    let mut reasons = Vec::new();
    if strings.iter().any(|s| has_usage_block_line(s)) {
        reasons.push("usage-string");
    }
    if let Some(r) = help_code_marker(src) {
        reasons.push(r);
    }
    reasons
}

/// A literal with a line that starts a usage block: `usage:`, `OPTIONS:`, ….
fn has_usage_block_line(literal: &str) -> bool {
    const KEYS: &[&str] = &[
        "usage",
        "options",
        "subcommands",
        "commands",
        "arguments",
        "verbs",
        "flags",
        "exit codes",
        "exit code",
    ];
    literal.lines().any(|line| {
        let l = line.trim_start().to_ascii_lowercase();
        KEYS.iter().any(|k| {
            l.strip_prefix(k)
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        })
    })
}

/// Code that exists only to print help.
fn help_code_marker(src: &str) -> Option<&'static str> {
    const FNS: &[&str] = &[
        "fn usage",
        "fn print_usage",
        "fn help_text",
        "fn usage_text",
        "fn print_help",
        "fn cmd_help",
        "fn verb_help",
        "fn usage_error",
    ];
    for f in FNS {
        for (i, _) in src.match_indices(f) {
            let rest = &src[i + f.len()..];
            if rest.starts_with('(') || rest.starts_with('<') {
                return Some("help-fn");
            }
        }
    }
    for kw in ["const ", "static "] {
        for (i, _) in src.match_indices(kw) {
            let ident: String = src[i + kw.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if ident.split('_').any(|seg| seg == "USAGE" || seg == "HELP") {
                return Some("help-const");
            }
        }
    }
    if src.contains("#[command(")
        || src.contains("#[clap(")
        || src.contains("about = \"")
        || src.contains("long_about")
    {
        return Some("clap-help");
    }
    None
}

// ----------------------------------------------------------------- extraction

/// `(string literals, doc comments)` in source order; plain comments, char
/// literals and code are dropped.
pub(crate) fn prose_of_rust(src: &str) -> (Vec<String>, Vec<String>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut strings = Vec::new();
    let mut docs = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if b[i..].starts_with(b"//") {
            let j = src[i..].find('\n').map_or(n, |k| i + k);
            let line = &src[i..j];
            if line.starts_with("///") || line.starts_with("//!") {
                docs.push(line[3..].trim().to_string());
            }
            i = j + 1;
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let doc = b[i..].starts_with(b"/**") || b[i..].starts_with(b"/*!");
            let mut depth = 1;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if b[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            if doc {
                let end = j.saturating_sub(2).max(i + 3);
                docs.push(src[i + 3..end].trim().to_string());
            }
            i = j;
            continue;
        }
        // raw strings: r"…", r#"…"#, br"…", cr"…"
        if let Some((open_len, hashes)) = raw_string_open(b, i) {
            let start = i + open_len;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[start..].find(&close).map_or(n, |k| start + k);
            strings.push(src[start..end].to_string());
            i = end + close.len();
            continue;
        }
        if c == b'"' && !(i > 0 && i + 1 < n && b[i - 1] == b'\'' && b[i + 1] == b'\'') {
            let mut j = i + 1;
            let mut buf = String::new();
            while j < n {
                let ch = b[j];
                if ch == b'\\' {
                    let nxt = b.get(j + 1).copied();
                    if nxt == Some(b'\n') {
                        j += 2;
                        while j < n && matches!(b[j], b' ' | b'\t' | b'\r' | b'\n') {
                            j += 1;
                        }
                        continue;
                    }
                    buf.push('\\');
                    if let Some(x) = nxt {
                        buf.push(x as char);
                    }
                    j += 2;
                    continue;
                }
                if ch == b'"' {
                    break;
                }
                // Copy one UTF-8 scalar.
                let ch_len = utf8_len(ch);
                buf.push_str(&src[j..(j + ch_len).min(n)]);
                j += ch_len;
            }
            strings.push(buf);
            i = j + 1;
            continue;
        }
        if c == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let j = src[i + 2..].find('\'').map_or(n, |k| i + 2 + k);
                i = j + 1;
                continue;
            }
            // 'x' where x may be multi-byte
            if i + 1 < n {
                let l = utf8_len(b[i + 1]);
                if i + 1 + l < n && b[i + 1 + l] == b'\'' {
                    i += 2 + l;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        i += utf8_len(c);
    }
    (strings, docs)
}

/// `Some((opener length, hash count))` when a raw string opens at `i`.
fn raw_string_open(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut k = i;
    if k < b.len() && (b[k] == b'b' || b[k] == b'c') {
        k += 1;
    }
    if k >= b.len() || b[k] != b'r' {
        return None;
    }
    if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
        return None;
    }
    k += 1;
    let mut hashes = 0;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        k += 1;
    }
    if k < b.len() && b[k] == b'"' {
        Some((k + 1 - i, hashes))
    } else {
        None
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// FNV-1a 64 over the prose, as 16 hex digits. Change detection, not a
/// certificate — the roster is reviewed text, not a trust boundary.
pub(crate) fn prose_hash(strings: &[String], docs: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &x in bytes {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for s in strings {
        feed(s.as_bytes());
        feed(b"\n\x00");
    }
    feed(b"\n\x01");
    for d in docs {
        feed(d.as_bytes());
        feed(b"\n\x00");
    }
    format!("{h:016x}")
}

fn read_prose_hash(path: &Path) -> Option<String> {
    let src = std::fs::read_to_string(path).ok()?;
    let (s, d) = prose_of_rust(&src);
    Some(prose_hash(&s, &d))
}

// ---------------------------------------------------------------------- dates

fn today_days() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or(0)
}

/// `YYYY-MM-DD` -> days since 1970-01-01 (proleptic Gregorian); `None` when
/// malformed.
fn days_from_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_iso`], for the rows the refusal lines print.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "//! A tool.\nconst USAGE: &str = \"Usage: tool <file> [--fast]\\n\\nOptions:\\n  --fast  skip checks\";\nfn main() { let _c = '\"'; // not a \"string\"\n    /* plain comment \"not prose\" */ println!(\"{USAGE}\"); }\n";
    const OTHER: &str = "fn helper() -> u8 { 7 } // no help here\n";
    const TODAY: i64 = 20_706; // 2026-09-10

    fn tree(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "xtask-help-surfaces-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/t/src")).unwrap();
        std::fs::write(
            root.join("crates/t/Cargo.toml"),
            "[package]\nname = \"t\"\n",
        )
        .unwrap();
        std::fs::write(root.join("crates/t/src/main.rs"), SRC).unwrap();
        std::fs::write(root.join("crates/t/src/helper.rs"), OTHER).unwrap();
        root
    }

    fn tags(refusals: &[String]) -> String {
        refusals
            .iter()
            .map(|r| r.split_whitespace().next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The verb-level red fixture: the REAL check over a temp tree, GREEN on a
    /// read row and RED — with the rule named — on every way a roster can lie.
    #[test]
    fn a_changed_or_unrostered_help_surface_fails_the_help_surfaces_verb() {
        let root = tree("verb");
        let hash = read_prose_hash(&root.join("crates/t/src/main.rs")).unwrap();
        let row = (
            "crates/t/src/main.rs",
            hash.as_str(),
            "2026-09-10",
            "fixture",
        );
        let check = |surfaces: &[SurfaceRow<'_>], not_help: &[NotHelpRow<'_>]| {
            help_surfaces_check(&root, &["crates"], surfaces, not_help, 180, TODAY)
        };

        assert!(
            check(&[row], &[]).is_empty(),
            "control: a read row is green"
        );
        let unrostered = check(&[], &[]);
        assert!(!unrostered.is_empty(), "no row at all must be RED");
        assert!(tags(&unrostered).contains("R1"), "{unrostered:?}");
        assert!(
            tags(&check(&[row], &[("crates/t/src/helper.rs", "planted")])).contains("R5"),
            "NOT_HELP naming a file discovery does not find"
        );
        assert!(
            tags(&check(&[row, row], &[])).contains("R6"),
            "the same path rostered twice"
        );
        assert!(
            tags(&check(
                &[("crates/t/src/main.rs", &hash, "2025-01-01", "f")],
                &[]
            ))
            .contains("R4"),
            "a row older than the limit"
        );
        assert!(
            tags(&check(
                &[("crates/t/src/main.rs", &hash, "yesterday", "f")],
                &[]
            ))
            .contains("R4"),
            "a malformed date"
        );

        // A code-only edit leaves the row green; a help-literal edit turns it red.
        std::fs::write(
            root.join("crates/t/src/main.rs"),
            SRC.replace("let _c", "let _d"),
        )
        .unwrap();
        assert!(check(&[row], &[]).is_empty(), "code-only edit: still green");
        std::fs::write(
            root.join("crates/t/src/main.rs"),
            SRC.replace("skip checks", "skip nothing"),
        )
        .unwrap();
        let red = check(&[row], &[]);
        assert!(!red.is_empty(), "an edited help literal must be RED");
        assert!(tags(&red).contains("R2"), "edited help literal: {red:?}");
        assert!(
            red.iter().any(|r| r.contains("2026-09-10")),
            "the refusal carries today's row to paste: {red:?}"
        );

        // A program with no help text and no row: R7, cleared by a NOT_HELP row
        // that R5 must not then refuse.
        std::fs::write(root.join("crates/t/src/main.rs"), SRC).unwrap();
        std::fs::create_dir_all(root.join("crates/t/src/bin")).unwrap();
        std::fs::write(
            root.join("crates/t/src/bin/quiet.rs"),
            "fn main() { let _ = std::env::args(); }\n",
        )
        .unwrap();
        let quiet = check(&[row], &[]);
        assert!(!quiet.is_empty(), "an unaccounted program must be RED");
        assert!(tags(&quiet).contains("R7"), "{quiet:?}");
        assert!(
            check(
                &[row],
                &[("crates/t/src/bin/quiet.rs", "takes no arguments")]
            )
            .is_empty(),
            "a NOT_HELP row accounts for a program"
        );
        std::fs::remove_file(root.join("crates/t/src/bin/quiet.rs")).unwrap();

        std::fs::remove_file(root.join("crates/t/src/main.rs")).unwrap();
        assert!(
            tags(&check(&[row], &[])).contains("R3"),
            "rostered file removed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The gate itself, in the test lane: the workspace roster is complete and
    /// current. A red here names the surface and prints the row a fresh read
    /// should record.
    #[test]
    fn the_workspace_help_surfaces_are_read_verified() {
        let (ok, log) = help_surfaces_report(&crate::workspace_root());
        assert!(ok, "{log}");
    }

    #[test]
    fn extraction_keeps_literals_and_doc_comments_and_drops_the_rest() {
        let (strings, docs) = prose_of_rust(SRC);
        assert_eq!(docs, vec!["A tool.".to_string()]);
        assert_eq!(strings.len(), 2, "{strings:?}");
        assert!(strings[0].starts_with("Usage: tool"));
        assert!(
            !strings
                .iter()
                .any(|s| s.contains("not prose") || s.contains("not a"))
        );
        let (raw, _) = prose_of_rust("let r = r#\"a \"quoted\" b\"#; let c = '\\'';");
        assert_eq!(raw, vec!["a \"quoted\" b".to_string()]);
    }

    #[test]
    fn iso_dates_round_trip() {
        for (iso, days) in [
            ("1970-01-01", 0),
            ("2026-09-10", TODAY),
            ("2000-02-29", 11_016),
        ] {
            assert_eq!(days_from_iso(iso), Some(days), "{iso}");
            assert_eq!(civil_from_days(days), iso);
        }
        assert_eq!(days_from_iso("2026-13-01"), None);
        assert_eq!(days_from_iso("26-09-10"), None);
    }
}

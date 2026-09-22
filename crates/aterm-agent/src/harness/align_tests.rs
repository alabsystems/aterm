// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] — the packaging contract, the probes and the alignment
//! verdict (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §1.2, §3.1-§3.8).
//!
//! The four the stage asks for, each with its negative control:
//!
//! * admission REFUSES each malformed contract — one test per shape, and the
//!   good contract is admitted whole so a refusal can never be vacuous;
//! * a failing probe degrades EXACTLY one capability and NAMES the probe that
//!   did it;
//! * the verdict is the INTERSECTION of signed and local, and can only narrow
//!   (property-checked over every pair of the four words);
//! * an unknown ABI reads `unsupported` and renders NOTHING.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A contract that exercises every block and key of the grammar. Every
/// refusal test below is this text with ONE line changed, so a refusal is
/// always attributable to that line.
const GOOD: &str = r#"
# the claude harness, abridged from design §1.2
abi = 1
wraps_any = ["claude"]
policy = "hooks-only"
patch = "deny"
capabilities = ["rm-approve", "usage-hud", "introspect"]
required = []
actuators = ["meta", "notice", "decide"]
egress = ["oauth2.googleapis.com", "sheets.googleapis.com"]
probe_set = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

[shim]
env = ["DISABLE_UPDATES=1"]
args = ["--plugin-dir", "${HARNESS_ROOT}/plugin", "--settings", "${HARNESS_STATE}/settings.json"]

[[tool]]
bin = "node"
probe = "version"
min = "20"
caps = ["usage-hud.sheet"]

[[capability]]
id = "rm-approve"
needs = ["hook.PermissionRequest", "wire.permissionDecision"]
max_level = 2
budget = "60/1h"

[[capability]]
id = "usage-hud"
needs = ["literal.statusLine"]
max_level = 1

[[capability]]
id = "introspect"
needs = []
max_level = 0
"#;

fn good() -> Contract {
    Contract::admit(GOOD).expect("the reference contract is admitted")
}

fn tuple() -> Tuple {
    Tuple {
        harness: String::from("claude-harness"),
        harness_build: String::from("2026092101"),
        program: String::from("claude"),
        program_build: String::from("2026091701"),
        aterm_build: String::from("0.86.0"),
        probe_set: String::from(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    }
}

fn ok(id: &str) -> ProbeResult {
    ProbeResult {
        id: id.to_string(),
        status: ProbeStatus::Ok,
        detail: String::new(),
    }
}

fn absent(id: &str) -> ProbeResult {
    ProbeResult {
        id: id.to_string(),
        status: ProbeStatus::Absent,
        detail: String::from("not in --help"),
    }
}

/// Every probe the reference contract needs, all `ok`.
fn all_ok() -> Vec<ProbeResult> {
    vec![
        ok("hook.PermissionRequest"),
        ok("wire.permissionDecision"),
        ok("literal.statusLine"),
        ok("tool.node"),
    ]
}

/// A unique temp directory the caller removes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("aterm-align-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the temp directory is created");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn write_probe(dir: &Path, id: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(id);
    std::fs::write(&path, body).expect("the probe is written");
    let mut perms = std::fs::metadata(&path)
        .expect("the probe exists")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("the probe is executable");
}

// ---------------------------------------------------------------------------
// Admission — the POSITIVE control first
// ---------------------------------------------------------------------------

#[test]
fn the_reference_contract_is_admitted_whole() {
    let c = good();
    assert_eq!(c.abi, 1);
    assert_eq!(c.wraps_any, vec![String::from("claude")]);
    assert_eq!(c.policy, Policy::HooksOnly);
    assert_eq!(c.patch, PatchPolicy::Deny);
    assert!(c.wraps("claude"));
    assert!(!c.wraps("codex"));
    assert_eq!(c.capabilities.len(), 3);
    assert_eq!(c.capability("rm-approve").map(|c| c.max_level), Some(2));
    assert_eq!(
        c.capability("rm-approve").map(|c| c.budget.as_str()),
        Some("60/1h")
    );
    assert_eq!(c.tools.len(), 1);
    assert_eq!(c.tools[0].probe_id(), "tool.node");
    assert_eq!(
        c.shim.env,
        vec![(String::from("DISABLE_UPDATES"), String::from("1"))]
    );
    assert_eq!(c.probe_set_short(), "01234567");
    // A contract with NO optional keys at all is still a contract: the
    // defaults are the safe ones.
    let bare = Contract::admit(
        "abi = 1\nwraps_any = [\"claude\"]\ncapabilities = [\"introspect\"]\n\n[[capability]]\nid = \"introspect\"\n",
    )
    .expect("a minimal contract is admitted");
    assert_eq!(bare.policy, Policy::HooksOnly);
    assert_eq!(bare.patch, PatchPolicy::Deny);
    assert!(bare.egress.is_empty());
    assert!(bare.shim.args.is_empty());
}

/// Each row is (what is wrong, the line that replaces one of GOOD's). The
/// refusal must name the offending thing, and NOTHING may be admitted.
#[test]
fn admission_refuses_each_malformed_contract_whole() {
    let cases: &[(&str, &str, &str)] = &[
        ("abi = 1", "abi = \"one\"", "abi"),
        ("abi = 1", "abi = 0", "abi"),
        ("abi = 1", "", "abi"),
        ("wraps_any = [\"claude\"]", "wraps_any = []", "wraps_any"),
        (
            "wraps_any = [\"claude\"]",
            "wraps_any = [\"../claude\"]",
            "wraps_any",
        ),
        ("policy = \"hooks-only\"", "policy = \"whatever\"", "policy"),
        ("patch = \"deny\"", "patch = \"maybe\"", "patch"),
        (
            "actuators = [\"meta\", \"notice\", \"decide\"]",
            "actuators = [\"meta\", \"signal\"]",
            "actuator",
        ),
        (
            "egress = [\"oauth2.googleapis.com\", \"sheets.googleapis.com\"]",
            "egress = [\"https://sheets.googleapis.com/v4\"]",
            "egress",
        ),
        (
            "probe_set = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"",
            "probe_set = \"sha256:beef\"",
            "probe_set",
        ),
        ("required = []", "required = [\"no-such-cap\"]", "required"),
        // The shim's two closed vocabularies.
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"PATH=/tmp/evil\"]",
            "never sets",
        ),
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"DYLD_INSERT_LIBRARIES=/tmp/x.dylib\"]",
            "never sets",
        ),
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"NODE_OPTIONS=--require /tmp/x.js\"]",
            "never sets",
        ),
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"DISABLE=\"]",
            "empty value",
        ),
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"lower=1\"]",
            "[A-Z0-9_]",
        ),
        (
            "env = [\"DISABLE_UPDATES=1\"]",
            "env = [\"NOEQUALS\"]",
            "NAME=VALUE",
        ),
        (
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/plugin\", \"--settings\", \"${HARNESS_STATE}/settings.json\"]",
            "args = [\"--dangerously-skip-permissions\"]",
            "closed vocabulary",
        ),
        (
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/plugin\", \"--settings\", \"${HARNESS_STATE}/settings.json\"]",
            "args = [\"--bare\"]",
            "closed vocabulary",
        ),
        (
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/plugin\", \"--settings\", \"${HARNESS_STATE}/settings.json\"]",
            "args = [\"--permission-mode\", \"bypassPermissions\"]",
            "closed vocabulary",
        ),
        (
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/plugin\", \"--settings\", \"${HARNESS_STATE}/settings.json\"]",
            "args = [\"--settings\", \"${HOME}/.claude/settings.json\"]",
            "substitution",
        ),
        (
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/plugin\", \"--settings\", \"${HARNESS_STATE}/settings.json\"]",
            "args = [\"--plugin-dir\", \"${HARNESS_ROOT}/../../../plugin\"]",
            "`..`",
        ),
        // The tool rows.
        (
            "bin = \"node\"",
            "bin = \"/usr/local/bin/node\"",
            "bare name",
        ),
        ("probe = \"version\"", "probe = \"literal\"", "version"),
        ("min = \"20\"", "min = \"twenty\"", "dotted number"),
        (
            "caps = [\"usage-hud.sheet\"]",
            "caps = [\"no-such-cap\"]",
            "not a declared capability",
        ),
        ("caps = [\"usage-hud.sheet\"]", "caps = []", "no caps"),
        // The capability blocks.
        (
            "needs = [\"literal.statusLine\"]",
            "needs = [\"statusLine\"]",
            "<kind>.<name>",
        ),
        ("max_level = 1", "max_level = 5", "max_level"),
        ("budget = \"60/1h\"", "budget = \"lots\"", "budget"),
        // The file grammar itself.
        (
            "policy = \"hooks-only\"",
            "polciy = \"hooks-only\"",
            "unknown key",
        ),
        ("[shim]", "[shims]", "unknown block"),
        (
            "policy = \"hooks-only\"",
            "policy = 'hooks-only'",
            "double-quoted",
        ),
        (
            "policy = \"hooks-only\"",
            "policy = \"hooks\\-only\"",
            "backslash",
        ),
        (
            "[[capability]]\nid = \"introspect\"",
            "[[capability]]",
            "required",
        ),
    ];
    for (from, to, expect) in cases {
        assert!(
            GOOD.contains(from),
            "the fixture no longer carries {from:?}; fix the test, not the code"
        );
        let text = GOOD.replacen(from, to, 1);
        match Contract::admit(&text) {
            Ok(_) => panic!("admitted a contract whose {from:?} became {to:?}"),
            Err(why) => assert!(
                why.contains(expect),
                "refusing {to:?} said {why:?}, which does not name {expect:?}"
            ),
        }
    }
}

#[test]
fn a_capability_with_no_block_and_a_block_with_no_capability_are_both_refused() {
    let missing_block = GOOD.replacen(
        "capabilities = [\"rm-approve\", \"usage-hud\", \"introspect\"]",
        "capabilities = [\"rm-approve\", \"usage-hud\", \"introspect\", \"limits\"]",
        1,
    );
    let why = Contract::admit(&missing_block).expect_err("a declared cap with no block is refused");
    assert!(why.contains("limits"), "{why}");

    let extra_block = format!("{GOOD}\n[[capability]]\nid = \"limits\"\nneeds = []\n");
    let why = Contract::admit(&extra_block).expect_err("a block for an undeclared cap is refused");
    assert!(why.contains("not in the capabilities list"), "{why}");

    let twice = format!("{GOOD}\n[[capability]]\nid = \"introspect\"\nneeds = []\n");
    let why = Contract::admit(&twice).expect_err("a repeated capability block is refused");
    assert!(why.contains("twice"), "{why}");
}

#[test]
fn the_reader_refuses_what_it_cannot_read_rather_than_skipping_it() {
    for (text, expect) in [
        ("abi = 1\nwraps_any = [\n  \"claude\",\n]\n", "one line"),
        ("[programs.claude]\n", "[a-z0-9_-]"),
        ("abi = 1\nabi = 2\n", "duplicate key"),
        ("[shim]\n[shim]\n", "repeated [table]"),
        ("abi = 0x01\n", "an integer"),
        ("this is not toml\n", "neither a header nor key"),
        ("[unclosed\n", "unclosed"),
    ] {
        let why = Contract::admit(text).expect_err("this file leaves the grammar");
        assert!(
            why.contains(expect),
            "{text:?} said {why:?}, want {expect:?}"
        );
    }
    // A `#` INSIDE a string is not a comment.
    let c = Contract::admit(
        "abi = 1\nwraps_any = [\"claude\"]\ncapabilities = [\"introspect\"]\negress = [\"a.example\"] # a note\n\n[[capability]]\nid = \"introspect\"\n",
    )
    .expect("a trailing comment is not a refusal");
    assert_eq!(c.egress, vec![String::from("a.example")]);
}

#[test]
fn the_contract_is_bounded_and_a_file_past_the_bound_is_refused() {
    let big = "x".repeat(MAX_CONTRACT_BYTES + 1);
    let why = Contract::admit(&big).expect_err("a file past the byte bound is refused");
    assert!(why.contains("at most"), "{why}");
    let many = "# a comment\n".repeat(MAX_CONTRACT_LINES + 2);
    let why = Contract::admit(&many).expect_err("a file past the line bound is refused");
    assert!(why.contains("at most"), "{why}");
}

#[test]
fn the_shim_renders_its_substitutions_and_re_bounds_the_rendered_value() {
    let c = good();
    let root = PathBuf::from("/opt/store/claude-harness/2026092101");
    let state = PathBuf::from("/opt/harness/claude-harness");
    let rendered = c.shim.render(&root, &state).expect("the shim renders");
    assert_eq!(
        rendered.args,
        vec![
            String::from("--plugin-dir"),
            String::from("/opt/store/claude-harness/2026092101/plugin"),
            String::from("--settings"),
            String::from("/opt/harness/claude-harness/settings.json"),
        ]
    );
    assert_eq!(rendered.env[0].1, "1");
    // NEGATIVE: a value that fits the bound DECLARED but not RENDERED is
    // refused at render time, which is the whole reason render re-admits.
    let long = format!(
        "abi = 1\nwraps_any = [\"claude\"]\ncapabilities = [\"introspect\"]\n\n[shim]\nenv = [\"X={SUBST_ROOT}\"]\n\n[[capability]]\nid = \"introspect\"\n"
    );
    let c = Contract::admit(&long).expect("the declared entry is short");
    let deep = PathBuf::from(format!("/{}", "d".repeat(MAX_ENTRY_BYTES)));
    let why = c
        .shim
        .render(&deep, &state)
        .expect_err("the rendered entry is over the bound");
    assert!(why.contains("rendered entry"), "{why}");
}

// ---------------------------------------------------------------------------
// Probe results — a crash is `error`, never `ok` and never `absent`
// ---------------------------------------------------------------------------

#[test]
fn a_probe_result_is_read_fail_closed() {
    assert_eq!(
        ProbeResult::parse("help.settings", "{\"status\":\"ok\"}").status,
        ProbeStatus::Ok
    );
    assert_eq!(
        ProbeResult::parse(
            "help.settings",
            "{\"id\":\"help.settings\",\"status\":\"absent\",\"detail\":\"no --settings\"}"
        ),
        ProbeResult {
            id: String::from("help.settings"),
            status: ProbeStatus::Absent,
            detail: String::from("no --settings"),
        }
    );
    // NEGATIVE CASES: every one of these is `error`, and none is `absent`.
    for (stdout, expect) in [
        ("", "printed nothing"),
        ("   \n", "printed nothing"),
        ("not json", "not one JSON object"),
        ("[1,2]", "not a JSON object"),
        ("{\"detail\":\"hi\"}", "printed no status"),
        ("{\"status\":\"fine\"}", "printed status"),
        ("{\"status\":\"OK\"}", "printed status"),
        (
            "{\"id\":\"hook.Other\",\"status\":\"ok\"}",
            "claims id \"hook.Other\"",
        ),
    ] {
        let r = ProbeResult::parse("help.settings", stdout);
        assert_eq!(r.status, ProbeStatus::Error, "{stdout:?} read {r:?}");
        assert_eq!(r.id, "help.settings");
        assert!(r.detail.contains(expect), "{stdout:?} said {:?}", r.detail);
    }
}

#[cfg(unix)]
#[test]
fn run_probes_measures_a_real_tree_and_a_broken_probe_is_an_error() {
    let dir = TempDir::new("run");
    let probes = dir.path().join("probes");
    std::fs::create_dir_all(&probes).expect("the probe dir is created");
    write_probe(
        &probes,
        "help.settings",
        "#!/bin/sh\nprintf '{\"id\":\"help.settings\",\"status\":\"ok\",\"detail\":\"%s\"}' \"$CANDIDATE\"\n",
    );
    write_probe(
        &probes,
        "literal.statusLine",
        "#!/bin/sh\nprintf '{\"status\":\"absent\"}'\n",
    );
    write_probe(&probes, "hook.Crash", "#!/bin/sh\nexit 3\n");
    write_probe(&probes, "hook.Silent", "#!/bin/sh\nexit 0\n");
    // Not a probe id, so it is not a probe and not an error either.
    std::fs::write(probes.join("README"), "notes\n").expect("the readme is written");

    let ctx = ProbeCtx {
        candidate: PathBuf::from("/usr/bin/true"),
        tree: dir.path().to_path_buf(),
        state: dir.path().to_path_buf(),
        online: false,
    };
    let run = run_probes(&probes, &ctx, PROBE_BUDGET);
    assert!(run.refusal.is_none(), "{:?}", run.refusal);
    assert_eq!(run.results.len(), 4, "{:?}", run.results);

    let settings = run.get("help.settings").expect("the help probe ran");
    assert_eq!(settings.status, ProbeStatus::Ok);
    assert_eq!(
        settings.detail, "/usr/bin/true",
        "CANDIDATE reached the probe"
    );
    assert_eq!(
        run.get("literal.statusLine").map(|r| r.status),
        Some(ProbeStatus::Absent)
    );
    for (id, expect) in [
        ("hook.Crash", "exited 3"),
        ("hook.Silent", "printed nothing"),
    ] {
        let r = run.get(id).unwrap_or_else(|| panic!("{id} has a result"));
        assert_eq!(r.status, ProbeStatus::Error, "{id} read {r:?}");
        assert!(r.detail.contains(expect), "{id} said {:?}", r.detail);
    }

    // A probe that never finishes is an ERROR, and the deadline is what makes
    // it one. It lives in a directory of its own so a slow machine can never
    // let it eat the budget of the four measured probes above — that flaked
    // once, and the fix belongs in the test's shape, not in a longer timeout.
    let hang_dir = dir.path().join("hang");
    std::fs::create_dir_all(&hang_dir).expect("the hang dir is created");
    write_probe(&hang_dir, "wait.Hang", "#!/bin/sh\nsleep 30\n");
    let hung = run_probes(&hang_dir, &ctx, Duration::from_millis(300));
    assert_eq!(hung.results.len(), 1);
    assert_eq!(hung.results[0].status, ProbeStatus::Error);
    assert!(
        hung.results[0].detail.contains("deadline"),
        "{:?}",
        hung.results[0].detail
    );

    // NEGATIVE CONTROL: with the budget already gone, the probes that never
    // ran read `error`, and never `absent` — nothing measured them.
    let starved = run_probes(&probes, &ctx, Duration::from_millis(1));
    assert_eq!(starved.results.len(), 4);
    assert!(
        starved
            .results
            .iter()
            .all(|r| r.status == ProbeStatus::Error),
        "{:?}",
        starved.results
    );
    assert!(
        starved
            .results
            .iter()
            .any(|r| r.detail.contains("budget was spent")),
        "{:?}",
        starved.results
    );
}

#[test]
fn a_probe_directory_that_is_not_there_is_a_refusal_not_an_empty_pass() {
    let dir = TempDir::new("missing");
    let ctx = ProbeCtx {
        candidate: PathBuf::from("/usr/bin/true"),
        tree: dir.path().to_path_buf(),
        state: dir.path().to_path_buf(),
        online: false,
    };
    let run = run_probes(&dir.path().join("nope"), &ctx, PROBE_BUDGET);
    assert!(run.results.is_empty());
    assert!(run.refusal.is_some(), "a missing probe dir must say so");
}

#[test]
fn the_probe_set_digest_is_over_the_files_and_changes_with_them() {
    let dir = TempDir::new("digest");
    let probes = dir.path().join("probes");
    std::fs::create_dir_all(&probes).expect("the probe dir is created");
    std::fs::write(probes.join("help.settings"), "a").expect("write");
    std::fs::write(probes.join("literal.statusLine"), "b").expect("write");
    let first = probe_set_digest(&probes).expect("the digest is computed");
    assert!(first.starts_with("sha256:"), "{first}");
    assert_eq!(first.len(), "sha256:".len() + 64);
    assert_eq!(
        probe_set_digest(&probes).expect("the digest is stable"),
        first
    );
    std::fs::write(probes.join("literal.statusLine"), "c").expect("write");
    assert_ne!(
        probe_set_digest(&probes).expect("the digest is recomputed"),
        first,
        "a changed probe must change the probe_set"
    );
    // NEGATIVE: a subdirectory is not a file, and the digest refuses rather
    // than describing a tree it did not read.
    std::fs::create_dir(probes.join("sub")).expect("mkdir");
    let why = probe_set_digest(&probes).expect_err("a subdirectory is refused");
    assert!(why.contains("not a regular file"), "{why}");
}

#[test]
fn a_tool_row_is_measured_through_an_injected_runner() {
    let row = ToolRow {
        bin: String::from("node"),
        min: String::from("20"),
        caps: vec![String::from("usage-hud.sheet")],
    };
    let mut present = |_: &str| Some(String::from("v26.3.0\n"));
    let r = probe_tool(&row, &mut present);
    assert_eq!(r.status, ProbeStatus::Ok);
    assert_eq!(r.id, "tool.node");
    assert!(r.detail.contains("26.3.0"), "{:?}", r.detail);

    let mut old = |_: &str| Some(String::from("v18.1.0\n"));
    let r = probe_tool(&row, &mut old);
    assert_eq!(r.status, ProbeStatus::Absent, "too old is a MEASUREMENT");
    assert!(r.detail.contains("< 20"), "{:?}", r.detail);

    let mut gone = |_: &str| None;
    assert_eq!(probe_tool(&row, &mut gone).status, ProbeStatus::Absent);

    let mut garbage = |_: &str| Some(String::from("who knows\n"));
    let r = probe_tool(&row, &mut garbage);
    assert_eq!(r.status, ProbeStatus::Error, "no version is no measurement");

    // Presence-only rows accept anything that parses.
    let presence = ToolRow {
        bin: String::from("sh"),
        min: String::new(),
        caps: vec![String::from("introspect")],
    };
    let mut any = |_: &str| Some(String::from("GNU bash, version 5.2.37(1)-release"));
    assert_eq!(probe_tool(&presence, &mut any).status, ProbeStatus::Ok);
}

#[test]
fn version_comparison_treats_a_missing_component_as_zero() {
    assert!(version_at_least("20", "20"));
    assert!(version_at_least("20.1", "20"));
    assert!(version_at_least("26.3.0", "20"));
    assert!(!version_at_least("18.20.0", "20"));
    assert!(!version_at_least("2.1", "2.1.1"));
    assert!(version_at_least("2.1.1", "2.1"));
    assert_eq!(
        first_dotted_number("2.1.274 (Claude Code)").as_deref(),
        Some("2.1.274")
    );
    assert_eq!(
        first_dotted_number("codex-cli 0.154.0").as_deref(),
        Some("0.154.0")
    );
    assert_eq!(first_dotted_number("v26.3.0").as_deref(), Some("26.3.0"));
    assert_eq!(first_dotted_number("nothing here").as_deref(), None);
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

#[test]
fn every_probe_ok_is_aligned_and_names_no_capability() {
    let c = good();
    let a = evaluate(&c, &all_ok(), &c.required, &[]);
    assert_eq!(a.verdict, Verdict::Aligned);
    assert_eq!(a.counts(), (3, 3));
    assert_eq!(a.wire(), "aligned(3/3)");
    assert_eq!(a.caps_on().len(), 3);
    assert!(a.caps.iter().all(|c| c.blamed.is_none()));
}

#[test]
fn a_failing_probe_degrades_exactly_one_capability_and_names_why() {
    let c = good();
    let mut results = all_ok();
    results.retain(|r| r.id != "literal.statusLine");
    results.push(absent("literal.statusLine"));

    let a = evaluate(&c, &results, &c.required, &[]);
    assert_eq!(
        a.verdict,
        Verdict::Degraded(vec![String::from("usage-hud")])
    );
    assert_eq!(a.wire(), "degraded(2/3):usage-hud");
    assert_eq!(a.counts(), (2, 3));

    let hud = a
        .caps
        .iter()
        .find(|v| v.id == "usage-hud")
        .expect("the hud has a verdict");
    assert!(!hud.on());
    assert_eq!(hud.blamed.as_deref(), Some("literal.statusLine"));
    assert!(hud.reason().contains("absent"), "{:?}", hud.reason());
    assert!(hud.reason().contains("not in --help"), "{:?}", hud.reason());
    // The OTHER two are untouched — degradation, not all-or-nothing.
    for id in ["rm-approve", "introspect"] {
        let v = a.caps.iter().find(|v| v.id == id).expect("a verdict");
        assert!(v.on(), "{id} must survive an unrelated probe failure");
        assert!(v.blamed.is_none());
    }
}

#[test]
fn a_need_with_no_result_is_off_and_pending_never_ok() {
    let c = good();
    // Everything but rm-approve's second need measured.
    let results = vec![
        ok("hook.PermissionRequest"),
        ok("literal.statusLine"),
        ok("tool.node"),
    ];
    let a = evaluate(&c, &results, &c.required, &[]);
    let rm = a
        .caps
        .iter()
        .find(|v| v.id == "rm-approve")
        .expect("a verdict");
    assert!(!rm.on(), "an unmeasured need is not a measured one");
    assert_eq!(rm.blamed.as_deref(), Some("wire.permissionDecision"));
    assert!(rm.reason().contains("pending"), "{:?}", rm.reason());
    assert_eq!(
        a.verdict,
        Verdict::Degraded(vec![String::from("rm-approve")])
    );
}

#[test]
fn no_results_at_all_is_pending_and_turns_every_capability_off() {
    let c = good();
    let a = evaluate(&c, &[], &c.required, &[]);
    assert_eq!(a.verdict, Verdict::Pending);
    assert_eq!(a.wire(), "pending");
    assert!(a.caps_on().is_empty(), "pending renders nothing");
    assert!(a.caps.iter().all(|v| v.reason() == "probe pending"));
}

#[test]
fn a_required_capability_failing_is_unsupported_not_degraded() {
    let c = good();
    let mut results = all_ok();
    results.retain(|r| r.id != "literal.statusLine");
    results.push(absent("literal.statusLine"));
    let required = vec![String::from("usage-hud")];
    let a = evaluate(&c, &results, &required, &[]);
    assert_eq!(
        a.verdict,
        Verdict::Unsupported(String::from("required: usage-hud"))
    );
    assert_eq!(a.wire(), "unsupported:required: usage-hud");
    // The SAME failure with an empty `required` is only a degradation: the
    // owner's list is what makes it hold a program.
    let a = evaluate(&c, &results, &[], &[]);
    assert_eq!(
        a.verdict,
        Verdict::Degraded(vec![String::from("usage-hud")])
    );
}

#[test]
fn a_tool_row_gates_a_sub_capability_and_leaves_its_parent_alone() {
    let c = good();
    let mut results = all_ok();
    results.retain(|r| r.id != "tool.node");
    results.push(ProbeResult {
        id: String::from("tool.node"),
        status: ProbeStatus::Absent,
        detail: String::from("tool node: absent"),
    });
    let a = evaluate(&c, &results, &c.required, &[]);
    assert_eq!(
        a.verdict,
        Verdict::Aligned,
        "tool.node gates usage-hud.sheet, not usage-hud"
    );

    // A tool row naming the PARENT does take the parent down.
    let text = GOOD.replacen("caps = [\"usage-hud.sheet\"]", "caps = [\"usage-hud\"]", 1);
    let c = Contract::admit(&text).expect("the contract is admitted");
    let a = evaluate(&c, &results, &c.required, &[]);
    assert_eq!(
        a.verdict,
        Verdict::Degraded(vec![String::from("usage-hud")])
    );
    let hud = a
        .caps
        .iter()
        .find(|v| v.id == "usage-hud")
        .expect("verdict");
    assert_eq!(hud.blamed.as_deref(), Some("tool.node"));
}

#[test]
fn an_optional_need_is_consulted_only_when_the_owner_opted_in() {
    let text = GOOD.replacen(
        "needs = [\"literal.statusLine\"]",
        "needs = [\"literal.statusLine\"]\noptional = [\"literal.low-priority\"]",
        1,
    );
    let c = Contract::admit(&text).expect("the contract is admitted");
    let results = all_ok();
    assert_eq!(
        evaluate(&c, &results, &c.required, &[]).verdict,
        Verdict::Aligned,
        "an optional need nobody asked for cannot degrade anything"
    );
    let opted = vec![String::from("literal.low-priority")];
    assert_eq!(
        evaluate(&c, &results, &c.required, &opted).verdict,
        Verdict::Degraded(vec![String::from("usage-hud")]),
        "opted in, the same missing probe now counts"
    );
}

#[test]
fn an_unknown_abi_reads_unsupported_and_renders_nothing() {
    let ahead = GOOD.replacen("abi = 1", "abi = 2", 1);
    let c = Contract::admit(&ahead).expect("a future ABI still PARSES; the range is the gate");
    assert!(!abi_supported(c.abi));
    let a = evaluate(&c, &all_ok(), &c.required, &[]);
    assert_eq!(
        a.verdict,
        Verdict::Unsupported(String::from("needs aterm ABI 2, running 1"))
    );
    assert!(!a.verdict.renders(), "an ABI miss renders nothing");
    assert!(
        a.caps_on().is_empty(),
        "no probe result may lift an ABI miss"
    );
    assert_eq!(a.wire(), "unsupported:needs aterm ABI 2, running 1");
    // A miss in the OTHER direction — an ABI below the floor — reads the same
    // way. Today the floor is 1 so the case is constructed from the rule
    // rather than from a contract (abi = 0 is refused at parse).
    assert!(!abi_supported(0));
    assert!(abi_supported(HARNESS_ABI));
    assert!(abi_supported(HARNESS_ABI_MIN));
}

// ---------------------------------------------------------------------------
// The intersection rule
// ---------------------------------------------------------------------------

/// How much a verdict allows, as a number that `narrow` may only lower.
fn allowance(v: &Verdict, universe: usize) -> usize {
    match v {
        Verdict::Aligned => universe + 1,
        Verdict::Degraded(off) => universe.saturating_sub(off.len()),
        Verdict::Unsupported(_) => 0,
        // Pending allows nothing of its own; it defers, which the property
        // below handles by excluding it from the monotone comparison.
        Verdict::Pending => 0,
    }
}

#[test]
fn narrow_can_only_narrow_over_every_pair_of_verdicts() {
    let universe = 4;
    let all = [
        Verdict::Aligned,
        Verdict::Degraded(vec![String::from("a")]),
        Verdict::Degraded(vec![String::from("a"), String::from("b")]),
        Verdict::Unsupported(String::from("signed says no")),
    ];
    for a in &all {
        for b in &all {
            let got = a.narrow(b);
            assert!(
                allowance(&got, universe) <= allowance(a, universe),
                "{a:?} narrowed by {b:?} gave {got:?}, which allows MORE than {a:?}"
            );
            assert!(
                allowance(&got, universe) <= allowance(b, universe),
                "{a:?} narrowed by {b:?} gave {got:?}, which allows MORE than {b:?}"
            );
            assert_eq!(got, b.narrow(a), "narrow must not depend on the order");
        }
        // Pending defers to the other side, exactly.
        assert_eq!(&Verdict::Pending.narrow(a), a);
        assert_eq!(&a.narrow(&Verdict::Pending), a);
    }
    // The union of off-sets is the intersection of on-sets.
    assert_eq!(
        Verdict::Degraded(vec![String::from("a")])
            .narrow(&Verdict::Degraded(vec![String::from("b")])),
        Verdict::Degraded(vec![String::from("a"), String::from("b")])
    );
    // Signed `unsupported` beats a local `ok` — design §3.5's own example.
    assert_eq!(
        Verdict::Unsupported(String::from("why")).narrow(&Verdict::Aligned),
        Verdict::Unsupported(String::from("why"))
    );
}

#[test]
fn adopt_keeps_local_detail_lowers_the_verdict_and_records_a_disagreement() {
    let c = good();
    let local = evaluate(&c, &all_ok(), &c.required, &[]);
    assert_eq!(local.verdict, Verdict::Aligned);

    // No signed row at all: the local verdict stands alone and says so.
    let alone = adopt(None, local.clone());
    assert_eq!(alone.verdict, Verdict::Aligned);
    assert_eq!(alone.attested, Attestation::Local);
    assert!(alone.disagreement.is_none());

    // Signed degraded + local aligned → degraded, and the capability the
    // signed row names goes off with that as its reason.
    let signed = Verdict::Degraded(vec![String::from("usage-hud")]);
    let both = adopt(Some(&signed), local.clone());
    assert_eq!(
        both.verdict,
        Verdict::Degraded(vec![String::from("usage-hud")])
    );
    assert_eq!(both.attested, Attestation::Both);
    assert_eq!(
        both.caps_on(),
        vec![String::from("rm-approve"), String::from("introspect")]
    );
    let hud = both
        .caps
        .iter()
        .find(|v| v.id == "usage-hud")
        .expect("verdict");
    assert!(hud.reason().contains("signed"), "{:?}", hud.reason());
    assert!(
        both.disagreement
            .as_deref()
            .is_some_and(|d| d.contains("signed degraded:usage-hud") && d.contains("local aligned")),
        "{:?}",
        both.disagreement
    );

    // Signed unsupported + local ok → unsupported, nothing renders.
    let signed = Verdict::Unsupported(String::from("the lane refused it"));
    let refused = adopt(Some(&signed), local.clone());
    assert!(!refused.verdict.renders());
    assert!(refused.caps_on().is_empty());

    // Local degraded + signed aligned → degraded. LOCAL CAN LOWER.
    let mut results = all_ok();
    results.retain(|r| r.id != "literal.statusLine");
    results.push(absent("literal.statusLine"));
    let local = evaluate(&c, &results, &c.required, &[]);
    let adopted = adopt(Some(&Verdict::Aligned), local);
    assert_eq!(
        adopted.verdict,
        Verdict::Degraded(vec![String::from("usage-hud")]),
        "local lowers a signed aligned"
    );
    assert!(
        adopted.disagreement.is_none(),
        "the adopted verdict IS the local one"
    );

    // A signed row with nothing measured locally: the signed verdict stands.
    let pending = evaluate(&c, &[], &c.required, &[]);
    let adopted = adopt(Some(&Verdict::Aligned), pending);
    assert_eq!(adopted.verdict, Verdict::Aligned);
    assert_eq!(adopted.attested, Attestation::Signed);
}

#[test]
fn the_wire_word_round_trips_through_its_own_parser() {
    for v in [
        Verdict::Aligned,
        Verdict::Pending,
        Verdict::Degraded(vec![String::from("usage-hud"), String::from("limits")]),
        Verdict::Unsupported(String::from("needs aterm ABI 2, running 1")),
    ] {
        assert_eq!(Verdict::parse_wire(&v.as_wire()).as_ref(), Some(&v));
    }
    // The COUNTED form parses back to the same verdict — the counts are
    // display, not vocabulary.
    assert_eq!(Verdict::parse_wire("aligned(7/7)"), Some(Verdict::Aligned));
    assert_eq!(
        Verdict::parse_wire("degraded(5/7):usage-hud,model-failover"),
        Some(Verdict::Degraded(vec![
            String::from("usage-hud"),
            String::from("model-failover")
        ]))
    );
    // NEGATIVE: an unknown word is None, never a guess.
    for unknown in ["", "fine", "ALIGNED", "broken:reason", "held"] {
        assert_eq!(Verdict::parse_wire(unknown), None, "{unknown:?}");
    }
}

// ---------------------------------------------------------------------------
// The sidecar, the tuple and the signed row
// ---------------------------------------------------------------------------

#[test]
fn the_sidecar_round_trips_and_a_torn_line_costs_one_row() {
    let t = tuple();
    assert_eq!(t.key(), "claude@2026091701/0.86.0#01234567");
    let text = format!(
        "# aterm harness alignment\n{}{}",
        sidecar_line(&t, "degraded(2/3):usage-hud", 1_700_000_000),
        sidecar_line(&t, "aligned(3/3)", 1_700_000_500),
    );
    let rows = read_sidecar(&text);
    assert_eq!(rows.len(), 2);
    let newest = lookup(&rows, &t).expect("the tuple is in the sidecar");
    assert_eq!(newest.wire, "aligned(3/3)", "the last append wins");
    assert_eq!(newest.at, 1_700_000_500);
    assert_eq!(newest.verdict(), Some(Verdict::Aligned));

    // A torn final append costs THAT row and not the file.
    let torn = format!("{text}claude@2026091801/0.86.0#0123456");
    assert_eq!(read_sidecar(&torn).len(), 2);

    // A row whose word this client does not know is KEPT, and reads as no
    // verdict — the caller then says `probe pending` rather than guessing.
    let future = format!("{text}claude@2026091801/0.86.0#01234567 = \"quarantined\" # at=1\n");
    let rows = read_sidecar(&future);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].verdict(), None);

    // A wire word that tries to break the one-line grammar cannot: the quote
    // that frames the value and every line-breaking character are dropped.
    // NEGATIVE CONTROL for the shared predicate — `U+2028` is Zl, so
    // `char::is_control()` is false for it and the local filter passed it
    // into this row until 2026-09-22.
    assert!(!'\u{2028}'.is_control());
    let hostile = sidecar_line(&t, "aligned(3/3)\u{2028}x = \"forged\" # at=1", 1);
    assert_eq!(hostile.matches('\n').count(), 1, "{hostile:?}");
    assert!(!hostile.contains('\u{2028}'), "{hostile:?}");
    assert_eq!(read_sidecar(&hostile).len(), 1, "one row, not two");

    // A DIFFERENT tuple is not this tuple.
    let other = Tuple {
        aterm_build: String::from("0.87.0"),
        ..tuple()
    };
    assert!(
        lookup(&rows, &other).is_none(),
        "the aterm token is part of the key"
    );
}

#[test]
fn the_sidecar_and_memo_paths_cannot_be_steered_by_a_tuple() {
    let state = Path::new("/opt/harness/claude-harness");
    assert_eq!(
        sidecar_path(state, "2026092101"),
        PathBuf::from("/opt/harness/claude-harness/alignment/2026092101.toml")
    );
    // NEGATIVE: a build string that tries to traverse becomes one component.
    let path = sidecar_path(state, "../../etc/passwd");
    assert_eq!(
        path,
        PathBuf::from("/opt/harness/claude-harness/alignment/______etc_passwd.toml")
    );
    assert!(path.starts_with(state.join("alignment")));
    let evil = Tuple {
        program: String::from("claude"),
        program_build: String::from("../../.."),
        ..tuple()
    };
    let memo = refused_path(state, &evil);
    assert!(
        memo.starts_with(state.join("refused")),
        "{}",
        memo.display()
    );
    assert_eq!(memo.components().count(), state.components().count() + 2);
}

#[test]
fn a_signed_alignment_entry_is_parse_validated_and_refused_whole() {
    let row = SignedRow::parse("claude@2026091801=degraded:usage-hud,model-failover#a1b2c3d4")
        .expect("the design's own example parses");
    assert_eq!(row.program, "claude");
    assert_eq!(row.build, "2026091801");
    assert_eq!(row.probe_set, "a1b2c3d4");
    assert_eq!(
        row.verdict,
        Verdict::Degraded(vec![
            String::from("usage-hud"),
            String::from("model-failover")
        ])
    );
    assert_eq!(
        row.render(),
        "claude@2026091801=degraded:usage-hud,model-failover#a1b2c3d4"
    );
    assert_eq!(
        SignedRow::parse("claude@2026091601=aligned#a1b2c3d4")
            .expect("parses")
            .render(),
        "claude@2026091601=aligned#a1b2c3d4"
    );

    for (entry, expect) in [
        ("claude@2026091601", "no `=`"),
        ("claude=aligned#a1b2c3d4", "no `@`"),
        ("claude@2026091601=aligned", "no `#`"),
        ("claude@2026091601=aligned#a1b2", "eight hex"),
        ("claude@2026091601=fine#a1b2c3d4", "unknown verdict"),
        ("../claude@2026091601=aligned#a1b2c3d4", "program name"),
        ("claude@=aligned#a1b2c3d4", "build"),
    ] {
        let why = SignedRow::parse(entry).expect_err("this entry is malformed");
        assert!(why.contains(expect), "{entry:?} said {why:?}");
    }

    // A malformed entry refuses the WHOLE list, the Reject::Alignment shape.
    let t = tuple();
    let entries = vec!["claude@2026091701=aligned#01234567", "garbage"];
    assert!(
        signed_for(&entries, &t).is_err(),
        "one bad entry refuses the list"
    );

    let entries = vec![
        "claude@2026091601=aligned#01234567",
        "claude@2026091701=degraded:usage-hud#01234567",
    ];
    let found = signed_for(&entries, &t)
        .expect("the list is well formed")
        .expect("a row for this program build");
    assert_eq!(found.build, "2026091701");

    // A row measured by a DIFFERENT probe set is not about this tree.
    let entries = vec!["claude@2026091701=aligned#deadbeef"];
    assert!(
        signed_for(&entries, &t).expect("well formed").is_none(),
        "a probe_set mismatch means the row is about another probe set"
    );
}

#[test]
fn the_alignment_json_carries_the_tuple_the_verdict_and_every_capability() {
    let c = good();
    let mut results = all_ok();
    results.retain(|r| r.id != "literal.statusLine");
    results.push(absent("literal.statusLine"));
    let a = evaluate(&c, &results, &c.required, &[]);
    let doc = a.to_json(&tuple());
    let v: aterm_json::Value = aterm_json::from_str(&doc).expect("the document is JSON");
    let aterm_json::Value::Object(o) = v else {
        panic!("the document is an object");
    };
    assert_eq!(o.get("schema").and_then(aterm_json::Value::as_u64), Some(1));
    assert_eq!(
        o.get("verdict").and_then(aterm_json::Value::as_str),
        Some("degraded(2/3):usage-hud")
    );
    assert_eq!(
        o.get("aterm").and_then(aterm_json::Value::as_str),
        Some("0.86.0")
    );
    assert_eq!(
        o.get("attested").and_then(aterm_json::Value::as_str),
        Some("local")
    );
    assert_eq!(
        o.get("caps_ok").and_then(aterm_json::Value::as_u64),
        Some(2)
    );
    let Some(aterm_json::Value::Array(caps)) = o.get("caps") else {
        panic!("caps is an array");
    };
    assert_eq!(caps.len(), 3);
}

// -- the bounded runner every subprocess in this tree borrows -------------------

#[test]
fn a_child_that_never_returns_is_killed_at_the_deadline() {
    // THE POINT of this shape, and the reason two other call sites now use
    // it: `wait_with_output` on a child that hangs never returns. A stale
    // NFS mount does that to `df`, and a user statusLine that loops does it
    // to the vendor's footer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("sleep 30");
    let started = std::time::Instant::now();
    let got = capture_bounded(cmd, Duration::from_millis(250), None, 1024);
    let why = got.expect_err("a hung child is an error, not a wait");
    assert!(why.contains("deadline"), "{why}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it returned in {:?}",
        started.elapsed()
    );
}

#[test]
fn stdin_reaches_the_child_and_stdout_is_capped() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("cat");
    let got = capture_bounded(cmd, Duration::from_secs(5), Some(b"hello".to_vec()), 1024)
        .expect("it runs");
    assert_eq!(got.stdout, b"hello");
    assert!(got.success());

    // The cap is a CUT, not an error: a child that prints more than the
    // caller asked for loses the tail rather than the whole answer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'aaaaaaaaaaaaaaaaaaaa'");
    let got = capture_bounded(cmd, Duration::from_secs(5), None, 4).expect("it runs");
    assert_eq!(got.stdout, b"aaaa");

    // A child that never reads its stdin cannot block the writer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'ignored\\n'");
    let got = capture_bounded(
        cmd,
        Duration::from_secs(5),
        Some(vec![b'x'; 256 * 1024]),
        1024,
    )
    .expect("it runs");
    assert_eq!(got.stdout, b"ignored\n");

    // The exit code is REPORTED, never swallowed: the probe wrapper turns a
    // non-zero into §3.2's sentence, and the statusLine chain deliberately
    // does not.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("exit 7");
    let got = capture_bounded(cmd, Duration::from_secs(5), None, 16).expect("it runs");
    assert_eq!(got.code, Some(7));
    assert!(!got.success());
}

// ---------------------------------------------------------------------------
// One capability verdict, two callers
// ---------------------------------------------------------------------------

#[test]
fn a_profile_and_a_probe_run_speak_the_same_capability_verdict() {
    use crate::harness::profile::EMACS;

    // The PROFILE's answer and the PROBE RUN's answer are now the same type,
    // so their words cannot drift. Before this they were `Serve` and an
    // `on: bool` beside a free-text `reason`, and nothing compared them.
    let from_profile: Serve = EMACS.serves("usage-hud", "emacs");
    assert!(
        matches!(from_profile, Serve::Unsupported(_)),
        "emacs is not a model client"
    );
    assert_eq!(from_profile.word(), "unsupported");
    assert!(from_profile.reason().is_some());

    let from_probes = CapVerdict {
        id: String::from("usage-hud"),
        serve: Serve::Unsupported(String::from("statusLine: not in --help")),
        blamed: Some(String::from("statusLine")),
        ok: 1,
        total: 2,
    };
    assert_eq!(from_probes.serve.word(), from_profile.word());
    assert!(!from_probes.on());

    // An ON capability's sentence is RENDERED from the counts it already
    // carries, not stored beside them, so the two can never disagree.
    let on = CapVerdict {
        id: String::from("rm-approve"),
        serve: Serve::Ok,
        blamed: None,
        ok: 3,
        total: 3,
    };
    assert!(on.on());
    assert_eq!(on.serve.word(), "ok");
    assert_eq!(on.reason(), "3/3 probes ok");
    assert_eq!(
        CapVerdict {
            total: 0,
            ok: 0,
            ..on.clone()
        }
        .reason(),
        "needs nothing"
    );
    // NEGATIVE CONTROL: `ok` stores no reason, so a row that claims one is
    // not `ok` — which is the invariant the merge rests on.
    assert_eq!(on.serve.reason(), None);
    assert_ne!(from_probes.reason(), on.reason());
    // And the row both surfaces print is ONE function: the profile's
    // `&'static str` form and the probe run's `String` form render the same
    // text, which is what "one vocabulary" has to mean.
    assert_eq!(
        Serve::<String>::Ok.row("disk"),
        Serve::<&str>::Ok.row("disk")
    );
    assert_eq!(
        Serve::Unsupported(String::from("no hooks")).row("liveness"),
        Serve::Unsupported("no hooks").row("liveness")
    );
}

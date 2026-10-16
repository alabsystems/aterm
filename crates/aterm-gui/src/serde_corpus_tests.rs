// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's persistence and config surfaces, recorded while every model
//! derived through registry serde (docs/AUDIT-third-party-2026-10-15.md, SP.3);
//! since the switch (SP.6) the first-party serde must answer each the same:
//!
//! - `aterm.toml` ([`Config`]): container `default`, unknown keys ignored, the
//!   one `alias`, the `skip`ped `[harness]`, a hand-written `FontList`;
//! - the restore layout ([`RestoreManifest`]): internally tagged enums, a
//!   `deny_unknown_fields` tag, an untagged lenient reader;
//! - the seamless handoff manifest ([`SessionHandoff`]): nine lenient
//!   `deserialize_with` readers that turn a bad value into "unreadable", never
//!   a refused manifest;
//! - the checkpoint meta ([`CheckpointMeta`]), JSON whose bytes are hashed
//!   into the adoption proof;
//! - the `FIELDS` list the derive hands `deserialize_struct`, read by the
//!   config language's probe (`reader_fields`).
//!
//! State written by old builds is the bytes every shipped producer wrote,
//! `tests/fixtures/handoff/v*/` (the cross-version guard's fixtures): each
//! manifest, layout and meta there is read and written back out.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use std::path::{Path, PathBuf};

use answers::Corpus;
use aterm_core::terminal::CheckpointMeta;

use crate::app_config::Config;
use crate::native_config_language::tests::reader_fields;
use crate::restore::{CellMetricsCache, RestoreManifest};
use crate::session_store::SessionHandoff;

/// `base` with each `(from, to)` applied; `from` must occur exactly once, so
/// a mutation can never silently leave the document as it was.
fn mutate(base: &str, edits: &[(&str, &str)]) -> String {
    let mut doc = base.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

/// A [`Config`] written out (it has no `Serialize`): its `{:#?}` with every
/// `None` field left out (an absent line IS `None`), on one line. The
/// `skip`ped `[harness]` is always its default here (`Config::parse` reads it
/// by line, not through serde), so it is said once, not printed.
fn config_answer(config: &Config) -> String {
    let harness = if config.harness == crate::app_config::HarnessPolicy::default() {
        "harness=default"
    } else {
        "harness=SET"
    };
    // The two skipped fields (`harness`, `ai`) are filled by `Config::parse`,
    // never by serde: the reader answers for them without their bodies.
    assert!(
        config.ai == crate::app_config::AiPolicy::default(),
        "serde never fills `ai`"
    );
    let pretty = format!("{config:#?}");
    let mut lines = Vec::new();
    let mut in_skipped = false;
    for line in pretty.lines() {
        if line == "    harness: HarnessPolicy {" || line == "    ai: AiPolicy {" {
            in_skipped = true;
        } else if in_skipped {
            in_skipped = line != "    },";
        } else if !line.ends_with(": None,") {
            lines.push(line.trim());
        }
    }
    format!("{harness} {}", lines.join(" "))
}

const CONFIG: &str = r##"font_px = 15.5
gpu = false
scrollback_lines = 20000
cursor_style = "bar"
palette = ["#000000", "#ffffff"]
fallback_fonts = "Menlo, , Symbols Nerd Font "
game_font = "Press Start 2P"
columns = 120
lines = 40
adjust_baseline = -2
keybindings = { "cmd+t" = "new_tab", "ctrl+shift+c" = "copy" }

[render]
repaint_nudge = "log"

[sparkle_words]
enabled = true
languages = ["en", "ja"]

[sparkle_words.ink]
loop = true
sweep_ms = 900

[sparkle_words.profanity]
jitter = -3

[[sparkle_words.custom]]
words = ["shipit"]
ink = { colorway = "rainbow" }

[net]
listen = "127.0.0.1:7777"

[[net.connections]]
name = "lab"
host = "lab.example:7777"
fingerprint = "sha256:abcd"

[harness]
max_sessions = 4
"##;

#[test]
fn config_documents_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "config");
    let read = |doc: &str| aterm_toml::from_str::<Config>(doc).map(|config| config_answer(&config));
    corpus.record("base", read(CONFIG));
    corpus.record("empty", read(""));
    let cases: &[(&str, &[(&str, &str)])] = &[
        // Container default: unknown keys are ignored at every level.
        (
            "unknown-key/root",
            &[("gpu = false\n", "gpu = false\nshader = \"crt\"\n")],
        ),
        (
            "unknown-key/table",
            &[(
                "repaint_nudge = \"log\"\n",
                "repaint_nudge = \"log\"\nvsync = true\n",
            )],
        ),
        ("unknown-key/rust-name", &[("loop = true", "loop_ = true")]),
        (
            "unknown-key/connection",
            &[(
                "fingerprint = \"sha256:abcd\"\n",
                "fingerprint = \"sha256:abcd\"\nport = 1\n",
            )],
        ),
        ("unknown-key/table-header", &[("[render]", "[renderer]")]),
        // The alias, and the alias beside its own name.
        ("alias/canonical", &[("game_font", "display_font")]),
        (
            "alias/both",
            &[(
                "game_font = \"Press Start 2P\"\n",
                "game_font = \"Press Start 2P\"\ndisplay_font = \"Silkscreen\"\n",
            )],
        ),
        // The skipped field: the reader never sees `[harness]`.
        (
            "skip/harness-scalar",
            &[("[harness]\nmax_sessions = 4\n", "")],
        ),
        // A required key inside an optional table.
        (
            "missing-key/connection-host",
            &[("host = \"lab.example:7777\"\n", "")],
        ),
        (
            "missing-key/connection-defaulted-sid",
            &[("name = \"lab\"\n", "name = \"lab\"\nsid = \"s-1\"\n")],
        ),
        // FontList: a string or a list, and nothing else.
        (
            "fontlist/array",
            &[(
                "fallback_fonts = \"Menlo, , Symbols Nerd Font \"",
                "fallback_fonts = [\" Menlo\", \"\", \"Symbols Nerd Font\"]",
            )],
        ),
        (
            "fontlist/int",
            &[(
                "fallback_fonts = \"Menlo, , Symbols Nerd Font \"",
                "fallback_fonts = 3",
            )],
        ),
        (
            "fontlist/mixed-array",
            &[(
                "fallback_fonts = \"Menlo, , Symbols Nerd Font \"",
                "fallback_fonts = [\"Menlo\", 3]",
            )],
        ),
        (
            "fontlist/table",
            &[(
                "fallback_fonts = \"Menlo, , Symbols Nerd Font \"",
                "fallback_fonts = { first = \"Menlo\" }",
            )],
        ),
        // Wrong types.
        ("wrong-type/gpu-string", &[("gpu = false", "gpu = \"no\"")]),
        (
            "wrong-type/font-px-string",
            &[("font_px = 15.5", "font_px = \"15.5\"")],
        ),
        (
            "wrong-type/palette-string",
            &[(
                "palette = [\"#000000\", \"#ffffff\"]",
                "palette = \"#000000\"",
            )],
        ),
        (
            "wrong-type/keybindings-array",
            &[(
                "keybindings = { \"cmd+t\" = \"new_tab\", \"ctrl+shift+c\" = \"copy\" }",
                "keybindings = [\"cmd+t\"]",
            )],
        ),
        (
            "wrong-type/keybinding-value",
            &[("\"ctrl+shift+c\" = \"copy\"", "\"ctrl+shift+c\" = 3")],
        ),
        (
            "wrong-type/render-string",
            &[("[render]\nrepaint_nudge = \"log\"\n", "render = \"log\"\n")],
        ),
        // Tables written as arrays, and the other way about.
        (
            "array-for-table/render-empty",
            &[("[render]\nrepaint_nudge = \"log\"\n", "render = []\n")],
        ),
        (
            "array-for-table/render-positional",
            &[(
                "[render]\nrepaint_nudge = \"log\"\n",
                "render = [\"log\"]\n",
            )],
        ),
        (
            "array-for-table/render-tables",
            &[(
                "[render]\nrepaint_nudge = \"log\"\n",
                "[[render]]\nrepaint_nudge = \"log\"\n",
            )],
        ),
        (
            "table-for-array/connections",
            &[("[[net.connections]]", "[net.connections]")],
        ),
        // Integers at and past each width.
        (
            "int/columns-u16-overflow",
            &[("columns = 120", "columns = 65536")],
        ),
        ("int/columns-negative", &[("columns = 120", "columns = -1")]),
        ("int/jitter-i8-min", &[("jitter = -3", "jitter = -128")]),
        ("int/jitter-i8-overflow", &[("jitter = -3", "jitter = 128")]),
        (
            "int/baseline-i64-max",
            &[(
                "adjust_baseline = -2",
                "adjust_baseline = 9223372036854775807",
            )],
        ),
        (
            "int/sweep-u32-overflow",
            &[("sweep_ms = 900", "sweep_ms = 4294967296")],
        ),
        (
            "int/scrollback-float",
            &[("scrollback_lines = 20000", "scrollback_lines = 20000.0")],
        ),
        ("int/font-px-int", &[("font_px = 15.5", "font_px = 15")]),
        // Signed zero and the non-finite floats.
        (
            "zero/columns-negative-int",
            &[("columns = 120", "columns = -0")],
        ),
        (
            "zero/font-px-negative-float",
            &[("font_px = 15.5", "font_px = -0.0")],
        ),
        ("float/font-px-nan", &[("font_px = 15.5", "font_px = nan")]),
        ("float/font-px-inf", &[("font_px = 15.5", "font_px = -inf")]),
        (
            "float/font-px-f32-overflow",
            &[("font_px = 15.5", "font_px = 1e39")],
        ),
        (
            "float/font-px-subnormal",
            &[("font_px = 15.5", "font_px = 1e-45")],
        ),
        // The document itself.
        (
            "toml/duplicate-table",
            &[(
                "[render]\nrepaint_nudge = \"log\"\n",
                "[render]\nrepaint_nudge = \"log\"\n[render]\n",
            )],
        ),
        (
            "toml/bad-escape",
            &[("cursor_style = \"bar\"", "cursor_style = \"b\\qar\"")],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(CONFIG, edits)));
    }
    corpus.finish();
}

/// The `FIELDS` list of `T`'s derived table at `path`, as the derive hands it
/// to `deserialize_struct`.
fn fields_at<T: serde::de::DeserializeOwned>(
    path: &[&'static str],
) -> Result<String, &'static str> {
    reader_fields::<T>(path)
        .map(|fields| fields.join(" "))
        .ok_or("not a derived table")
}

/// Every derived table of `T` reachable from `path` through named fields and
/// `Option`s, each recorded as its `FIELDS`.
fn record_tables<T: serde::de::DeserializeOwned>(
    corpus: &mut Corpus,
    name: &str,
    path: &mut Vec<&'static str>,
) {
    let Some(fields) = reader_fields::<T>(path) else {
        return;
    };
    let at = if path.is_empty() {
        "(root)".to_string()
    } else {
        path.join(".")
    };
    corpus.record(format!("{name}/{at}"), Ok::<_, &str>(fields.join(" ")));
    for &field in fields {
        path.push(field);
        record_tables::<T>(corpus, name, path);
        path.pop();
    }
}

#[test]
fn derived_field_lists_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "fields");
    record_tables::<Config>(&mut corpus, "Config", &mut Vec::new());
    record_tables::<RestoreManifest>(&mut corpus, "RestoreManifest", &mut Vec::new());
    record_tables::<SessionHandoff>(&mut corpus, "SessionHandoff", &mut Vec::new());
    record_tables::<CheckpointMeta>(&mut corpus, "CheckpointMeta", &mut Vec::new());
    record_tables::<aterm_policy::Policy>(&mut corpus, "Policy", &mut Vec::new());
    record_tables::<atpkg::manifest::Index>(&mut corpus, "Index", &mut Vec::new());
    // Under `flatten` the derive asks for a map, not a struct: no FIELDS.
    corpus.record("Status/(root)", fields_at::<atpkg::status::Status>(&[]));
    // NEGATIVE CONTROL: not a derived table at all.
    corpus.record("Vec<String>/(root)", fields_at::<Vec<String>>(&[]));
    corpus.record("Config/keybindings", fields_at::<Config>(&["keybindings"]));
    corpus.record("Config/font_px", fields_at::<Config>(&["font_px"]));
    corpus.finish();
}

/// A layout shaped like a 0.104.0 crash journal (2026-10-15, values made up),
/// with a Settings leaf carrying an unsaved draft.
const LAYOUT: &str = r#"schema = 2

[[windows]]
rows = 47
cols = 127
active_tab = 0
outer_x = 1170
outer_y = 110
inner_px = [1954, 1424]
active_item = 0

[windows.show]
fullscreen = false
minimized = false
z_order = 0
key = true

[[windows.tabs]]
kind = "leaf"
cwd = "/home/dev/src"
title = "◐ audit"
focused = true
local_id = 0

[[windows.native_tabs]]
kind = "settings"
route = "/appearance"

[[windows.tab_order]]
kind = "terminal"
index = 0

[[windows.tab_order]]
kind = "native"
index = 0

[[windows.restored_tabs]]
zoomed = false
focused_path = ["second"]

[windows.restored_tabs.root]
node = "split"
axis = "vertical"
ratio = 0.61

[windows.restored_tabs.root.first]
node = "leaf"

[windows.restored_tabs.root.first.view]
kind = "terminal"
cwd = "/home/dev/src"
title = "◐ audit"
local_id = 0

[windows.restored_tabs.root.first.view.agent]
pid = 1033
start = "Thu Oct 15 20:36:32 2026"
program = "/opt/agents/claude/bin/claude"
argv = ["/opt/agents/claude/bin/claude", "--resume"]
session = "00000000-0000-4000-8000-000000000000"
cwd = "/home/dev/src"
version = "2.1.301"

[windows.restored_tabs.root.second]
node = "leaf"

[windows.restored_tabs.root.second.view]
kind = "native"
restore_tag = "settings"
route = "/appearance"
settings_drafts = [{ key = "font_px", text = "15" }]

[journal]
pid = 663
build = "0.104.0 (1792088595)"
written_unix_ms = 1792108451973
stamps_quit = true
"#;

/// A layout schema 1 wrote: terminal tabs only, no order, no restored tabs.
const LEGACY_LAYOUT: &str = r#"schema = 1

[[windows]]
rows = 24
cols = 80
active_tab = 0

[[windows.tabs]]
kind = "leaf"
cwd = "/tmp"
title = "shell"
focused = true
"#;

#[test]
fn restore_layouts_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "restore");
    let read = |doc: &str| {
        aterm_toml::from_str::<RestoreManifest>(doc)
            .map_err(|e| e.to_string())
            .and_then(|layout| layout.to_toml())
    };
    corpus.record("base", read(LAYOUT));
    corpus.record("legacy-schema-1", read(LEGACY_LAYOUT));
    corpus.record("missing-key/windows", read("schema = 2\n"));
    let cases: &[(&str, &[(&str, &str)])] = &[
        // Internally tagged: the tag missing, unknown, mistyped, repeated.
        (
            "tag/missing-node",
            &[(
                "[windows.restored_tabs.root.second]\nnode = \"leaf\"\n",
                "[windows.restored_tabs.root.second]\n",
            )],
        ),
        (
            "tag/unknown-node",
            &[("node = \"split\"", "node = \"tree\"")],
        ),
        ("tag/node-int", &[("node = \"split\"", "node = 1")]),
        (
            "tag/missing-kind",
            &[("kind = \"native\"\nrestore_tag", "restore_tag")],
        ),
        (
            "tag/unknown-kind",
            &[(
                "kind = \"native\"\nrestore_tag",
                "kind = \"browser\"\nrestore_tag",
            )],
        ),
        (
            "tag/kind-case",
            &[(
                "kind = \"native\"\nrestore_tag",
                "kind = \"Native\"\nrestore_tag",
            )],
        ),
        (
            "tag/placeholder-direct",
            &[(
                "kind = \"native\"\nrestore_tag = \"settings\"\nroute = \"/appearance\"\nsettings_drafts = [{ key = \"font_px\", text = \"15\" }]\n",
                "kind = \"placeholder\"\nrestore_tag = \"future.diagram\"\nreason = \"unavailable\"\n",
            )],
        ),
        (
            "tag/placeholder-missing-reason",
            &[(
                "kind = \"native\"\nrestore_tag = \"settings\"\nroute = \"/appearance\"\nsettings_drafts = [{ key = \"font_px\", text = \"15\" }]\n",
                "kind = \"placeholder\"\nrestore_tag = \"future.diagram\"\n",
            )],
        ),
        (
            "tag/pane-missing-kind",
            &[("[[windows.tabs]]\nkind = \"leaf\"\n", "[[windows.tabs]]\n")],
        ),
        // Tagged with deny_unknown_fields: an extra key refuses the entry.
        (
            "deny/native-tab-extra-key",
            &[(
                "route = \"/appearance\"\n\n[[windows.tab_order]]",
                "route = \"/appearance\"\npinned = true\n\n[[windows.tab_order]]",
            )],
        ),
        (
            "deny/native-tab-missing-route",
            &[(
                "[[windows.native_tabs]]\nkind = \"settings\"\nroute = \"/appearance\"\n",
                "[[windows.native_tabs]]\nkind = \"settings\"\n",
            )],
        ),
        (
            "deny/native-tab-unknown-kind",
            &[(
                "[[windows.native_tabs]]\nkind = \"settings\"",
                "[[windows.native_tabs]]\nkind = \"browser\"",
            )],
        ),
        (
            "deny/tab-order-extra-key",
            &[(
                "kind = \"terminal\"\nindex = 0\n",
                "kind = \"terminal\"\nindex = 0\nwidth = 3\n",
            )],
        ),
        (
            "deny/tab-order-index-string",
            &[(
                "kind = \"terminal\"\nindex = 0\n",
                "kind = \"terminal\"\nindex = \"0\"\n",
            )],
        ),
        // Not denied: an unknown key in a leaf or a window is dropped.
        (
            "unknown-key/terminal-view",
            &[(
                "local_id = 0\n\n[windows.restored_tabs.root.first.view.agent]",
                "local_id = 0\nbadge = \"x\"\n\n[windows.restored_tabs.root.first.view.agent]",
            )],
        ),
        (
            "unknown-key/window",
            &[("active_item = 0\n", "active_item = 0\nspace = 3\n")],
        ),
        // The untagged lenient drafts reader.
        (
            "drafts/string",
            &[(
                "settings_drafts = [{ key = \"font_px\", text = \"15\" }]",
                "settings_drafts = \"font_px\"",
            )],
        ),
        (
            "drafts/mixed",
            &[(
                "settings_drafts = [{ key = \"font_px\", text = \"15\" }]",
                "settings_drafts = [{ key = \"font_px\", text = \"15\" }, 5, { key = 1, text = \"x\" }, { key = \"gpu\" }, [\"theme\", \"dark\"]]",
            )],
        ),
        (
            "drafts/empty",
            &[(
                "settings_drafts = [{ key = \"font_px\", text = \"15\" }]",
                "settings_drafts = []",
            )],
        ),
        (
            "drafts/table",
            &[(
                "settings_drafts = [{ key = \"font_px\", text = \"15\" }]",
                "settings_drafts = { key = \"font_px\", text = \"15\" }",
            )],
        ),
        // Required keys.
        ("missing-key/rows", &[("rows = 47\n", "")]),
        ("missing-key/split-ratio", &[("ratio = 0.61\n", "")]),
        ("missing-key/defaulted-zoomed", &[("zoomed = false\n", "")]),
        // Shapes and widths.
        (
            "fixed-array/inner-px-three",
            &[("inner_px = [1954, 1424]", "inner_px = [1954, 1424, 2]")],
        ),
        (
            "fixed-array/inner-px-negative",
            &[("inner_px = [1954, 1424]", "inner_px = [-1954, 1424]")],
        ),
        ("int/rows-u16-overflow", &[("rows = 47", "rows = 65536")]),
        (
            "int/outer-x-i32-overflow",
            &[("outer_x = 1170", "outer_x = 2147483648")],
        ),
        (
            "int/outer-x-i32-min",
            &[("outer_x = 1170", "outer_x = -2147483648")],
        ),
        ("int/z-order-negative", &[("z_order = 0", "z_order = -1")]),
        ("zero/rows-negative-int", &[("rows = 47", "rows = -0")]),
        (
            "zero/ratio-negative-float",
            &[("ratio = 0.61", "ratio = -0.0")],
        ),
        ("float/ratio-nan", &[("ratio = 0.61", "ratio = nan")]),
        ("float/ratio-inf", &[("ratio = 0.61", "ratio = inf")]),
        ("float/ratio-int", &[("ratio = 0.61", "ratio = 1")]),
        (
            "enum/branch-unknown",
            &[("focused_path = [\"second\"]", "focused_path = [\"third\"]")],
        ),
        (
            "enum/axis-case",
            &[("axis = \"vertical\"", "axis = \"Vertical\"")],
        ),
        (
            "array-for-table/show",
            &[(
                "[windows.show]\nfullscreen = false\nminimized = false\nz_order = 0\nkey = true\n",
                "show = [false, false, 0, true]\n",
            )],
        ),
        (
            "array-for-table/agent",
            &[(
                "[windows.restored_tabs.root.first.view.agent]\npid = 1033\nstart = \"Thu Oct 15 20:36:32 2026\"\nprogram = \"/opt/agents/claude/bin/claude\"\nargv = [\"/opt/agents/claude/bin/claude\", \"--resume\"]\nsession = \"00000000-0000-4000-8000-000000000000\"\ncwd = \"/home/dev/src\"\nversion = \"2.1.301\"\n",
                "agent = [1033]\n",
            )],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(LAYOUT, edits)));
    }
    // The real cache's shape (cell-metrics.toml, 0.104.0).
    let cache = |doc: &str| {
        aterm_toml::from_str::<CellMetricsCache>(doc)
            .map_err(|e| e.to_string())
            .and_then(|cache| aterm_toml::to_string(&cache).map_err(|e| e.to_string()))
    };
    let metrics = "schema = 1\nfont_key = \"|lh=1\"\n\n[[entries]]\nscale_milli = 2000\nfont_px = 24.0\ncell_w = 15\ncell_h = 28\n";
    corpus.record("cell-metrics/written-by-0.104.0", cache(metrics));
    corpus.record(
        "cell-metrics/font-px-int",
        cache(&mutate(metrics, &[("font_px = 24.0", "font_px = 24")])),
    );
    corpus.record(
        "cell-metrics/cell-w-negative",
        cache(&mutate(metrics, &[("cell_w = 15", "cell_w = -15")])),
    );
    corpus.finish();
}

/// A small manifest shaped like a 0.104.0 producer's, every lenient field set.
const HANDOFF: &str = r#"schema = 1
next_turn_id = 4096
outgoing_build = 1792088595
roster_seq = 7
fabric_attached = ["s-1"]

[[sessions]]
local_id = 0
sid = "s-1"
state = "alive"
title = "zsh"
fg_holder = 40000
hold = "halt reason=manual"
supervisor = "ops 30000"
claim_known = true
attention_owners = ["ops 50%", "ci 10%"]
render_debt = "1,2"
timeline_id = 65560
journal_key = "663:1"
"#;

#[test]
fn handoff_manifests_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "handoff");
    let read = |doc: &str| {
        aterm_toml::from_str::<SessionHandoff>(doc)
            .map_err(|e| e.to_string())
            .and_then(|manifest| manifest.to_toml())
    };
    corpus.record("base", read(HANDOFF));
    let cases: &[(&str, &[(&str, &str)])] = &[
        // Each lenient reader: a value of the wrong type is unreadable, never an error.
        (
            "lenient/hold-int",
            &[("hold = \"halt reason=manual\"", "hold = 5")],
        ),
        (
            "lenient/hold-table",
            &[("hold = \"halt reason=manual\"", "hold = { why = \"x\" }")],
        ),
        (
            "lenient/supervisor-array",
            &[("supervisor = \"ops 30000\"", "supervisor = [\"ops\"]")],
        ),
        (
            "lenient/claim-string",
            &[("claim_known = true", "claim_known = \"true\"")],
        ),
        (
            "lenient/claim-int",
            &[("claim_known = true", "claim_known = 1")],
        ),
        (
            "lenient/attention-mixed",
            &[(
                "attention_owners = [\"ops 50%\", \"ci 10%\"]",
                "attention_owners = [\"ops 50%\", 5, { x = 1 }, [\"ci\"], \"ci 10%\"]",
            )],
        ),
        (
            "lenient/attention-string",
            &[(
                "attention_owners = [\"ops 50%\", \"ci 10%\"]",
                "attention_owners = \"ops 50%\"",
            )],
        ),
        (
            "lenient/render-debt-int",
            &[("render_debt = \"1,2\"", "render_debt = 12")],
        ),
        (
            "lenient/timeline-negative",
            &[("timeline_id = 65560", "timeline_id = -5")],
        ),
        (
            "lenient/timeline-string",
            &[("timeline_id = 65560", "timeline_id = \"7\"")],
        ),
        (
            "lenient/timeline-float",
            &[("timeline_id = 65560", "timeline_id = 7.0")],
        ),
        (
            "lenient/timeline-nan",
            &[("timeline_id = 65560", "timeline_id = nan")],
        ),
        (
            "lenient/timeline-negative-zero",
            &[("timeline_id = 65560", "timeline_id = -0")],
        ),
        (
            "lenient/fabric-int",
            &[(
                "journal_key = \"663:1\"\n",
                "journal_key = \"663:1\"\nfabric = 5\n",
            )],
        ),
        (
            "lenient/fabric-inline",
            &[(
                "journal_key = \"663:1\"\n",
                "journal_key = \"663:1\"\nfabric = { last_post_id = 7, posts = [{ id = 7, to = \"@s-peer\", kind = \"note\", body = \"hi\" }], receipts = [{ rid = 1, off = 2, verdict = \"refused\", kind = \"ask\", from = \"@s-ops\" }] }\n",
            )],
        ),
        (
            "lenient/fabric-attached-int",
            &[("fabric_attached = [\"s-1\"]", "fabric_attached = 5")],
        ),
        // Not lenient: these still refuse the manifest.
        (
            "strict/local-id-string",
            &[("local_id = 0", "local_id = \"0\"")],
        ),
        (
            "strict/fg-holder-i32-overflow",
            &[("fg_holder = 40000", "fg_holder = 2147483648")],
        ),
        (
            "strict/journal-key-int",
            &[("journal_key = \"663:1\"", "journal_key = 663")],
        ),
        (
            "strict/next-turn-negative",
            &[("next_turn_id = 4096", "next_turn_id = -1")],
        ),
        // Missing keys, and the Option without `default`.
        ("missing-key/sid", &[("sid = \"s-1\"\n", "")]),
        ("missing-key/sessions", &[("[[sessions]]", "[[sessions_]]")]),
        (
            "missing-key/defaulted-everything",
            &[(
                "hold = \"halt reason=manual\"\nsupervisor = \"ops 30000\"\nclaim_known = true\nattention_owners = [\"ops 50%\", \"ci 10%\"]\nrender_debt = \"1,2\"\ntimeline_id = 65560\njournal_key = \"663:1\"\n",
                "",
            )],
        ),
        (
            "unknown-key/session",
            &[("title = \"zsh\"\n", "title = \"zsh\"\nmood = \"🐚\"\n")],
        ),
        (
            "unknown-key/root",
            &[("roster_seq = 7\n", "roster_seq = 7\ncarry = \"repaint\"\n")],
        ),
        (
            "table-for-array/sessions",
            &[("[[sessions]]", "[sessions]")],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(HANDOFF, edits)));
    }
    // A carry that reads goes through the untagged reader's buffer whole:
    // nested posts and receipts, integers buffered and then read as `u64`. A
    // carry that does not read is dropped, the manifest kept.
    let fabric_cases: &[(&str, &[(&str, &str)])] = &[
        ("lenient/fabric-readable", &[]),
        (
            "lenient/fabric-unknown-key",
            &[
                ("last_msg_id = 3\n", "last_msg_id = 3\nlane = \"x\"\n"),
                ("id = 6\n", "id = 6\nurgent = true\n"),
            ],
        ),
        ("lenient/fabric-post-missing-body", &[("body = \"\"\n", "")]),
        (
            "lenient/fabric-receipt-off-string",
            &[("off = 9", "off = \"9\"")],
        ),
        (
            "lenient/fabric-counter-negative",
            &[("last_post_id = 8", "last_post_id = -1")],
        ),
    ];
    for (id, edits) in fabric_cases {
        corpus.record(*id, read(&format!("{HANDOFF}{}", mutate(FABRIC, edits))));
    }
    corpus.finish();
}

/// A session's carried mailbox, appended to [`HANDOFF`]'s one session: every
/// counter, a post with each optional field and one with none, an owed receipt.
const FABRIC: &str = r#"
[sessions.fabric]
last_msg_id = 3
last_post_id = 8
last_receipt_id = 2
uncarried = [8]
receipts_uncarried = 1

[[sessions.fabric.posts]]
id = 7
to = "@s-peer"
kind = "ask"
re = 4
dl = 1792088600
via = "relay"
key = "k-1"
body = "hi \u001B[31mred"

[[sessions.fabric.posts]]
id = 6
to = "@s-ops"
kind = "note"
body = ""

[[sessions.fabric.receipts]]
rid = 2
off = 9
verdict = "handled"
kind = "ask"
from = "@s-ops"
"#;

/// One meta a shipped producer wrote, for the mutations below.
const META_FIXTURE: &str = "v0.104.0/claude-code-1049/s0.meta.json";

#[test]
fn checkpoint_metas_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "checkpoint-meta");
    let read = |doc: &str| {
        aterm_json::from_str::<CheckpointMeta>(doc)
            .map_err(|e| e.to_string())
            .and_then(|meta| aterm_json::to_string(&meta).map_err(|e| e.to_string()))
    };
    let base = checkout_text(&fixtures().join(META_FIXTURE));
    corpus.record("base", read(&base));
    let edit = |edits: &[(&str, &str)]| -> String {
        let mut doc = base.clone();
        for (from, to) in edits {
            assert_eq!(
                doc.matches(from).count(),
                1,
                "{from:?} must occur once in {META_FIXTURE}"
            );
            doc = doc.replacen(from, to, 1);
        }
        doc
    };
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key/root",
            &[("{\"rows\":30,", "{\"future\":{\"a\":[1]},\"rows\":30,")],
        ),
        ("missing-key/rows", &[("{\"rows\":30,", "{")]),
        (
            "missing-key/defaulted-history",
            &[("\"history_lines\":0,", "")],
        ),
        (
            "wrong-type/rows-string",
            &[("{\"rows\":30,", "{\"rows\":\"30\",")],
        ),
        (
            "wrong-type/rows-float",
            &[("{\"rows\":30,", "{\"rows\":30.0,")],
        ),
        (
            "int/rows-u16-overflow",
            &[("{\"rows\":30,", "{\"rows\":65536,")],
        ),
        ("int/rows-negative", &[("{\"rows\":30,", "{\"rows\":-1,")]),
        (
            "zero/rows-negative-int",
            &[("{\"rows\":30,", "{\"rows\":-0,")],
        ),
        (
            "zero/rows-negative-float",
            &[("{\"rows\":30,", "{\"rows\":-0.0,")],
        ),
        (
            "float/rows-infinite",
            &[("{\"rows\":30,", "{\"rows\":1e999,")],
        ),
        (
            "float/rows-nan-literal",
            &[("{\"rows\":30,", "{\"rows\":NaN,")],
        ),
        (
            "int/counter-u64-max",
            &[(
                "\"absolute_row_counter\":30,",
                "\"absolute_row_counter\":18446744073709551615,",
            )],
        ),
        (
            "int/counter-u64-overflow",
            &[(
                "\"absolute_row_counter\":30,",
                "\"absolute_row_counter\":18446744073709551616,",
            )],
        ),
        // The lenient readers: title, parser, output state.
        ("lenient/title-int", &[("\"title\":{}", "\"title\":5")]),
        (
            "lenient/title-wrong-field",
            &[("\"title\":{}", "\"title\":{\"window\":5}")],
        ),
        (
            "lenient/title-set",
            &[(
                "\"title\":{}",
                "\"title\":{\"window\":\"zsh\",\"icon\":\"zsh\"}",
            )],
        ),
        (
            "lenient/output-state-array",
            &[("\"output_state\":{\"rep\":\"s\"}", "\"output_state\":[1]")],
        ),
        (
            "lenient/output-state-wrong-field",
            &[(
                "\"output_state\":{\"rep\":\"s\"}",
                "\"output_state\":{\"rep\":5}",
            )],
        ),
        (
            "lenient/parser-string",
            &[(
                "\"output_state\":{\"rep\":\"s\"}",
                "\"output_state\":{\"rep\":\"s\"},\"parser\":\"csi\"",
            )],
        ),
        (
            "lenient/parser-null",
            &[(
                "\"output_state\":{\"rep\":\"s\"}",
                "\"output_state\":{\"rep\":\"s\"},\"parser\":null",
            )],
        ),
        // Externally tagged unit enums.
        (
            "enum/cursor-style-unknown",
            &[(
                "\"cursor_style\":\"BlinkingBlock\"",
                "\"cursor_style\":\"Blinking\"",
            )],
        ),
        (
            "enum/cursor-style-object",
            &[(
                "\"cursor_style\":\"BlinkingBlock\"",
                "\"cursor_style\":{\"BlinkingBlock\":null}",
            )],
        ),
        (
            "enum/cursor-style-object-extra",
            &[(
                "\"cursor_style\":\"BlinkingBlock\"",
                "\"cursor_style\":{\"BlinkingBlock\":null,\"SteadyBar\":null}",
            )],
        ),
        (
            "enum/charset-int",
            &[(
                "\"gl\":\"G0\",\"single_shift\":\"None\",\"gr\":\"G2\",\"g1_96\":null,\"g2_96\":null,\"g3_96\":null},\"pending_wrap\"",
                "\"gl\":0,\"single_shift\":\"None\",\"gr\":\"G2\",\"g1_96\":null,\"g2_96\":null,\"g3_96\":null},\"pending_wrap\"",
            )],
        ),
        // A struct written as an array: the derive takes it positionally.
        (
            "array-for-table/xterm-keyboard",
            &[(
                "\"xterm_keyboard\":{\"modify_other_keys\":0,\"format_other_keys\":0}",
                "\"xterm_keyboard\":[2,1]",
            )],
        ),
        (
            "array-for-table/xterm-keyboard-short",
            &[(
                "\"xterm_keyboard\":{\"modify_other_keys\":0,\"format_other_keys\":0}",
                "\"xterm_keyboard\":[2]",
            )],
        ),
        (
            "duplicate-key/rows",
            &[("{\"rows\":30,", "{\"rows\":30,\"rows\":31,")],
        ),
        (
            "shape/trailing-garbage",
            &[(
                "\"output_state\":{\"rep\":\"s\"}}",
                "\"output_state\":{\"rep\":\"s\"}}x",
            )],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&edit(edits)));
    }
    corpus.finish();
}

/// A fixture as its producer wrote it. A Windows checkout under
/// `core.autocrlf` hands it over CRLF; the producers escape a carriage return
/// in JSON and TOML alike, so a raw CRLF is only ever the checkout's.
fn checkout_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/handoff")
}

/// Every file under `dir`, recursively, in name order.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("a fixture directory")
        .map(|entry| entry.expect("a fixture entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// THE BYTES OLD BUILDS WROTE: every handoff manifest, layout and checkpoint
/// meta each shipped release's producer committed under `tests/fixtures/
/// handoff/`, read by this build's models and written back out.
#[test]
fn state_written_by_shipped_producers_answers_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "shipped-state");
    let root = fixtures();
    let mut files = Vec::new();
    walk(&root, &mut files);
    let (mut manifests, mut layouts, mut metas) = (0, 0, 0);
    // Every `Ok` here is pinned by digest: the bytes are the fixture's, read
    // and written back, and a drift is found by re-running one desk.
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        // The id names the desk with `/` whatever the host's separator, so a
        // Windows run reads the same answers file.
        let desk = path
            .parent()
            .and_then(|dir| dir.strip_prefix(&root).ok())
            .expect("a desk under the root")
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let read_text = || checkout_text(&path);
        if name.ends_with(".meta.json") {
            metas += 1;
            let meta = aterm_json::from_str::<CheckpointMeta>(&read_text())
                .map_err(|e| e.to_string())
                .and_then(|meta| aterm_json::to_string(&meta).map_err(|e| e.to_string()));
            corpus.record(
                format!("{desk}/{name}"),
                meta.map(|bytes| answers::digest(&bytes)),
            );
        } else if name.starts_with("seamless-") && name.ends_with(".layout.toml") {
            layouts += 1;
            let layout = aterm_toml::from_str::<RestoreManifest>(&read_text())
                .map_err(|e| e.to_string())
                .and_then(|layout| layout.to_toml());
            corpus.record(
                format!("{desk}/layout"),
                layout.map(|bytes| answers::digest(&bytes)),
            );
        } else if name.starts_with("seamless-") && name.ends_with(".toml") {
            manifests += 1;
            // The first line binds the file to its attempt's nonce; the TOML
            // follows it (`seamless`'s reader splits them the same way).
            let text = read_text();
            let (_nonce, toml) = text.split_once('\n').unwrap_or(("", ""));
            let manifest = aterm_toml::from_str::<SessionHandoff>(toml)
                .map_err(|e| e.to_string())
                .and_then(|manifest| manifest.to_toml());
            corpus.record(
                format!("{desk}/manifest"),
                manifest.map(|bytes| answers::digest(&bytes)),
            );
        }
    }
    // A fixture tree that moved would leave nothing of a kind to read (the
    // answers file names every fixture it read, so a lost one is a red too).
    assert!(
        manifests > 0 && layouts > 0 && metas > 0,
        "the shipped-producer sweep read {manifests} manifests, {layouts} layouts, {metas} metas"
    );
    // The negative control the harness asks for: a manifest whose schema line
    // is torn, as a truncated write would leave it.
    corpus.record(
        "negative-control/torn-manifest",
        aterm_toml::from_str::<SessionHandoff>("schema = ").map(|_| String::new()),
    );
    corpus.finish();
}

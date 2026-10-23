// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed index, a package manifest and `status.toml` under TOML mutations.
//! `status.toml` carries unknown keys through `#[serde(flatten)]` into a map of
//! `aterm_toml::Value`, TOML datetimes included, so an older or newer build's
//! keys survive each rewrite; the index and manifests ignore unknown keys and
//! default most of the rest.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
pub(crate) mod answers;

use answers::Corpus;

use crate::manifest::{Index, PkgManifest};
use crate::status::Status;

fn mutate(base: &str, edits: &[(&str, &str)]) -> String {
    let mut doc = base.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

/// `status.toml` as a 0.104.0 build writes it, two keys of earlier builds
/// (`last_index_reached_at`, `metered_hold_until`) riding the flatten.
const STATUS: &str = r#"schema = 1
updated_at = "2026-10-15T21:46:49Z"
enabled = true
index_source = "alabsystems/aterm"
outcome = "codex 0.162.1 → 0.163.0 (OpenAI latest)"
seams = ["rustup:trust"]
last_success_at = "2026-10-15T20:38:59Z"
last_index_build = 70
index_build_changed_at = "2026-10-15T20:38:59Z"
last_pass = "ok"
last_pass_at = "2026-10-15T20:38:59.000000000Z"
last_pass_attempted_index_build = 70
last_pass_attempted_at = "2026-10-15T20:38:59.000000000Z"
pass_seq = 98
stale_index_only_pass_seq = 0
last_index_reached_at = "2026-09-25T03:17:47Z"
metered_hold_until = ""

[programs]

[programs.ay]
installed_build = 8256
state = "managed 8256 — pinned by index 70"
tree_root = "bec64277a29e884b8a20a12ed5de4908d5f157e414af3aa8cff00c34ddfb1ef5"

[programs.claude]
installed_build = 1000002000001000301
state = "managed 2.1.301 — Anthropic latest"
tree_root = "6038b1f5b21832d15f8e7b21ffbd2429dc52bb55cdab4797933686c275041ddc"
"#;

#[test]
fn status_records_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "status");
    let read = |doc: &str| {
        aterm_toml::from_str::<Status>(doc)
            .map_err(|e| e.to_string())
            .and_then(|status| status.to_toml())
    };
    corpus.record("written-by-0.104.0", read(STATUS));
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "extra/offset-datetime",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = 2026-10-15T20:38:59Z\n",
            )],
        ),
        (
            "extra/offset-datetime-fraction",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = 2026-10-15T20:38:59.123456789+02:00\n",
            )],
        ),
        (
            "extra/local-datetime",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = 2026-10-15T20:38:59\n",
            )],
        ),
        (
            "extra/local-date",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = 2026-10-15\n",
            )],
        ),
        (
            "extra/local-time",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = 20:38:59.5\n",
            )],
        ),
        (
            "extra/datetime-array",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = [2026-10-15, 2026-10-16T00:00:00Z]\n",
            )],
        ),
        (
            "extra/inline-table",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = { at = 2026-10-15T20:38:59Z, why = \"metered\" }\n",
            )],
        ),
        (
            "extra/table",
            &[(
                "[programs]\n",
                "[future]\nlevel = 2\nseen = 2026-10-15T20:38:59Z\n\n[programs]\n",
            )],
        ),
        (
            "extra/array-of-tables",
            &[(
                "[programs]\n",
                "[[future_rows]]\nn = 1\n[[future_rows]]\nn = -0\n\n[programs]\n",
            )],
        ),
        (
            "extra/float-specials",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = [nan, inf, -inf, -0.0, 1e300]\n",
            )],
        ),
        (
            "extra/integers",
            &[(
                "metered_hold_until = \"\"\n",
                "metered_hold_until = [9223372036854775807, -9223372036854775808, 0x1f, -0]\n",
            )],
        ),
        (
            "extra/quoted-dotted-key",
            &[("metered_hold_until = \"\"\n", "\"a.b\" = 1\n")],
        ),
        (
            "collision/key-named-extra",
            &[("metered_hold_until = \"\"\n", "extra = { a = 1 }\n")],
        ),
        (
            "collision/known-field-wrong-type",
            &[("pass_seq = 98", "pass_seq = \"98\"")],
        ),
        (
            "collision/known-field-datetime",
            &[(
                "updated_at = \"2026-10-15T21:46:49Z\"",
                "updated_at = 2026-10-15T21:46:49Z",
            )],
        ),
        (
            "collision/known-field-other-case",
            &[("metered_hold_until = \"\"\n", "Pass_Seq = 3\n")],
        ),
        ("missing-key/schema", &[("schema = 1\n", "")]),
        ("missing-key/enabled", &[("enabled = true\n", "")]),
        ("missing-key/defaulted-pass-seq", &[("pass_seq = 98\n", "")]),
        (
            "missing-key/program-state",
            &[("state = \"managed 8256 — pinned by index 70\"\n", "")],
        ),
        (
            "missing-key/program-build",
            &[("installed_build = 8256\n", "")],
        ),
        (
            "unknown-key/program-row",
            &[(
                "installed_build = 8256\n",
                "installed_build = 8256\nchannel = \"stable\"\n",
            )],
        ),
        (
            "wrong-type/enabled-string",
            &[("enabled = true", "enabled = \"true\"")],
        ),
        (
            "wrong-type/seams-string",
            &[("seams = [\"rustup:trust\"]", "seams = \"rustup:trust\"")],
        ),
        (
            "array-for-table/programs",
            &[(
                "[programs]\n\n[programs.ay]",
                "[[programs]]\n\n[programs.ay]",
            )],
        ),
        (
            "array-for-table/program-row",
            &[(
                "[programs.ay]\ninstalled_build = 8256\nstate = \"managed 8256 — pinned by index 70\"\ntree_root = \"bec64277a29e884b8a20a12ed5de4908d5f157e414af3aa8cff00c34ddfb1ef5\"\n",
                "ay = [8256, \"managed\", \"x\"]\n",
            )],
        ),
        (
            "int/pass-seq-i64-max",
            &[("pass_seq = 98", "pass_seq = 9223372036854775807")],
        ),
        (
            "int/pass-seq-negative",
            &[("pass_seq = 98", "pass_seq = -1")],
        ),
        (
            "int/schema-u32-overflow",
            &[("schema = 1", "schema = 4294967296")],
        ),
        (
            "int/build-u64-past-i64",
            &[(
                "installed_build = 8256",
                "installed_build = 18446744073709551615",
            )],
        ),
        (
            "zero/pass-seq-negative-int",
            &[("pass_seq = 98", "pass_seq = -0")],
        ),
        (
            "zero/pass-seq-negative-float",
            &[("pass_seq = 98", "pass_seq = -0.0")],
        ),
        ("float/pass-seq-nan", &[("pass_seq = 98", "pass_seq = nan")]),
        ("float/pass-seq-inf", &[("pass_seq = 98", "pass_seq = inf")]),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(STATUS, edits)));
    }
    let programs = &STATUS[STATUS.find("[programs]").expect("the programs table")..];
    corpus.record(
        "collision/programs-scalar",
        read(&mutate(STATUS, &[(programs, "programs = \"none\"\n")])),
    );
    corpus.record("toml/empty", read(""));
    corpus.finish();
}

const INDEX: &str = r#"
schema = 2
index_build = 41
generated_at = "2026-06-28T12:00:00Z"
valid_until = "2026-07-05T12:00:00Z"
machine_id = "m-corpus"
roster_seq = 7

[programs.ay]
repo = "ay"
policy = "prebuilt-or-build"

[programs.trust]
repo = "trust"
policy = "prebuilt-only"
coherence_group = "rustc"

[programs.gh]
repo = "gh"
policy = "prebuilt-only"
system = "gh"

[[channels]]
name = "stable"
channel_build = 137
min_build = 120
yanked = ["trust@4790"]
pin = { trust = 4821, ay = 18, gh = 2026082601 }
pin_by_target = { "x86_64-unknown-linux-gnu" = { trust = 9122 } }

[channels.meta]
nightly = "nightly-2025-12-03"
"#;

const PKG: &str = r#"
schema = 2
program = "claude"
version = "2.1.231"
build_number = 2026082601
exposes = ["claude"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "binary"
protocol = "https"
url = "https://downloads.example/claude/2.1.231/darwin-arm64/claude"
payload = "raw-binary"
entry = "claude"
asset = "claude-2.1.231-darwin-arm64"
sha256 = "7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c"
tree_root = "abc"
size = 230824016
vendor = "Example Vendor"
strip_components = 1
links = { emacs = "Emacs.app/Contents/MacOS/Emacs" }

[artifact.cost]
download_bytes = 230824016
disk_installed = 230824016
"#;

#[test]
fn index_and_manifests_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "index-manifest");
    let index = |doc: &str| aterm_toml::from_str::<Index>(doc).map(|v| format!("{v:?}"));
    let pkg = |doc: &str| aterm_toml::from_str::<PkgManifest>(doc).map(|v| format!("{v:?}"));
    corpus.record("index/base", index(INDEX));
    corpus.record("pkg/base", pkg(PKG));
    let programs_start = INDEX.find("[programs.ay]").expect("the programs tables");
    let channels_start = INDEX.find("[[channels]]").expect("the channels table");
    let programs = &INDEX[programs_start..channels_start];
    let channels = &INDEX[channels_start..];
    let index_cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key/root",
            &[("roster_seq = 7\n", "roster_seq = 7\nsignature = \"x\"\n")],
        ),
        (
            "unknown-key/program",
            &[("system = \"gh\"\n", "system = \"gh\"\nextra = true\n")],
        ),
        ("missing-key/schema", &[("schema = 2\n", "")]),
        ("missing-key/index-build", &[("index_build = 41\n", "")]),
        (
            "missing-key/defaulted-machine",
            &[("machine_id = \"m-corpus\"\nroster_seq = 7\n", "")],
        ),
        ("missing-key/program-repo", &[("repo = \"gh\"\n", "")]),
        ("missing-key/channel-name", &[("name = \"stable\"\n", "")]),
        (
            "wrong-type/index-build-string",
            &[("index_build = 41", "index_build = \"41\"")],
        ),
        (
            "wrong-type/generated-at-datetime",
            &[(
                "generated_at = \"2026-06-28T12:00:00Z\"",
                "generated_at = 2026-06-28T12:00:00Z",
            )],
        ),
        ("wrong-type/pin-string", &[("ay = 18,", "ay = \"18\",")]),
        (
            "wrong-type/yanked-string",
            &[("yanked = [\"trust@4790\"]", "yanked = \"trust@4790\"")],
        ),
        (
            "wrong-type/pin-by-target-flat",
            &[(
                "{ \"x86_64-unknown-linux-gnu\" = { trust = 9122 } }",
                "{ \"x86_64-unknown-linux-gnu\" = 9122 }",
            )],
        ),
        (
            "array-for-table/programs",
            &[(programs, "programs = [[\"ay\"]]\n\n")],
        ),
        (
            "table-for-array/channels",
            &[("[[channels]]\nname", "[channels.first]\nname")],
        ),
        (
            "array-for-table/channel-row",
            &[
                (channels, ""),
                (
                    "roster_seq = 7\n",
                    "roster_seq = 7\nchannels = [[\"stable\", 137]]\n",
                ),
            ],
        ),
        (
            "int/roster-negative",
            &[("roster_seq = 7", "roster_seq = -7")],
        ),
        (
            "int/schema-u32-overflow",
            &[("schema = 2", "schema = 4294967296")],
        ),
        (
            "int/build-i64-overflow",
            &[("index_build = 41", "index_build = 18446744073709551615")],
        ),
        (
            "zero/build-negative-int",
            &[("index_build = 41", "index_build = -0")],
        ),
        (
            "float/min-build-nan",
            &[("min_build = 120", "min_build = nan")],
        ),
        (
            "float/min-build-inf",
            &[("min_build = 120", "min_build = -inf")],
        ),
    ];
    for (id, edits) in index_cases {
        corpus.record(format!("index/{id}"), index(&mutate(INDEX, edits)));
    }
    let pkg_cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key/artifact",
            &[(
                "vendor = \"Example Vendor\"\n",
                "vendor = \"Example Vendor\"\nmirror = \"x\"\n",
            )],
        ),
        (
            "missing-key/target",
            &[("target = \"aarch64-apple-darwin\"\n", "")],
        ),
        (
            "missing-key/defaulted-protocol",
            &[("protocol = \"https\"\n", "")],
        ),
        (
            "missing-key/build-number",
            &[("build_number = 2026082601\n", "")],
        ),
        (
            "wrong-type/links-int",
            &[(
                "links = { emacs = \"Emacs.app/Contents/MacOS/Emacs\" }",
                "links = { emacs = 1 }",
            )],
        ),
        (
            "missing-key/defaulted-cost",
            &[(
                "[artifact.cost]\ndownload_bytes = 230824016\ndisk_installed = 230824016\n",
                "",
            )],
        ),
        (
            "table-for-array/artifact",
            &[("[[artifact]]", "[artifact]")],
        ),
        (
            "array-for-table/cost",
            &[(
                "[artifact.cost]\ndownload_bytes = 230824016\ndisk_installed = 230824016\n",
                "cost = [1, 2, 3]\n",
            )],
        ),
        (
            "int/strip-u32-overflow",
            &[("strip_components = 1", "strip_components = 4294967296")],
        ),
        ("int/size-negative", &[("size = 230824016", "size = -1")]),
        (
            "zero/size-negative-int",
            &[("size = 230824016", "size = -0")],
        ),
        ("float/size-inf", &[("size = 230824016", "size = inf")]),
    ];
    for (id, edits) in pkg_cases {
        corpus.record(format!("pkg/{id}"), pkg(&mutate(PKG, edits)));
    }
    corpus.finish();
}

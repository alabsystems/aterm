// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The escape-sequence policy document under every mutation the first-party
//! serde must answer exactly as registry serde did (docs/AUDIT-third-party-
//! 2026-10-15.md, SP.3). Every table is `deny_unknown_fields`: a key this build
//! does not know refuses the whole policy rather than being ignored, so the
//! answers here are as much the security surface as the parse.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use answers::Corpus;
use aterm_policy::{Policy, profiles};

const BASE: &str = r#"schema_version = 1
profile = "Standard"

[defaults]
unmatched = "warn"
shell_integration_require_nonce = true

[[rules]]
sequence = "OSC 52 set"
origin_min = "User"
response = "ask"
rate_limit = "clipboard-write"
prompt_id = "clipboard"

[[rules]]
sequence = "CSI t"
origin_min = "Host"
response = "drop"

[[rate_limits]]
id = "clipboard-write"
capacity_bytes = 4096
refill_per_second = 512
per_sequence_max = 1024
"#;

/// `BASE` with each `(from, to)` applied; `from` must occur exactly once, so
/// a mutation can never silently leave the document as it was.
fn mutate(edits: &[(&str, &str)]) -> String {
    let mut doc = BASE.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

fn read(doc: &str) -> Result<String, String> {
    let policy: Policy = aterm_toml::from_str(doc).map_err(|e| e.to_string())?;
    policy.to_toml().map_err(|e| format!("re-serialize: {e}"))
}

#[test]
fn policy_documents_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "policy");
    for (name, policy) in [
        ("permissive", profiles::permissive()),
        ("standard", profiles::standard()),
        ("hardened", profiles::hardened()),
    ] {
        let written = policy.to_toml().expect("a built-in profile serializes");
        corpus.record(format!("profile/{name}"), read(&written));
    }
    corpus.record("base", read(BASE));

    let cases: &[(&str, &[(&str, &str)])] = &[
        // Unknown keys, at every level: each table refuses them.
        (
            "unknown-key/root",
            &[("profile = ", "colour = 1\nprofile = ")],
        ),
        (
            "unknown-key/defaults",
            &[(
                "unmatched = \"warn\"\n",
                "unmatched = \"warn\"\nfallback = \"drop\"\n",
            )],
        ),
        (
            "unknown-key/rule",
            &[(
                "response = \"drop\"\n",
                "response = \"drop\"\nnote = \"x\"\n",
            )],
        ),
        (
            "unknown-key/rate-limit",
            &[(
                "per_sequence_max = 1024\n",
                "per_sequence_max = 1024\nburst = 2\n",
            )],
        ),
        ("unknown-key/case", &[("schema_version", "Schema_Version")]),
        // Missing keys: required ones refuse, defaulted ones fill in.
        (
            "missing-key/schema-version",
            &[("schema_version = 1\n", "")],
        ),
        ("missing-key/profile", &[("profile = \"Standard\"\n", "")]),
        (
            "missing-key/defaults-table",
            &[(
                "[defaults]\nunmatched = \"warn\"\nshell_integration_require_nonce = true\n",
                "",
            )],
        ),
        ("missing-key/unmatched", &[("unmatched = \"warn\"\n", "")]),
        (
            "missing-key/defaulted-nonce",
            &[("shell_integration_require_nonce = true\n", "")],
        ),
        (
            "missing-key/rule-sequence",
            &[("sequence = \"CSI t\"\n", "")],
        ),
        (
            "missing-key/rule-optional",
            &[(
                "rate_limit = \"clipboard-write\"\nprompt_id = \"clipboard\"\n",
                "",
            )],
        ),
        (
            "missing-key/rate-capacity",
            &[("capacity_bytes = 4096\n", "")],
        ),
        (
            "missing-key/rate-defaulted",
            &[("per_sequence_max = 1024\n", "")],
        ),
        // Wrong types.
        (
            "wrong-type/schema-string",
            &[("schema_version = 1", "schema_version = \"1\"")],
        ),
        (
            "wrong-type/schema-bool",
            &[("schema_version = 1", "schema_version = true")],
        ),
        (
            "wrong-type/profile-int",
            &[("profile = \"Standard\"", "profile = 2")],
        ),
        ("wrong-type/nonce-string", &[("= true", "= \"yes\"")]),
        (
            "wrong-type/rate-limit-int",
            &[("rate_limit = \"clipboard-write\"", "rate_limit = 7")],
        ),
        (
            "wrong-type/capacity-string",
            &[("capacity_bytes = 4096", "capacity_bytes = \"4096\"")],
        ),
        (
            "wrong-type/capacity-float",
            &[("capacity_bytes = 4096", "capacity_bytes = 4096.0")],
        ),
        (
            "wrong-type/capacity-fraction",
            &[("capacity_bytes = 4096", "capacity_bytes = 1.5")],
        ),
        (
            "wrong-type/rules-string",
            &[(
                "profile = \"Standard\"\n",
                "profile = \"Standard\"\nrules = \"none\"\n",
            )],
        ),
        // A table where a value belongs, and the other way about.
        (
            "array-for-table/defaults",
            &[(
                "[defaults]\nunmatched = \"warn\"\nshell_integration_require_nonce = true\n",
                "defaults = [\"warn\"]\n",
            )],
        ),
        (
            "array-for-table/defaults-empty",
            &[(
                "[defaults]\nunmatched = \"warn\"\nshell_integration_require_nonce = true\n",
                "defaults = []\n",
            )],
        ),
        (
            "table-for-value/response",
            &[("response = \"drop\"", "response = { drop = true }")],
        ),
        (
            "table-for-array/rules",
            &[(
                "[[rules]]\nsequence = \"CSI t\"",
                "[rules.extra]\nsequence = \"CSI t\"",
            )],
        ),
        // Integers at and past each width.
        (
            "int/capacity-u32-max",
            &[("capacity_bytes = 4096", "capacity_bytes = 4294967295")],
        ),
        (
            "int/capacity-u32-overflow",
            &[("capacity_bytes = 4096", "capacity_bytes = 4294967296")],
        ),
        (
            "int/capacity-negative",
            &[("capacity_bytes = 4096", "capacity_bytes = -1")],
        ),
        (
            "int/capacity-i64-max",
            &[(
                "capacity_bytes = 4096",
                "capacity_bytes = 9223372036854775807",
            )],
        ),
        (
            "int/capacity-i64-overflow",
            &[(
                "capacity_bytes = 4096",
                "capacity_bytes = 9223372036854775808",
            )],
        ),
        (
            "int/schema-hex",
            &[("schema_version = 1", "schema_version = 0x1")],
        ),
        (
            "int/refill-underscore",
            &[("refill_per_second = 512", "refill_per_second = 5_12")],
        ),
        // Signed zero and the non-finite floats.
        (
            "zero/negative-int",
            &[("per_sequence_max = 1024", "per_sequence_max = -0")],
        ),
        (
            "zero/negative-float",
            &[("per_sequence_max = 1024", "per_sequence_max = -0.0")],
        ),
        (
            "float/nan",
            &[("refill_per_second = 512", "refill_per_second = nan")],
        ),
        (
            "float/inf",
            &[("refill_per_second = 512", "refill_per_second = inf")],
        ),
        (
            "float/negative-inf",
            &[("refill_per_second = 512", "refill_per_second = -inf")],
        ),
        // Enum spellings: rename_all is exact, never case-folded.
        (
            "enum/unknown-response",
            &[("response = \"drop\"", "response = \"explode\"")],
        ),
        (
            "enum/response-case",
            &[("response = \"drop\"", "response = \"Drop\"")],
        ),
        (
            "enum/profile-case",
            &[("profile = \"Standard\"", "profile = \"standard\"")],
        ),
        (
            "enum/origin-case",
            &[("origin_min = \"Host\"", "origin_min = \"host\"")],
        ),
        (
            "enum/origin-every",
            &[("origin_min = \"Host\"", "origin_min = \"NetworkUntrusted\"")],
        ),
        ("enum/empty", &[("response = \"drop\"", "response = \"\"")]),
        // Strings the model keeps verbatim.
        (
            "string/unicode",
            &[(
                "prompt_id = \"clipboard\"",
                "prompt_id = \"剪贴板 \\u00e9\\t\"",
            )],
        ),
        (
            "string/empty-rate-limit",
            &[("rate_limit = \"clipboard-write\"", "rate_limit = \"\"")],
        ),
        // The document itself.
        (
            "toml/duplicate-key",
            &[(
                "profile = \"Standard\"\n",
                "profile = \"Standard\"\nprofile = \"Hardened\"\n",
            )],
        ),
        (
            "toml/truncated",
            &[("per_sequence_max = 1024\n", "per_sequence_max = ")],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(edits)));
    }
    corpus.record("toml/empty", read(""));
    corpus.finish();
}

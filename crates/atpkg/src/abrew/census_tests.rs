// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A bottled formula record; `cellar` decides relocation.
fn formula(name: &str, cellar: &str, deps: &[&str]) -> Value {
    aterm_json::json!({
        "name": name, "full_name": name, "versions": {"stable": "1.0"}, "revision": 0,
        "dependencies": deps,
        "bottle": {"stable": {"files": {"arm64_tahoe": {"url": "https://x/b.tar.gz", "sha256": SHA, "cellar": cellar}}}},
    })
}

/// A cask record downloading `file`, placing one app.
fn cask(token: &str, file: &str) -> Value {
    aterm_json::json!({
        "token": token, "version": "1.0", "url": format!("https://x/{file}"), "sha256": SHA,
        "artifacts": [{"app": ["Demo.app"]}],
    })
}

fn with(mut record: Value, key: &str, value: Value) -> Value {
    record
        .as_object_mut()
        .unwrap()
        .insert(key.to_string(), value);
    record
}

fn bytes(records: Vec<Value>) -> Vec<u8> {
    aterm_json::to_vec(&Value::Array(records)).unwrap()
}

/// The fixture: relocation-free `zlib-ng` and `tool` (which needs
/// `zlib-ng`), relocating `openssl` and `curl-ish` (relocation-free, but
/// needs `openssl`), a disabled formula, one asking for a newer macOS
/// than the machine runs, relocating `libgit`, and relocation-free
/// `client`, which needs both relocating formulae; a zip cask, a dmg
/// cask, a pkg cask, a zip cask that depends on the relocating formula,
/// and a zip cask that needs macOS 14 or newer.
fn fixture() -> (Vec<u8>, Vec<u8>) {
    let formulae = vec![
        formula("zlib-ng", ":any_skip_relocation", &[]),
        formula("tool", ":any_skip_relocation", &["zlib-ng"]),
        formula("openssl", "/opt/homebrew/Cellar", &[]),
        formula("curl-ish", ":any_skip_relocation", &["openssl"]),
        with(
            formula("old", ":any_skip_relocation", &[]),
            "disabled",
            Value::Bool(true),
        ),
        with(
            formula("future", ":any_skip_relocation", &[]),
            "requirements",
            aterm_json::json!([{"name": "macos", "version": "30", "contexts": [], "specs": ["stable"]}]),
        ),
        formula("libgit", "/opt/homebrew/Cellar", &[]),
        formula("client", ":any_skip_relocation", &["openssl", "libgit"]),
    ];
    let casks = vec![
        cask("zipped", "Demo.zip"),
        cask("imaged", "Demo.dmg"),
        with(
            cask("installer", "Setup.zip"),
            "artifacts",
            aterm_json::json!([{"pkg": ["Setup.pkg"]}]),
        ),
        with(
            cask("needs-openssl", "Needs.zip"),
            "depends_on",
            aterm_json::json!({"formula": ["openssl"]}),
        ),
        with(
            cask("floored", "Floored.zip"),
            "depends_on",
            aterm_json::json!({"macos": {">=": ["14"]}}),
        ),
    ];
    (bytes(formulae), bytes(casks))
}

fn tahoe() -> Machine {
    Machine::mac("arm64", "26.1", Some("26.1"), true)
}

#[test]
fn the_census_counts_what_admission_decides() {
    let (formulae, casks) = fixture();
    let census = measure(&formulae, &casks, &tahoe()).unwrap();
    assert_eq!((census.formulae, census.casks), (8, 5));
    assert_eq!(
        census.solo.admitted,
        ["zlib-ng", "tool", "curl-ish", "client"]
    );
    assert_eq!(
        census.solo.refusals,
        BTreeMap::from([
            ("<name> needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(), (2, 2)),
            ("<name> needs a newer macOS".to_string(), (1, 1)),
            ("abrew: <name> is disabled upstream".to_string(), (1, 1)),
        ])
    );
    assert_eq!(census.closure.admitted, ["zlib-ng", "tool"]);
    assert_eq!(
        census.closure.refusals,
        BTreeMap::from([
            (
                "openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(),
                (2, 1)
            ),
            (
                "libgit needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(),
                (1, 0)
            ),
        ])
    );
    assert_eq!(
        census.solo.admitted.len(),
        census.closure.admitted.len() + census.closure.refused
    );
    assert_eq!(census.cask.admitted, ["zipped", "imaged", "floored"]);
    assert_eq!(
        census.cask.ranked(),
        [
            (
                "cask <name> ships a pkg installer (root work abrew never does itself)",
                1,
                1
            ),
            (
                "openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)",
                1,
                1
            ),
        ]
    );
    let text = census.text();
    assert!(
        text.contains("formulae: 4/8 solo (50.0%), 2 with closure (25.0%)\n"),
        "{text}"
    );
    assert!(
        text.contains("  admitted solo, refused with closure: 2\n   openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar): 2 (1 alone)\n"),
        "{text}"
    );
    assert!(text.contains("casks: 3/5 (60.0%)\n"), "{text}");
    let json = census.json();
    assert_eq!(
        json.pointer("/formulae/closure").and_then(Value::as_u64),
        Some(2)
    );
    assert_eq!(
        json.pointer("/casks/admitted").and_then(Value::as_u64),
        Some(3)
    );
}

/// Negative control: the same records judged for other machines get
/// other verdicts, so the counts above are admission's decisions for
/// the machine modelled, not the fixture's shape. Big Sur has no bottle
/// the relocation-free formulae were built for and is older than
/// `floored`'s floor; macOS 30 meets `future`'s.
#[test]
fn the_census_follows_the_machine_it_models() {
    let (formulae, casks) = fixture();
    let big_sur = measure(
        &formulae,
        &casks,
        &Machine::mac("arm64", "11.7", None, false),
    )
    .unwrap();
    assert!(big_sur.solo.admitted.is_empty() && big_sur.closure.admitted.is_empty());
    assert_eq!(
        big_sur.solo.refusals["<name> needs its Homebrew build/relocation recipe (no bottle for this machine)"],
        (7, 7)
    );
    let next = measure(
        &formulae,
        &casks,
        &Machine::mac("arm64", "30.0", None, false),
    )
    .unwrap();
    assert_eq!(
        next.solo.admitted,
        ["zlib-ng", "tool", "curl-ish", "future", "client"]
    );
    assert_eq!(next.closure.admitted, ["zlib-ng", "tool", "future"]);
    assert!(
        !next
            .solo
            .refusals
            .contains_key("<name> needs a newer macOS")
    );
    assert_eq!(big_sur.cask.admitted, ["zipped", "imaged"]);
    assert_eq!(
        big_sur.cask.refusals["cask <name> needs macOS >= 14"],
        (1, 1)
    );
    assert_eq!(next.cask.admitted, ["zipped", "imaged", "floored"]);
}

#[test]
fn what_is_not_an_index_is_refused() {
    let (formulae, casks) = fixture();
    assert!(measure(b"{}", &casks, &tahoe()).is_err());
    assert!(measure(&formulae, b"not json", &tahoe()).is_err());
    let empty = measure(b"[]", b"[]", &tahoe()).unwrap();
    assert!(
        empty.text().contains("formulae: 0/0 solo (0.0%)"),
        "{}",
        empty.text()
    );
}

#[test]
fn only_the_refused_package_s_own_name_is_masked() {
    assert_eq!(
        masked("cask zip needs zip-tools.", "zip"),
        "cask <name> needs zip-tools."
    );
    assert_eq!(
        masked("abrew: python@3.13 is disabled upstream", "python@3.13"),
        "abrew: <name> is disabled upstream"
    );
    assert_eq!(masked("ends with go.", "go"), "ends with <name>.");
    assert_eq!(masked("a ago go-lang", "go"), "a ago go-lang");
    assert_eq!(masked("", "go"), "");
    assert_eq!(
        masked("cask installer ships a pkg installer", "installer"),
        "cask <name> ships a pkg installer"
    );
}

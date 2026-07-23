// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The v0.26 BRIDGE proof (release spec §4, decision 7): the manifest this
//! tool emits must be accepted by the parser already deployed on every v0.25
//! machine — a fleet no commit can update. The exact v0.25 `Manifest` struct
//! is VENDORED below as a frozen fixture (copied from v0.25's
//! `crates/aterm-update/src/manifest.rs`), independent of the copy
//! `manifest_out::v025` uses at publish time, so a drift in EITHER copy makes
//! these tests and the publish self-check disagree loudly.
//!
//! Round-tripping through the rewritten client proves nothing about the
//! frozen fleet parser — hence this file (the reliability judge's point).

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly.
#[path = "../src/changelog.rs"]
#[allow(dead_code)] // test mount: only the section-extraction surface is used
mod changelog;
#[path = "../src/ledger.rs"]
#[allow(dead_code)] // mounted for crate::ledger cross-references (Error/floor)
mod ledger;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)] // test mount: build/emit/v025_check are the surface here
mod manifest_out;

use aterm_update_core::Manifest;

/// The last v0.25-published build — every emitted bridge manifest must carry
/// a strictly higher number (mirrors ledger::LEDGER_FLOOR; duplicated here as
/// a literal so a typo in the constant cannot silently weaken both sides).
const V025_FLOOR: u64 = 1_783_354_739;

// ---------------------------------------------------------------------------
// the FROZEN v0.25 parser (vendored verbatim; do not "modernize")
// ---------------------------------------------------------------------------

/// Copied from v0.25 `crates/aterm-update/src/manifest.rs` (struct +
/// `SUPPORTED_SCHEMA` + `parse` — the whole wire-format surface; the local
/// marker types Ready/Floor/FailedMark are not part of the wire). This is
/// what runs on users' machines TODAY.
mod v025 {
    use serde::Deserialize;

    pub const SUPPORTED_SCHEMA: u32 = 1;

    #[derive(Debug, Clone, Deserialize)]
    pub struct Manifest {
        #[serde(default)]
        pub schema: u32,
        pub version: String,
        pub build_number: u64,
        #[serde(default)]
        pub commit: Option<String>,
        pub sha256: String,
        pub dmg: String,
        #[serde(default)]
        pub min_build: Option<u64>,
        #[serde(default)]
        pub changelog: Option<String>,
    }

    impl Manifest {
        pub fn parse(text: &str) -> Result<Self, String> {
            let m: Manifest = toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
            if m.schema > SUPPORTED_SCHEMA {
                return Err(format!(
                    "manifest schema {} is newer than supported ({SUPPORTED_SCHEMA}); upgrade aterm",
                    m.schema
                ));
            }
            Ok(m)
        }
    }
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// The spec §4 example cut, verbatim field values.
fn spec4_inputs<'a>(body: &'a str) -> manifest_out::ManifestInputs<'a> {
    manifest_out::ManifestInputs {
        version: "0.26",
        build_number: 1_783_918_101,
        commit: "aed5a06caed5a06caed5a06caed5a06caed5a06c",
        dmg_name: "aterm-0.26.dmg",
        dmg_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        repo_slug: "alabsystems/aterm",
        min_os: "11.0",
        team_id: "",
        pub_date: "2026-07-06T21:29:44Z",
        min_build: None,
        changelog: body,
    }
}

/// A changelog body with every character class the TOML literal must carry
/// raw: quotes, hashes, backslashes, backticks, blank lines, nested bullets.
const TRICKY_BODY: &str = "### Added\n\
    - **DEFAULT-3/DEFAULT-4** — signed updates \"on\" by default (`#pins`).\n\
    \n\
    ### Fixed\n\
    - a \\ backslash and a 'quote' and ''two quotes''\n\
      - nested bullet under FIND-1";

// ---------------------------------------------------------------------------
// the bridge proofs
// ---------------------------------------------------------------------------

/// The core §4 assertion set: the emitted v0.26 manifest parses under the
/// FROZEN v0.25 struct with schema ≤ 1, every required field intact, and a
/// build number above the fleet floor.
#[test]
fn emitted_v026_manifest_parses_under_the_frozen_v025_struct() {
    let m = manifest_out::build(&spec4_inputs(TRICKY_BODY));
    let text = manifest_out::emit(&m).expect("emit must succeed on the spec §4 shape");

    let old = v025::Manifest::parse(&text)
        .expect("the deployed v0.25 parser must accept the bridge manifest");
    assert!(
        old.schema <= v025::SUPPORTED_SCHEMA,
        "schema {} > v0.25 gate",
        old.schema
    );
    assert_eq!(old.schema, 1, "the bridge emits schema = 1, permanently");
    assert_eq!(old.version, "0.26");
    assert_eq!(old.build_number, 1_783_918_101);
    assert!(
        old.build_number > V025_FLOOR,
        "build_number must beat every fleet floor.toml"
    );
    assert_eq!(
        old.dmg, "aterm-0.26.dmg",
        "the client resolves the DMG asset by this name"
    );
    assert_eq!(
        old.sha256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256 gates the download bytes"
    );
    let commit = old
        .commit
        .expect("commit must survive (v0.25 records it at stage time)");
    assert_eq!(commit.len(), 40, "full 40-hex release commit");
    assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        old.min_build, None,
        "no floor unless the operator ratchets one"
    );
    // The changelog body must arrive VERBATIM (plus the emitter's normalizing
    // trailing newline) — this is what the v0.25 Software Update window shows.
    assert_eq!(
        old.changelog.as_deref(),
        Some(format!("{TRICKY_BODY}\n").as_str())
    );
}

/// The v0.26 field set includes keys v0.25 never had (url, min_os, team_id,
/// pub_date). Decision 6 leaned on "v0.25 ignores unknowns" — prove it: the
/// keys are REALLY in the emitted bytes, and the frozen parser still accepts.
#[test]
fn new_v026_keys_are_present_but_invisible_to_v025() {
    let m = manifest_out::build(&spec4_inputs("### Added\n- something"));
    let text = manifest_out::emit(&m).unwrap();
    for key in ["url = ", "min_os = ", "team_id = ", "pub_date = "] {
        assert!(
            text.contains(key),
            "expected emitted key {key:?} in:\n{text}"
        );
    }
    assert!(
        text.contains(
            "https://github.com/alabsystems/aterm/releases/download/v0.26/aterm-0.26.dmg"
        ),
        "install.sh greps this exact URL out of the manifest:\n{text}"
    );
    v025::Manifest::parse(&text).expect("unknown keys must be ignored (no deny_unknown_fields)");
}

/// `cut --min-build N` / `yank` (spec decision 21): the ratchet must reach
/// the deployed fleet through the frozen parser.
#[test]
fn min_build_ratchet_reaches_the_v025_fleet() {
    let mut inputs = spec4_inputs("### Fixed\n- the yank");
    // A yank cuts a freshly claimed successor; the floor can equal that new
    // build (retiring every predecessor) but can never be one beyond it.
    inputs.build_number = 1_783_918_102;
    inputs.min_build = Some(1_783_918_102);
    let text = manifest_out::emit(&manifest_out::build(&inputs)).unwrap();
    let old = v025::Manifest::parse(&text).unwrap();
    assert_eq!(
        old.min_build,
        Some(1_783_918_102),
        "the fleet must see the apply floor"
    );
}

/// The publisher must refuse a floor above its newly claimed build even though
/// the frozen v0.25 parser was historically permissive. This negative control
/// proves the new gate lives on the emitting/shared side and cannot be bypassed
/// merely because old clients deserialize the impossible value.
#[test]
fn publisher_refuses_min_build_above_its_own_claim() {
    let mut inputs = spec4_inputs("### Fixed\n- impossible floor");
    inputs.min_build = Some(inputs.build_number + 1);
    let err = manifest_out::emit(&manifest_out::build(&inputs))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("min_build") && err.contains("exceeds its build_number"),
        "{err}"
    );

    let raw = format!(
        "schema = 1\nversion = \"0.26\"\nbuild_number = {}\nsha256 = \"x\"\n\
         dmg = \"a.dmg\"\nmin_build = {}\n",
        inputs.build_number,
        inputs.build_number + 1
    );
    assert_eq!(
        v025::Manifest::parse(&raw).unwrap().min_build,
        Some(inputs.build_number + 1),
        "negative control: the historical parser alone does not enforce this invariant"
    );
}

/// The REAL, living CHANGELOG.md rides the bridge: the section body that
/// would ship next is emitted and read back byte-for-byte by BOTH parsers.
/// (Prefers [Unreleased]; falls back to the newest rolled section so this
/// test keeps guarding after a cut empties [Unreleased].)
#[test]
fn live_changelog_body_survives_the_bridge_byte_for_byte() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md");
    let text = std::fs::read_to_string(path).expect("read the real CHANGELOG.md");
    let body = changelog::rolled_body(&text, "Unreleased").or_else(|_| {
        // Fall back to the newest ROLLED section (e.g. `## [0.31]`) so this test keeps
        // guarding after a release cut empties `[Unreleased]`. The `!= "Unreleased"`
        // filter MUST live INSIDE `find_map` — otherwise `find_map` stops at the first
        // `## [` header, which is `[Unreleased]` itself, and the outer filter then nulls
        // it to `None`, panicking here every post-cut cycle (the empty-Unreleased state).
        let section = text
            .lines()
            .find_map(|l| {
                l.strip_prefix("## [")?
                    .split(']')
                    .next()
                    .filter(|s| *s != "Unreleased")
            })
            .expect("CHANGELOG.md has no rolled section either");
        changelog::rolled_body(&text, section)
    });
    let body = body.expect("a non-empty changelog section must exist");

    let mut inputs = spec4_inputs(&body);
    inputs.build_number = V025_FLOOR + 12_345; // any post-floor claim
    let m = manifest_out::build(&inputs);
    let emitted = manifest_out::emit(&m).expect("the live changelog must be emittable");

    let expect = format!("{body}\n");
    let old = v025::Manifest::parse(&emitted).expect("v0.25 must accept the live body");
    assert_eq!(
        old.changelog.as_deref(),
        Some(expect.as_str()),
        "v0.25 read differs"
    );
    let new = Manifest::parse(&emitted).expect("shared type must accept the live body");
    assert_eq!(
        new.changelog.as_deref(),
        Some(expect.as_str()),
        "shared read differs"
    );
}

/// The emit-side proof the publish self-check leans on, stated explicitly:
/// emitted bytes parse back to the exact value under the SHARED type too.
#[test]
fn emitted_bytes_round_trip_the_shared_type_exactly() {
    let m = manifest_out::build(&spec4_inputs(TRICKY_BODY));
    let text = manifest_out::emit(&m).unwrap();
    assert_eq!(
        Manifest::parse(&text).unwrap(),
        m,
        "emit→parse must be exact"
    );
}

/// The vendored fixture must really be a GATE, not a rubber stamp: it rejects
/// a future schema exactly like the deployed binaries would.
#[test]
fn the_frozen_v025_parser_rejects_a_future_schema() {
    let err = v025::Manifest::parse(
        "schema = 2\nversion = \"9.9\"\nbuild_number = 99\nsha256 = \"x\"\ndmg = \"a.dmg\"\n",
    )
    .unwrap_err();
    assert!(err.contains("newer than supported"), "{err}");
}

/// And the publish-time twin (`manifest_out::v025_check`) must agree with the
/// test-local fixture: bytes the check passes, this fixture parses — run over
/// the trickiest body we emit.
#[test]
fn publish_time_v025_check_agrees_with_the_test_fixture() {
    let m = manifest_out::build(&spec4_inputs(TRICKY_BODY));
    let text = m.to_toml().expect("serialize");
    manifest_out::v025_check(&text).expect("publish-time check must pass");
    v025::Manifest::parse(&text).expect("test fixture must agree");

    // Below-floor build: BOTH gates must refuse (the check errors; the fleet
    // would silently never stage it — which is exactly why the check exists).
    let mut low = spec4_inputs("### Fixed\n- x");
    low.build_number = V025_FLOOR; // not strictly above
    let low_text = manifest_out::build(&low).to_toml().unwrap();
    let err = manifest_out::v025_check(&low_text).unwrap_err().to_string();
    assert!(err.contains("floor"), "{err}");
}

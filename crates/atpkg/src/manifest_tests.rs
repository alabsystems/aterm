// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::sig::testkit;

/// `body`, machine-signed and verified through the real roster chain.
fn verified(body: &str) -> VerifiedBytes {
    testkit::machine_signed(body.as_bytes().to_vec())
}

/// A realistic index naming six programs (and not `dotfiles`, a private-config repo),
/// attributed to the test roster's machine.
fn full_index() -> String {
    format!(
        r#"
schema = 2
index_build = 41
generated_at = "2026-06-28T12:00:00Z"
valid_until = "2026-07-05T12:00:00Z"
machine_id = "{id}"
roster_seq = {seq}

[programs.ay]
repo = "ay"
policy = "prebuilt-or-build"

[programs.trust]
repo = "trust"
policy = "prebuilt-only"
coherence_group = "rustc"

[programs.aterm]
repo = "aterm"
policy = "prebuilt-only"

[programs.codex]
repo = "codex"
policy = "prebuilt-only"
extra = true

[programs.vendorx]
repo = "vendorx"
policy = "prebuilt-only"
extra = true

[programs.gh]
repo = "gh"
policy = "prebuilt-only"
system = "gh"

[[channels]]
name = "stable"
channel_build = 137
min_build = 120
yanked = ["trust@4790"]
pin = {{ aterm = 1234, trust = 4821, ay = 18, codex = 2026082601, vendorx = 1, gh = 2026082601 }}

[channels.meta]
nightly = "nightly-2025-12-03"
trust_mc_rev = "0.67.0"
"#,
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    )
}

/// A per-program floor raises only the program it names; every other program keeps the
/// channel-wide one.
#[test]
fn a_per_program_min_build_floors_one_program_only() {
    let body = format!(
        r#"
schema = 2
index_build = 41
valid_until = "2026-07-05T12:00:00Z"
machine_id = "{id}"
roster_seq = {seq}

[programs.trust]
repo = "trust"
policy = "prebuilt-only"

[programs.nn]
repo = "nn"
policy = "prebuilt-only"

[[channels]]
name = "stable"
channel_build = 137
min_build = 0
min_build_by_program = {{ trust = 7000 }}
pin = {{ trust = 7100, nn = 108 }}
"#,
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    let idx = parse_index(&verified(&body)).expect("a per-program floor parses");
    let ch = &idx.channels[0];
    assert_eq!(ch.min_build_for("trust"), 7000, "the floored program");
    assert_eq!(ch.min_build_for("nn"), 0, "a program the floor never named");
    let view = idx
        .channel_for("stable", "aarch64-apple-darwin")
        .expect("stable exists");
    assert_eq!(view.min_build_for("trust"), 7000);
    assert_eq!(view.min_build_for("nn"), 0);
    let old = parse_index(&verified(&full_index())).expect("valid index parses");
    let och = &old.channels[0];
    assert!(och.min_build_by_program.is_empty());
    assert_eq!(och.min_build_for("ay"), och.min_build);
    assert_eq!(och.min_build_for("trust"), och.min_build);
}

#[test]
fn parses_a_full_index_and_its_attribution() {
    let idx = parse_index(&verified(&full_index())).expect("valid index parses");
    assert_eq!(idx.index_build, 41);
    assert_eq!(idx.valid_until, "2026-07-05T12:00:00Z");
    assert_eq!(idx.machine_id.as_deref(), Some(testkit::MACHINE_ID));
    assert_eq!(idx.roster_seq, Some(testkit::SEQ));
    assert_eq!(
        idx.program("trust").unwrap().coherence_group.as_deref(),
        Some("rustc")
    );
    assert_eq!(idx.program("ay").unwrap().repo, "ay");
    assert_eq!(idx.program("gh").unwrap().system.as_deref(), Some("gh"));
    assert_eq!(idx.program("ay").unwrap().system, None);
    assert_eq!(idx.channels.len(), 1);
    let ch = &idx.channels[0];
    assert_eq!(ch.name, "stable");
    assert_eq!(ch.pin.get("trust"), Some(&4821));
}

/// A per-target overlay changes only the target it names: that target sees the overlaid
/// build for a program `pin` also names (replace) and a program `pin` never named (add);
/// every other target sees `pin` as published.
#[test]
fn a_per_target_pin_overlays_its_own_target_and_no_other() {
    let mut body = full_index();
    body = body.replace(
        "[channels.meta]",
        "pin_by_target = { \"x86_64-unknown-linux-gnu\" = { trust = 9122, nn = 108 } }\n[channels.meta]",
    );
    let idx = parse_index(&verified(&body)).expect("an index with a per-target overlay parses");
    let raw = &idx.channels[0];
    assert_eq!(
        raw.pin.get("trust"),
        Some(&4821),
        "the platform-agnostic pin is untouched on the wire"
    );

    let linux = idx
        .channel_for("stable", "x86_64-unknown-linux-gnu")
        .expect("stable exists");
    assert_eq!(
        linux.pin.get("trust"),
        Some(&9122),
        "REPLACED for the overlaid target"
    );
    assert_eq!(
        linux.pin.get("nn"),
        Some(&108),
        "ADDED: a program `pin` never named"
    );
    assert_eq!(
        linux.pin.get("ay"),
        Some(&18),
        "carried: a program the overlay does not mention"
    );
    assert!(
        linux.pin_by_target.is_empty(),
        "the view cannot be overlaid twice"
    );
    assert_eq!(linux.name, "stable");
    assert_eq!(
        linux.min_build, raw.min_build,
        "every other field rides along"
    );

    let mac = idx
        .channel_for("stable", "aarch64-apple-darwin")
        .expect("stable exists");
    assert_eq!(
        mac.pin.get("trust"),
        Some(&4821),
        "another target sees the published pin"
    );
    assert_eq!(
        mac.pin.get("nn"),
        None,
        "and not the other target's additions"
    );
    assert!(
        idx.channel_for("nightly", "x86_64-unknown-linux-gnu")
            .is_none(),
        "no such channel"
    );
}

/// The builds clients resolve, over every triple an index names, are `pin` ∪ every
/// `pin_by_target` overlay: the set the public mirror's work list (`atpkg-publish mirror`)
/// must carry.
#[test]
fn the_builds_clients_resolve_are_pin_union_every_overlay() {
    use std::collections::BTreeSet;

    let body = full_index().replace(
        "[channels.meta]",
        "pin_by_target = { \"x86_64-unknown-linux-gnu\" = { trust = 9122, nn = 108 }, \
         \"aarch64-unknown-linux-gnu\" = { trust = 9123 } }\n[channels.meta]",
    );
    let idx = parse_index(&verified(&body)).expect("an index with two overlays parses");
    let ch = &idx.channels[0];

    let mut triples: BTreeSet<&str> = ch.pin_by_target.keys().map(String::as_str).collect();
    assert_eq!(triples.len(), 2, "the fixture names two overlaid triples");
    triples.insert("aarch64-apple-darwin");

    let resolvable: BTreeSet<(String, u64)> = triples
        .iter()
        .flat_map(|t| {
            idx.channel_for(&ch.name, t)
                .expect("stable exists")
                .pin
                .into_iter()
        })
        .collect();

    let declared: BTreeSet<(String, u64)> = ch
        .pin
        .iter()
        .map(|(p, b)| (p.clone(), *b))
        .chain(
            ch.pin_by_target
                .values()
                .flat_map(|o| o.iter().map(|(p, b)| (p.clone(), *b))),
        )
        .collect();

    assert_eq!(
        resolvable, declared,
        "the resolvable set IS `pin` ∪ every overlay — if this stops holding, \
         the public mirror's work list no longer describes what it has to carry"
    );

    let pin_only: BTreeSet<(String, u64)> = ch.pin.iter().map(|(p, b)| (p.clone(), *b)).collect();
    assert!(
        resolvable.contains(&("trust".to_string(), 9122))
            && resolvable.contains(&("nn".to_string(), 108))
            && !pin_only.contains(&("trust".to_string(), 9122))
            && !pin_only.contains(&("nn".to_string(), 108)),
        "an overlay build is resolvable and appears in no `pin` row"
    );
    assert!(
        resolvable.contains(&("trust".to_string(), 4821)),
        "the `pin` build an overlay masks is still resolved by every other target, \
         so a publisher may never drop it in favour of the overlay"
    );
}

#[test]
fn unlisted_repo_is_unreachable() {
    let idx = parse_index(&verified(&full_index())).unwrap();
    assert!(
        idx.program("dotfiles").is_none(),
        "an unlisted repo is not a program"
    );
    assert!(!idx.is_program("dotfiles"));
    assert!(
        !idx.installable(&[]).contains("dotfiles"),
        "a private-config repo can never be in the installable set"
    );
    assert!(idx.is_program("trust") && idx.is_program("ay") && idx.is_program("aterm"));
}

/// A HARNESS IS IN THE DEFAULT SET LIKE ANY PROGRAM: the index names it, so the set the
/// passes complete holds it unasked; `exclude` still narrows it
/// out, a harness as well as the rest.
#[test]
fn the_default_set_holds_a_harness_and_exclude_still_narrows_it() {
    let body = full_index().replace(
        "[programs.ay]",
        "[programs.muse-alab]\nrepo = \"muse-alab\"\n\n[programs.ay]",
    );
    let idx = parse_index(&verified(&body)).unwrap();
    assert!(idx.is_program("muse-alab"));
    assert!(idx.installable(&[]).contains("muse-alab"));
    assert!(
        !idx.installable(&["muse-alab".into()]).contains("muse-alab"),
        "a person's exclude keeps a harness out"
    );
    assert!(
        !idx.installable(&["ay".into()]).contains("ay"),
        "exclude narrows the rest too"
    );
}

#[test]
fn exclude_is_narrowing_only() {
    let idx = parse_index(&verified(&full_index())).unwrap();
    assert_eq!(
        idx.installable(&[]),
        ["aterm", "ay", "codex", "gh", "trust", "vendorx"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert_eq!(
        idx.installable(&["trust".into(), "gh".into()]),
        ["aterm", "ay", "codex", "vendorx"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
}

/// The `https` row keys parse (all `serde(default)`), `links` is a map, and a row with no
/// `protocol` is `github-release`.
#[test]
fn parses_https_protocol_artifact_keys() {
    let body = r#"
schema = 2
program = "claude"
version = "2.1.231"
build_number = 2026082601
exposes = ["claude"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "binary"
protocol = "https"
url = "https://downloads.claude.ai/claude-code-releases/2.1.231/darwin-arm64/claude"
payload = "raw-binary"
entry = "claude"
asset = "claude-2.1.231-darwin-arm64"
sha256 = "7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c"
tree_root = "abc"
size = 230824016
vendor = "Anthropic PBC"
strip_components = 1
links = { emacs = "Emacs.app/Contents/MacOS/Emacs" }
"#;
    let m = parse_pkg(&verified(body)).expect("valid https manifest");
    let a = m.artifact_for("aarch64-apple-darwin").unwrap();
    assert_eq!(a.kind, "binary");
    assert_eq!(a.protocol, "https");
    assert!(a.url.starts_with("https://downloads.claude.ai/"));
    assert_eq!(a.payload, "raw-binary");
    assert_eq!(a.entry, "claude");
    assert_eq!(a.vendor, "Anthropic PBC");
    assert_eq!(a.strip_components, 1);
    assert_eq!(
        a.links.get("emacs").map(String::as_str),
        Some("Emacs.app/Contents/MacOS/Emacs")
    );
    let plain = parse_pkg(&verified(
        "schema = 2\nprogram = \"ay\"\nbuild_number = 18\n[[artifact]]\n\
         target = \"aarch64-apple-darwin\"\nasset = \"ay-18.tar.zst\"\nsha256 = \"d\"\n",
    ))
    .unwrap();
    let a = plain.artifact_for("aarch64-apple-darwin").unwrap();
    assert_eq!(a.protocol, "github-release");
    assert!(a.url.is_empty() && a.payload.is_empty() && a.entry.is_empty());
    assert_eq!(a.strip_components, 0);
    assert!(a.links.is_empty() && a.vendor.is_empty());
}

/// `shim_env` is validated at parse: the accepted shape reads back through
/// `PkgManifest::shim_env`, an absent key is no environment, and any refused entry refuses
/// the whole manifest as `Reject::ShimEnv` naming it, never `Malformed`.
#[test]
fn shim_env_is_signed_validated_and_refused_by_name() {
    let head = "schema = 2\nprogram = \"claude\"\nversion = \"2.1.231\"\n\
                build_number = 2026082601\nexposes = [\"claude\"]\n";
    let row = "[[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"binary\"\n\
               protocol = \"https\"\nurl = \"https://downloads.claude.ai/c/claude\"\n\
               payload = \"raw-binary\"\nentry = \"claude\"\nasset = \"claude-arm64\"\n\
               sha256 = \"d\"\ntree_root = \"abc\"\nsize = 1\n";
    let with = format!("{head}shim_env = [\"DISABLE_AUTOUPDATER=1\"]\n{row}");
    let m = parse_pkg(&verified(&with)).expect("a valid shim_env parses");
    assert_eq!(m.shim_env, vec!["DISABLE_AUTOUPDATER=1".to_string()]);
    assert_eq!(m.shim_env().spelled(), "DISABLE_AUTOUPDATER=1");
    let without = parse_pkg(&verified(&format!("{head}{row}"))).unwrap();
    assert!(without.shim_env.is_empty() && without.shim_env().is_empty());
    for list in [
        "[\"DISABLE_AUTOUPDATER\"]",
        "[\"disable_autoupdater=1\"]",
        "[\"PATH=/tmp\"]",
        "[\"X=\"]",
        "[\"X=1\", \"X=2\"]",
        "[\"A=1\", \"B=1\", \"C=1\", \"D=1\", \"E=1\", \"F=1\", \"G=1\", \"H=1\", \"I=1\"]",
        "[\"X=a\\nb\"]",
    ] {
        let body = format!("{head}shim_env = {list}\n{row}");
        match parse_pkg(&verified(&body)) {
            Err(Reject::ShimEnv(_)) => {}
            other => panic!("{list}: expected Reject::ShimEnv, got {other:?}"),
        }
    }
    let body = format!("{head}shim_env = \"DISABLE_AUTOUPDATER=1\"\n{row}");
    assert_eq!(parse_pkg(&verified(&body)).unwrap_err(), Reject::Malformed);
}

/// The retired index-level `requires` still parses, whatever it names (a cycle, an unlisted
/// name), so the program naming it is refused by name rather than the whole index.
#[test]
fn the_retired_requires_parses_so_its_program_can_be_refused() {
    let body = full_index()
        .replace(
            "[programs.gh]\nrepo = \"gh\"",
            "[programs.gh]\nrequires = [\"ay\", \"dotfiles\"]\nrepo = \"gh\"",
        )
        .replace(
            "[programs.ay]\nrepo = \"ay\"",
            "[programs.ay]\nrequires = [\"gh\"]\nrepo = \"ay\"",
        );
    let idx = parse_index(&verified(&body)).expect("an index naming `requires` verifies");
    assert!(idx.program("gh").unwrap().names_requires());
    assert!(idx.program("ay").unwrap().names_requires());
    assert!(!idx.program("trust").unwrap().names_requires());
}

/// The retired `kind = "vendor-fetch"` is refused at parse with the split spelled out
/// — never read as a binary, never as an https row, never silently Unknown.
#[test]
fn the_retired_vendor_fetch_kind_is_refused_naming_the_split() {
    let body = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                target = \"aarch64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                asset = \"claude\"\nsha256 = \"d\"\nurl = \"https://downloads.claude.ai/c\"\n";
    match parse_pkg(&verified(body)) {
        Err(Reject::RetiredKind(why)) => assert!(why.contains("vendor-fetch"), "{why}"),
        other => panic!("expected RetiredKind, got {other:?}"),
    }
    let mixed = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                 target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha256 = \"d\"\n\
                 [[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                 asset = \"a\"\nsha256 = \"d\"\n";
    assert!(matches!(
        parse_pkg(&verified(mixed)),
        Err(Reject::RetiredKind(_))
    ));
}

/// A row of a deleted protocol (`pkg`, `system-pm`) still parses, so admission refuses that
/// one program by name instead of the whole manifest being malformed; a release-shaped row
/// still fails closed without `asset`/`sha256`.
#[test]
fn deleted_protocol_rows_parse_and_release_rows_still_need_their_digests() {
    let body = r#"
schema = 2
program = "homebrew"
version = "4.5.0"
build_number = 2026082701
exposes = []

[[artifact]]
target = "aarch64-apple-darwin"
kind = "installer-pkg"
protocol = "pkg"
url = "https://github.com/Homebrew/brew/releases/download/4.5.0/Homebrew-4.5.0.pkg"
sha256 = "7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c"
size = 144434507
signer_team = "927JGANW46"
elevated = true
provides = ["/opt/homebrew/bin/brew"]
vendor = "Homebrew"

[[artifact]]
target = "x86_64-unknown-linux-gnu"
kind = "system-package"
protocol = "system-pm"
manager = "apt"
package = "emacs"
provides = ["emacs", "/usr/bin/emacs"]
elevated = true

[[artifact]]
target = "x86_64-pc-windows-msvc"
kind = "system-package"
protocol = "system-pm"
manager = "winget"
package = "GNU.Emacs"
provides = ["emacs"]
"#;
    let m = parse_pkg(&verified(body)).expect("pkg + system-pm rows parse");
    let p = m.artifact_for("aarch64-apple-darwin").unwrap();
    assert_eq!(
        (p.kind.as_str(), p.protocol.as_str()),
        ("installer-pkg", "pkg")
    );
    assert!(matches!(
        crate::vendor::check_row(p, &m.exposes),
        Err(crate::FlowError::VendorRefused(_))
    ));
    let apt = m.artifact_for("x86_64-unknown-linux-gnu").unwrap();
    assert_eq!(
        (apt.kind.as_str(), apt.protocol.as_str()),
        ("system-package", "system-pm")
    );
    assert!(m.artifact_for("aarch64-pc-windows-msvc").is_none());
    for missing in [
        "target = \"aarch64-apple-darwin\"\nsha256 = \"d\"\n",
        "target = \"aarch64-apple-darwin\"\nasset = \"a\"\n",
        "target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha512 = \"d\"\n",
        "target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha256 = \"d\"\nsha512 = \"d\"\n",
        "target = \"aarch64-apple-darwin\"\nprotocol = \"https\"\nkind = \"binary\"\n\
         sha256 = \"d\"\nurl = \"https://github.com/x\"\n",
    ] {
        let body =
            format!("schema = 2\nprogram = \"ay\"\nbuild_number = 18\n[[artifact]]\n{missing}");
        assert_eq!(
            parse_pkg(&verified(&body)).unwrap_err(),
            Reject::Malformed,
            "{missing}"
        );
    }
}

/// A row's gate is the one digest it sets, named by its algorithm; neither or both is no
/// digest.
#[test]
fn a_row_is_gated_by_the_one_digest_it_sets() {
    const AGY_SHA512: &str = "473a34b685d0a09d48790e8b86bc3e545dfab889fe0d8fc81cb7e1fdfc336fe7\
                              8ce57022e3b438c754c93c9cffeb13fdbd177e50fdedc087d52732fa5475183f";
    let sha256 = "7b09f01c".repeat(8);
    assert_eq!(
        one_digest(&sha256, ""),
        Some((DigestAlg::Sha256, sha256.as_str()))
    );
    assert_eq!(
        one_digest("", AGY_SHA512),
        Some((DigestAlg::Sha512, AGY_SHA512))
    );
    assert_eq!(
        one_digest(&sha256, AGY_SHA512),
        None,
        "two digests are no gate"
    );
    assert_eq!(one_digest("", ""), None);
    assert!(DigestAlg::Sha512.is_lower_hex(AGY_SHA512));
    assert!(DigestAlg::Sha256.is_lower_hex(&sha256));
    for (alg, bad) in [
        (DigestAlg::Sha256, AGY_SHA512.to_string()),
        (DigestAlg::Sha512, sha256.clone()),
        (DigestAlg::Sha512, AGY_SHA512.to_uppercase()),
        (DigestAlg::Sha512, AGY_SHA512.replacen('4', "g", 1)),
    ] {
        assert!(!alg.is_lower_hex(&bad), "{alg} {bad}");
    }
}

/// Every published triple is a forge cell, so some lane compiles it.
#[test]
fn every_published_triple_is_a_forge_cell() {
    let cells = aterm_forge::resolve::default_cells();
    for t in TARGETS {
        assert!(
            cells.iter().any(|c| c.triple == *t),
            "no forge cell compiles `{t}`, and atpkg publishes artifact rows for it"
        );
    }
}

#[test]
fn rejects_newer_schema() {
    let body = full_index().replace("schema = 2", "schema = 99");
    assert_eq!(parse_index(&verified(&body)).unwrap_err(), Reject::Schema);
    assert!(parse_index(&verified(&full_index())).is_ok());
}

/// A schema-1 index still parses (1 ≤ `SUPPORTED_SCHEMA`) but carries no attribution, so
/// the roster bind refuses it.
#[test]
fn a_schema_one_index_parses_but_carries_no_attribution() {
    let body = "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"AAAA\"\n";
    let idx = parse_index(&verified(body)).expect("an older schema is not a parse failure");
    assert_eq!(idx.machine_id, None);
    assert_eq!(idx.roster_seq, None);
}

/// Each fails closed as `Malformed`, never a silent default: a missing `index_build`; a
/// duplicate `machine_id`, which a last-wins parser would let re-attribute the index; a
/// string `roster_seq`, which must not read as absent.
#[test]
fn malformed_indexes_fail_closed() {
    for (body, why) in [
        (
            "schema = 2\nvalid_until = \"2026-07-05T12:00:00Z\"\n",
            "missing index_build",
        ),
        (
            "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             machine_id = \"m3\"\nmachine_id = \"m11\"\nroster_seq = 3\n",
            "duplicate machine_id",
        ),
        (
            "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             machine_id = \"m3\"\nroster_seq = \"3\"\n",
            "string roster_seq",
        ),
    ] {
        assert_eq!(
            parse_index(&verified(body)).unwrap_err(),
            Reject::Malformed,
            "{why}"
        );
    }
}

#[test]
fn sibling_table_cannot_hijack_the_attribution() {
    let body = "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                machine_id = \"m3\"\nroster_seq = 3\n\
                [meta]\nmachine_id = \"m11\"\nroster_seq = 9\n";
    let idx = parse_index(&verified(body)).expect("parses; [meta] ignored");
    assert_eq!(idx.machine_id.as_deref(), Some("m3"));
    assert_eq!(idx.roster_seq, Some(3));
}

#[test]
fn parses_pkg_manifest_and_selects_artifact() {
    let body = r#"
schema = 2
program = "trust"
version = "1.96.0-dev"
build_number = 4821
exposes = ["trust", "trust-mc"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "sysroot-bundle"
asset = "trust-4821-aarch64-apple-darwin.tar.zst"
sha256 = "deadbeef"
size = 1837465600
[artifact.cost]
download_bytes = 1837465600
disk_installed = 3221225472
build_seconds = 0
"#;
    let m = parse_pkg(&verified(body)).expect("valid pkg manifest");
    assert_eq!(m.program, "trust");
    assert_eq!(m.build_number, 4821);
    assert_eq!(m.exposes, vec!["trust".to_string(), "trust-mc".to_string()]);
    let a = m
        .artifact_for("aarch64-apple-darwin")
        .expect("triple present");
    assert_eq!(a.sha256, "deadbeef");
    assert_eq!(a.cost.disk_installed, 3221225472);
    assert!(m.artifact_for("x86_64-unknown-linux-gnu").is_none());
}

#[test]
fn strict_utf8_rejects_invalid_bytes_in_signed_index() {
    let mut raw = b"schema = 2\nindex_build = 1\n".to_vec();
    raw.push(0xFF);
    raw.extend_from_slice(b"\nvalid_until = \"2026-07-05T12:00:00Z\"\n");
    assert_eq!(
        parse_index(&testkit::machine_signed(raw)).unwrap_err(),
        Reject::Malformed
    );
}

#[test]
fn parse_pkg_rejects_newer_schema_and_missing_field() {
    let newer = "schema = 99\nprogram = \"ay\"\nbuild_number = 1\n";
    assert_eq!(parse_pkg(&verified(newer)).unwrap_err(), Reject::Schema);
    let missing = "schema = 2\nprogram = \"ay\"\n";
    assert_eq!(
        parse_pkg(&verified(missing)).unwrap_err(),
        Reject::Malformed
    );
}

/// `[artifact.provenance]` rides per row, exactly as `atpkg-publish` writes it
/// (after the row's `[artifact.cost]`, with a merged row appended after it).
#[test]
fn provenance_rides_per_row() {
    let commit = "7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15";
    let body = format!(
        "schema = 1\nprogram = \"ay\"\nversion = \"0.28.0\"\nbuild_number = 9001\nexposes = [\"ay\"]\n\n\
         [[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"binary\"\nprotocol = \"github-release\"\n\
         asset = \"ay-9001.tar.zst\"\nsha256 = \"{h}\"\ntree_root = \"{h}\"\nsize = 10\n\
         [artifact.cost]\ndownload_bytes = 10\ndisk_installed = 20\nbuild_seconds = 0\n\
         [artifact.provenance]\ncompiler_release = \"1.99.0-dev\"\ncompiler_commit = \"{commit}\"\n\
         lineage = \"unproven\"\nscan_objects = \"5\"\nscan_findings = \"0\"\n\n\
         [[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"binary\"\nprotocol = \"github-release\"\n\
         asset = \"ay-9001-x86_64-apple-darwin.tar.zst\"\nsha256 = \"{h}\"\nsize = 10\n\
         [artifact.cost]\ndownload_bytes = 10\ndisk_installed = 20\nbuild_seconds = 0\n",
        h = "a".repeat(64)
    );
    let m = parse_pkg(&verified(&body)).expect("a manifest carrying provenance parses");
    let p = m.artifacts[0]
        .provenance
        .as_ref()
        .expect("row 0 carries provenance");
    assert_eq!(p.compiler_commit, commit);
    assert_eq!(p.lineage, "unproven");
    assert_eq!(p.scan_findings, "0");
    assert_eq!(
        m.artifacts[1].provenance, None,
        "a merged row without the table has none"
    );
    assert_eq!(
        m.artifacts[1].cost.disk_installed, 20,
        "the table does not swallow the next row"
    );
}

#[test]
fn pkg_program_field_binds_identity() {
    let m = parse_pkg(&verified(
        "schema = 2\nprogram = \"ay\"\nbuild_number = 18\n",
    ))
    .unwrap();
    assert!(m.is_for("ay"));
    assert!(
        !m.is_for("trust"),
        "a pkg signed for ay must not pass as trust"
    );
}

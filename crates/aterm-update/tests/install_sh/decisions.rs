// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The installer's pure decisions: the manifest's identity fields, the
//! official channel's signing-team pin, the app half's idempotence and its
//! signature gate, the container election, which repository a checkout
//! installs from, and the Linux desktop entry.

use crate::support::{Lib, Scratch, q, root, s};

/// A required identity field occurs exactly once as a one-line string; an
/// optional one at most once.
#[test]
fn a_manifest_identity_field_is_one_plain_string() {
    let scratch = Scratch::new();
    let rows: &[(&str, &str, u8, i32, &str)] = &[
        ("version = \"0.2.0\"\n", "version", 1, 0, "0.2.0"),
        (
            "version = \"0.2.0\"\nversion = \"0.2.1\"\n",
            "version",
            1,
            2,
            "",
        ),
        ("version = 0.2.0\n", "version", 1, 2, ""),
        ("version = \"0.2.0\"\n", "dmg", 1, 2, ""),
        ("version = \"0.2.0\"\n", "zip", 0, 0, ""),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (body, key, required, _, _))| {
            let file = scratch.write(&format!("m{n}.toml"), body);
            lib.call(
                format!("toml_single_str {} {key} {required}", q(s(&file))),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[i].rc, recs[i].out.as_str()),
            (row.3, row.4),
            "{row:?}"
        );
    }
}

/// On the official repository the Developer-ID team is pinned in the script:
/// a manifest that omits or contradicts it is refused. An explicit
/// ATERM_TEAM_ID outranks everything; a fork keeps its manifest's pin.
#[test]
fn the_official_channel_pins_its_signing_team() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let pins = lib.call(":", &["OFFICIAL_REPO_SLUG", "OFFICIAL_TEAM_ID"]);
    let rows: &[(&str, &str, &str, i32, &str)] = &[
        ("alabsystems/aterm", "", "$OFFICIAL_TEAM_ID", 0, "OFFICIAL"),
        ("alabsystems/aterm", "", "", 2, ""),
        ("alabsystems/aterm", "", "ZZZZZ99999", 2, ""),
        ("alabsystems/aterm", "FORKTEAM99", "", 0, "FORKTEAM99"),
        ("my-org/aterm", "", "", 0, ""),
        ("my-org/aterm", "", "FORKTEAM99", 0, "FORKTEAM99"),
        ("my-org/aterm", "FORKTEAM99", "OTHERTEAM11", 0, "FORKTEAM99"),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(slug, env, manifest, _, _)| {
            lib.call(
                format!("required_team_for {slug} '{env}' \"{manifest}\""),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    assert_eq!(recs[pins].var("OFFICIAL_REPO_SLUG"), "alabsystems/aterm");
    let official = recs[pins].var("OFFICIAL_TEAM_ID");
    assert_eq!(official, aterm_update_core::pins::APPLE_TEAM_ID);
    for (row, i) in rows.iter().zip(calls) {
        let want = if row.4 == "OFFICIAL" { official } else { row.4 };
        assert_eq!((recs[i].rc, recs[i].out.as_str()), (row.3, want), "{row:?}");
    }
}

/// An unpinned re-run against the installed elected version downloads
/// nothing; a pin always reinstalls; a foreign bundle never short-circuits.
#[test]
fn only_an_unpinned_rerun_of_the_installed_release_is_a_no_op() {
    let scratch = Scratch::new();
    let rows = [
        ("v0.44.0 0 com.aterm.aterm 0.44.0", 0),
        ("v0.44.0 1 com.aterm.aterm 0.44.0", 1),
        ("v0.44.0 0 com.aterm.aterm 0.43.0", 1),
        ("v0.44.0 0 com.example.impostor 0.44.0", 1),
        ("v0.44.0 0 com.aterm.aterm ''", 1),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(args, _)| lib.call(format!("app_already_current {args}"), &[]))
        .collect();
    let recs = lib.run(&scratch);
    for ((args, rc), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, *rc, "{args}");
    }
}

/// The idempotence gate blesses only a bundle the fresh install would have
/// accepted, and tells a verifier that did not answer (124 codesign, 125
/// Gatekeeper) from one that declined (1).
#[test]
fn the_installed_bundle_gate_verifies_like_a_fresh_install() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    // The seal check carries no -R; the team requirement check does.
    lib.step(
        "codesign() {
			local arg
			for arg in \"$@\"; do [[ \"$arg\" != -R ]] || return \"$REQ\"; done
			return \"$SEAL\"
		}
		spctl() { return \"$GATE\"; }",
    );
    let app = "/nonexistent/aterm.app";
    let team = "A66A9P66Z7";
    let rows: &[(&str, &str, i32)] = &[
        ("SEAL=0 REQ=0 GATE=0", team, 0),
        ("SEAL=0 REQ=0 GATE=0", "", 0),
        ("SEAL=1 REQ=0 GATE=0", "", 1),
        ("SEAL=0 REQ=1 GATE=0", team, 1),
        ("SEAL=0 REQ=0 GATE=1", team, 1),
        ("SEAL=0 REQ=0 GATE=0", "A66A9P66Z7\"; rm -rf /", 1),
        ("SEAL=124 REQ=0 GATE=0", "", 124),
        ("SEAL=0 REQ=0 GATE=124", team, 125),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(stubs, team, _)| {
            lib.call(
                format!("{stubs}; installed_bundle_verified {app} {}", q(team)),
                &[],
            )
        })
        .collect();
    // A codesign that never returns is stopped by the hang detector.
    lib.step("codesign() { sleep 60; }; VERIFY_WAIT_SECS=1");
    let hung = lib.call(format!("installed_bundle_verified {app} ''"), &[]);
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, row.2, "{row:?}");
    }
    assert_eq!(recs[hung].rc, 124);
}

/// One digest, two spellings: equality is case-blind, and an empty operand
/// never matches.
#[test]
fn digests_compare_case_blind_and_never_empty() {
    let scratch = Scratch::new();
    let lower = "0123456789abcdef".repeat(4);
    let upper = lower.to_uppercase();
    let other = format!("{}0", &lower[..63]);
    let rows = [
        (lower.as_str(), upper.as_str(), 0),
        (lower.as_str(), lower.as_str(), 0),
        (lower.as_str(), other.as_str(), 1),
        ("", "", 1),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(a, b, _)| lib.call(format!("sha256_equal {} {}", q(a), q(b)), &[]))
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, row.2, "{row:?}");
    }
}

/// The desktop entry's Exec= is written unquoted, so only a plain absolute
/// path may ride it.
#[test]
fn only_a_plain_absolute_path_rides_an_unquoted_exec_line() {
    let scratch = Scratch::new();
    let rows = [
        ("/home/user/.local/bin/aterm", 0),
        ("/opt/aterm-0.44.0/bin_x86+64/.aterm", 0),
        (".local/bin/aterm", 1),
        ("/home/user name/bin/aterm", 1),
        ("/home/u\"/bin/aterm", 1),
        ("/home/u%U/bin/aterm", 1),
        ("/home/u;rm/bin/aterm", 1),
        ("", 1),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(path, _)| lib.call(format!("desktop_exec_path_ok {}", q(path)), &[]))
        .collect();
    let recs = lib.run(&scratch);
    for ((path, rc), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, *rc, "{path:?}");
    }
}

/// A clone run installs the desktop entry and the aterm icon at every hicolor
/// size the checkout ships (`uname` shadowed: the lane is Linux-only).
#[test]
fn a_clone_run_installs_the_desktop_entry_and_every_icon_size() {
    let scratch = Scratch::new();
    let bin = scratch.exe("desktop/bin/aterm", "#!/bin/sh\n");
    let share = scratch.mkdir("desktop/share");
    let mut lib = Lib::new();
    let call = lib.call(
        format!(
            "( uname() {{ echo Linux; }}; BIN_DIR={} XDG_DATA_HOME={} ROOT={}; install_linux_desktop_entry )",
            q(s(bin.parent().expect("bin dir"))),
            q(s(&share)),
            q(s(&root()))
        ),
        &["ATERM_DESKTOP_MARKER"],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
    let entry = crate::support::read(&share.join("applications/aterm.desktop"));
    // The marker is an on-disk contract across versions: uninstall finds the
    // entries older installers wrote by this exact line.
    assert_eq!(
        recs[call].var("ATERM_DESKTOP_MARKER"),
        "# Managed by aterm install.sh — its uninstall removes only an entry carrying this line."
    );
    assert!(
        entry.starts_with(recs[call].var("ATERM_DESKTOP_MARKER")),
        "the entry carries the ownership marker the uninstall sweep reads: {entry}"
    );
    assert!(
        entry.contains(&format!("Exec={} --window", s(&bin))),
        "{entry}"
    );
    let shipped: Vec<String> = std::fs::read_dir(root().join("assets/linux/icons/hicolor"))
        .expect("the checkout's icons")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(shipped.len(), 5, "{shipped:?}");
    for size in shipped {
        let icon = share.join(format!("icons/hicolor/{size}/apps/aterm.png"));
        assert!(
            icon.metadata().is_ok_and(|m| m.len() > 0),
            "{} missing",
            icon.display()
        );
    }
}

const SHA_DMG: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SHA_ZIP: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// The container election: the lean zip whenever the release ships one (the
/// toolset deferred to first launch, or excluded), else the bare DMG a
/// pre-lean release was cut for; half a pair or a malformed one refuses.
#[test]
fn the_container_election_matrix() {
    let scratch = Scratch::new();
    let (dmg, zip) = ("aterm-0.48.0.dmg", "aterm-0.48.0-mac.zip");
    // (toolchain, zip, zip digest) -> (status, kind, asset, digest, lean reason, deferred)
    type Input<'a> = (u8, &'a str, &'a str);
    type Elected<'a> = (i32, &'a str, &'a str, &'a str, &'a str, &'a str);
    let rows: &[(Input, Elected)] = &[
        ((1, zip, SHA_ZIP), (0, "zip", zip, SHA_ZIP, "default", "1")),
        (
            (0, zip, SHA_ZIP),
            (0, "zip", zip, SHA_ZIP, "no-toolchain", "0"),
        ),
        ((1, "", ""), (0, "dmg", dmg, SHA_DMG, "", "0")),
        ((0, "", ""), (0, "dmg", dmg, SHA_DMG, "", "0")),
        ((1, zip, ""), (2, "", "", "", "", "")),
        ((1, "aterm-evil-mac.zip", SHA_ZIP), (2, "", "", "", "", "")),
        ((1, zip, "not-a-digest"), (2, "", "", "", "", "")),
    ];
    let vars = [
        "CONTAINER_KIND",
        "ASSET_NAME",
        "ASSET_SHA",
        "LEAN_REASON",
        "TOOLCHAIN_DEFERRED",
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|((toolchain, zip, zip_sha), _)| {
            lib.call(
                format!("elect_container {toolchain} 0.48.0 {dmg} {SHA_DMG} '{zip}' '{zip_sha}'"),
                &vars,
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((input, want), i) in rows.iter().zip(calls) {
        let rec = &recs[i];
        assert_eq!(rec.rc, want.0, "{input:?}");
        if want.0 == 0 {
            let got = vars.map(|v| rec.var(v));
            assert_eq!(got, [want.1, want.2, want.3, want.4, want.5], "{input:?}");
        }
    }
}

/// The manifest parse reads the current keys; the retired Intel pair's keys
/// are neither required nor read, so they can never masquerade as a zip.
#[test]
fn the_manifest_parse_ignores_the_retired_intel_pair() {
    let scratch = Scratch::new();
    let lean = format!(
        "version = \"0.48.0\"\ndmg = \"aterm-0.48.0.dmg\"\nsha256 = \"{SHA_DMG}\"\nzip = \"aterm-0.48.0-mac.zip\"\nzip_sha256 = \"{SHA_ZIP}\""
    );
    let old = format!(
        "version = \"0.48.0\"\ndmg = \"aterm-0.48.0.dmg\"\nsha256 = \"{SHA_DMG}\"\ndmg_x86_64 = \"aterm-0.48.0-x86_64.dmg\"\ndmg_x86_64_sha256 = \"short\""
    );
    let mut lib = Lib::new();
    let elect =
        "elect_container 1 \"$VERSION\" \"$DMG_NAME\" \"$SHA_WANT\" \"$ZIP_NAME\" \"$ZIP_SHA\"";
    let parse_lean = lib.call(
        format!("parse_manifest_identity_fields {}", q(&lean)),
        &["ZIP_NAME", "ZIP_SHA"],
    );
    let elect_lean = lib.call(elect, &["CONTAINER_KIND", "ASSET_NAME"]);
    let parse_old = lib.call(
        format!("parse_manifest_identity_fields {}", q(&old)),
        &["ZIP_NAME"],
    );
    let elect_old = lib.call(elect, &["CONTAINER_KIND", "ASSET_NAME"]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[parse_lean].rc, 0);
    assert_eq!(
        [
            recs[parse_lean].var("ZIP_NAME"),
            recs[parse_lean].var("ZIP_SHA")
        ],
        ["aterm-0.48.0-mac.zip", SHA_ZIP]
    );
    assert_eq!(
        [
            recs[elect_lean].var("CONTAINER_KIND"),
            recs[elect_lean].var("ASSET_NAME")
        ],
        ["zip", "aterm-0.48.0-mac.zip"]
    );
    assert_eq!(
        (recs[parse_old].rc, recs[parse_old].var("ZIP_NAME")),
        (0, "")
    );
    assert_eq!(
        [
            recs[elect_old].var("CONTAINER_KIND"),
            recs[elect_old].var("ASSET_NAME")
        ],
        ["dmg", "aterm-0.48.0.dmg"]
    );
}

/// A run from a checkout installs from the channel its manifest declares — the
/// shipped binary's compiled-in channel — never from `repository` alone.
#[test]
fn a_checkout_installs_from_its_update_channel() {
    let scratch = Scratch::new();
    let pkg = "[workspace.package]\nversion = \"0.9.0\"\nrepository = \"https://github.com/dev-org/aterm\"\n";
    let manifest = |name: &str, body: &str| {
        scratch
            .write(&format!("{name}.toml"), body)
            .display()
            .to_string()
    };
    let mut rows: Vec<(String, i32, String)> = vec![
        (
            manifest(
                "both",
                &format!(
                    "[workspace.metadata.aterm]\nupdate_channel = \"chan-org/aterm\"  # public\n\n{pkg}"
                ),
            ),
            0,
            "chan-org/aterm".into(),
        ),
        (manifest("absent", pkg), 0, "dev-org/aterm".into()),
        (
            manifest(
                "wrongtable",
                &format!("[workspace.metadata.atpkg]\nupdate_channel = \"x/y\"\n{pkg}"),
            ),
            0,
            "dev-org/aterm".into(),
        ),
        (
            manifest(
                "pubkey",
                &format!("[workspace.metadata.aterm]\nupdate_channel_pubkey = \"x/y\"\n{pkg}"),
            ),
            0,
            "dev-org/aterm".into(),
        ),
        (
            manifest("neither", "[workspace]\nmembers = []\n"),
            1,
            String::new(),
        ),
        (
            scratch.join("no-such.toml").display().to_string(),
            1,
            String::new(),
        ),
    ];
    for (n, bad) in [
        "chan-org",
        "chan-org/aterm/extra",
        "/aterm",
        "chan org/aterm",
        "chan-org/..",
        "https://github.com/chan-org/aterm",
    ]
    .iter()
    .enumerate()
    {
        rows.push((
            manifest(
                &format!("bad{n}"),
                &format!("[workspace.metadata.aterm]\nupdate_channel = \"{bad}\"\n{pkg}"),
            ),
            0,
            "dev-org/aterm".into(),
        ));
    }
    let mut lib = Lib::new();
    let this = lib.call(
        format!(
            "channel_slug_from_manifest {}",
            q(s(&root().join("Cargo.toml")))
        ),
        &[],
    );
    let calls: Vec<usize> = rows
        .iter()
        .map(|(file, _, _)| lib.call(format!("channel_slug_from_manifest {}", q(file)), &[]))
        .collect();
    let recs = lib.run(&scratch);
    assert_eq!(
        (recs[this].rc, recs[this].out.as_str()),
        (
            0,
            format!(
                "{}/{}",
                aterm_update_core::DEFAULT_OWNER,
                aterm_update_core::DEFAULT_REPO
            )
            .as_str()
        ),
        "this checkout installs from the channel its binaries update from"
    );
    for ((file, rc, slug), i) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[i].rc, recs[i].out.as_str()),
            (*rc, slug.as_str()),
            "{file}"
        );
    }
}

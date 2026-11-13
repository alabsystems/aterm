// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A `ctl` call's deadline starts from its verb's own timeout: what
/// `timeout=<ms>` says, and nothing for a verb without one or an unreadable
/// value.
#[test]
fn a_ctl_call_is_bounded_by_its_verbs_timeout_and_the_hang_detector() {
    assert_eq!(
        verb_timeout(&["await", "idle", "1000", "timeout=60000"]),
        Duration::from_secs(60)
    );
    assert_eq!(verb_timeout(&["metrics", "--json"]), Duration::ZERO);
    assert_eq!(verb_timeout(&["send", "timeout=x"]), Duration::ZERO);
}

#[test]
fn options_parse_into_typed_args_and_refuse_bad_values() {
    let mut o = BTreeMap::new();
    o.insert("--samples", "9".to_string());
    o.insert("--backend", "gpu".to_string());
    o.insert("--cooldown-s", "0.5".to_string());
    o.insert("--settle-ms", "0".to_string());
    o.insert("--warm", String::new());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(a.samples, Some(9));
    assert_eq!(a.backend, BackendRequest::Gpu);
    assert_eq!(a.cooldown_s, Some(0.5));
    assert_eq!(a.settle_ms, Some(0));
    assert!(a.warm);
    for (flag, bad) in [
        ("--samples", "0"),
        ("--samples", "x"),
        ("--backend", "metal"),
        ("--cooldown-s", "-1"),
        ("--settle-ms", "-5"),
    ] {
        let mut o = BTreeMap::new();
        o.insert(flag, bad.to_string());
        assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
    }
}

#[test]
fn the_bench_box_is_the_m4_max_or_a_labelled_dev_smoke() {
    assert!(!bench_box("Apple M4 Max", false).expect("bench").dev_smoke);
    for chip in ["Apple M5 Max", "Apple M4 Pro", "Virtual Apple M4 Max"] {
        match bench_box(chip, false) {
            Err(Halt::Skip { fix, .. }) => assert!(fix.contains("--dev-smoke"), "{fix}"),
            other => panic!("{chip}: expected a skip, got {other:?}"),
        }
        let smoke = bench_box(chip, true).expect("labelled");
        assert!(smoke.dev_smoke, "{chip}");
        assert!(smoke.label().ends_with("NON-PUBLISHABLE"), "{chip}");
    }
}

#[test]
fn the_committed_pins_parse() {
    let p = pins().expect("manifest.toml parses into the typed pins");
    assert_eq!(p.ghostty.tip.artifact_sha256.len(), 64);
    assert_eq!(p.harness.vtebench.commit.len(), 40);
}

/// The arena hands the unverified bytes to the shared, bounded transport at a quarantine
/// path. NEGATIVE CONTROL: an injected refusal is returned without creating or naming the
/// final cache path; changing the call to target the cache directly breaks these assertions.
#[test]
fn the_ghostty_dmg_uses_the_bounded_resumable_quarantine_transport() {
    let root = Scratch::new("ghostty-fetch").expect("scratch");
    let cached = root.path().join("Ghostty-tip.dmg");
    let quarantine = cached.with_extension("part");
    let mut asked = None;
    let result = download_ghostty_dmg_with(
        "https://example.invalid/Ghostty.dmg",
        &quarantine,
        |url, destination, cap| {
            asked = Some((url.to_string(), destination.to_path_buf(), cap));
            Err("HTTP 504 after 19 attempts / 200s total retry-pause bound".to_string())
        },
    );
    let Halt::Skip { why, fix } = result.expect_err("the injected transport refused") else {
        panic!("a transport refusal is a skip")
    };
    assert_eq!(
        asked,
        Some((
            "https://example.invalid/Ghostty.dmg".to_string(),
            quarantine,
            GHOSTTY_DMG_CAP,
        ))
    );
    assert!(why.contains("HTTP 504 after 19 attempts / 200s"), "{why}");
    assert!(fix.contains("--dmg"), "{fix}");
    assert!(
        !cached.exists(),
        "unverified bytes never reach the cache path"
    );
}

#[test]
fn the_template_fills_every_field() {
    let l = Ledger {
        date: "2026-10-15",
        dim: "D1",
        machine: "m",
        macos: "27.0",
        aterm: "abc sha256=x",
        competitors: "ghostty",
    };
    let out = fill_template(
        "# {{DIM}} — {{DATE}}\n{{MACHINE}} {{MACOS}} {{ATERM}} {{COMPETITORS}}\n",
        &l,
    );
    assert_eq!(out, "# D1 — 2026-10-15\nm 27.0 abc sha256=x ghostty\n");
    // The entry head's every field is one a ledger fills.
    let head = fill_template(TEMPLATE, &l);
    assert!(!head.contains("{{"), "{head}");
    assert!(head.contains("# Arena: D1 — 2026-10-15"), "{head}");
}

/// Every socket a lane makes fits `sun_path` whatever `$TMPDIR` is, and
/// the layout it replaced — the socket inside the run's HOME tree under
/// macOS's 49-byte per-user `$TMPDIR` — is refused before aterm sees it.
#[test]
fn a_socket_path_fits_sun_path_whatever_tmpdir_is() {
    let widest = aterm_tempfile::quiet_in(Path::new(SOCKET_BASE)).join(format!(
        "at.{}.{}.noindex",
        u32::MAX,
        u32::MAX
    ));
    let sock = socket_in(&widest, "trial-999-candidate.sock").expect("fits");
    assert!(sock.as_os_str().len() <= MAX_SUN_PATH, "{}", sock.display());
    let tmpdir = Path::new("/var/folders/rw/3088njqs1vsf5365hfl3yr8h0000gn/T/");
    assert_eq!(tmpdir.as_os_str().len(), 49);
    let deep = tmpdir
        .join("aterm-arena-compare-99999-3b9ac9ff")
        .join("trial-999-candidate");
    let e = socket_in(&deep, "control.sock").expect_err("past sun_path");
    assert!(e.contains("at most 103"), "{e}");
}

#[test]
fn a_scratch_directory_is_private_and_gone_on_drop() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = Scratch::sockets().expect("socket dir");
    let path = dir.path().to_path_buf();
    let mode = std::fs::metadata(&path).expect("made").permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    drop(dir);
    assert!(!path.exists(), "{}", path.display());
}

#[test]
fn sha256_streams_a_file() {
    let dir = Scratch::new("atv-sha").expect("scratch");
    let f = dir.path().join("f");
    std::fs::write(&f, b"abc").expect("write");
    assert_eq!(
        sha256_file(&f).expect("hash"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_report_reaches_the_inbox() {
    let inbox = Inbox::bind().expect("bind");
    report(inbox.path(), "drain 1.5\n").expect("report");
    assert_eq!(
        inbox.wait(Duration::from_secs(60)).expect("report"),
        "drain 1.5"
    );
    let silent = Inbox::bind().expect("bind");
    assert!(silent.wait(Duration::from_millis(1)).is_err());
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn raw(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|s| (*s).to_string()).collect()
}

/// The accepted shape, then every refusal the rule names, each naming the entry.
#[test]
fn admit_accepts_the_rule_and_refuses_everything_else_by_name() {
    let env = ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1"])).unwrap();
    assert_eq!(
        env.entries(),
        &[("DISABLE_AUTOUPDATER".to_string(), "1".to_string())]
    );
    assert!(!env.is_empty());
    assert!(ShimEnv::admit(&[]).unwrap().is_empty());
    let env = ShimEnv::admit(&raw(&["A_1=x=y", "B=it's fine"])).unwrap();
    assert_eq!(env.spelled(), "A_1=x=y, B=it's fine");

    let refusals: &[(&[&str], &str)] = &[
        (&["DISABLE_AUTOUPDATER"], "not NAME=VALUE"),
        (&["=1"], "empty name"),
        (&["disable_autoupdater=1"], "name is not [A-Z0-9_]+"),
        (&["DISABLE-AUTOUPDATER=1"], "name is not [A-Z0-9_]+"),
        (&["1X=1"], "digit-led"),
        (&["PATH=/tmp"], "never sets"),
        (&["BASH_ENV=/tmp/rc"], "never sets"),
        (&["SHELLOPTS=xtrace"], "never sets"),
        (&["NODE_OPTIONS=--require /x.js"], "never sets"),
        (&["PYTHONPATH=/x"], "never sets"),
        (&["DYLD_INSERT_LIBRARIES=/x.dylib"], "never sets"),
        (&["LD_PRELOAD=/x.so"], "never sets"),
        (&["X="], "empty value"),
        (&["X=a\nb"], "control byte"),
        (&["X=a\u{7f}"], "control byte"),
        (&["X=a "], "starts or ends with whitespace"),
        (&["X= a"], "starts or ends with whitespace"),
        (&["X=\"1\""], "quote or a percent"),
        (&["X=%TEMP%"], "quote or a percent"),
        (&["X=1", "X=2"], "duplicate name"),
    ];
    for (entries, why) in refusals {
        let err = ShimEnv::admit(&raw(entries)).unwrap_err();
        assert!(err.contains(why), "{entries:?}: {err}");
        assert!(
            err.starts_with("shim_env entry "),
            "the refusal names the entry: {err}"
        );
    }
    let longest = format!("V={}", "x".repeat(MAX_ENTRY_BYTES - 2));
    assert_eq!(longest.len(), MAX_ENTRY_BYTES);
    assert!(ShimEnv::admit(std::slice::from_ref(&longest)).is_ok());
    let over = format!("{longest}x");
    let err = ShimEnv::admit(&[over]).unwrap_err();
    assert!(err.contains("longer than 256 bytes"), "{err}");
    let eight_long: Vec<String> = (0..8)
        .map(|i| format!("V{i}={}", "x".repeat(MAX_ENTRY_BYTES - 3)))
        .collect();
    let env = ShimEnv::admit(&eight_long).unwrap();
    assert!(env.to_lines().len() < MAX_SIDECAR_BYTES);
    let nine: Vec<String> = (0..9).map(|i| format!("V{i}=1")).collect();
    assert!(ShimEnv::admit(&nine).is_err());
    let eight: Vec<String> = (0..8).map(|i| format!("V{i}=1")).collect();
    assert_eq!(ShimEnv::admit(&eight).unwrap().entries().len(), 8);
}

/// The sidecar round-trips, an empty env removes it, a hand-edited one that breaks the
/// rule reads as NONE, and `remove_sidecar` takes it away.
#[test]
fn the_sidecar_round_trips_and_fails_closed() {
    let root = crate::testdir::dir("atpkg-shim-env-");
    let build_dir = root.join("store").join("claude").join("2026082701");
    std::fs::create_dir_all(&build_dir).unwrap();
    let env = ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1", "B=2"])).unwrap();
    write_sidecar(&build_dir, &env).unwrap();
    let path = sidecar_path(&build_dir).unwrap();
    assert_eq!(
        path,
        root.join("store")
            .join("claude")
            .join("2026082701.shim-env"),
        "a SIBLING of the build dir, like <build>.ready"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "DISABLE_AUTOUPDATER=1\nB=2\n"
    );
    assert_eq!(read_sidecar(&build_dir), env);
    std::fs::write(&path, "DISABLE_AUTOUPDATER=1\nPATH=/evil\n").unwrap();
    assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
    std::fs::write(&path, "# note\n\nDISABLE_AUTOUPDATER=1\n").unwrap();
    assert_eq!(read_sidecar(&build_dir).spelled(), "DISABLE_AUTOUPDATER=1");
    write_sidecar(&build_dir, &ShimEnv::NONE).unwrap();
    assert!(!path.exists());
    assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
    write_sidecar(&build_dir, &env).unwrap();
    remove_sidecar(&build_dir);
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A binary inside a store build, or inside its laid-aside `.noindex` tree, reads that build's
/// sidecar; a build with none reads nothing, and so do a path outside any store, one with no
/// program level, one whose build name is not one atpkg writes and one whose `..` steps leave
/// the build, each beside a record of its own a looser reading would take, and a relative path.
#[test]
fn a_store_binary_reads_its_builds_sidecar_and_nothing_else_does() {
    let root = crate::testdir::dir("atpkg-shim-env-of-");
    let store = root.join("pkg").join("store").join("claude");
    let build_dir = store.join("1000002000001000310");
    std::fs::create_dir_all(build_dir.join("bin")).unwrap();
    let exe = build_dir.join("bin").join("claude");
    std::fs::write(&exe, "").unwrap();
    let env = ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1", "CLAUDE_CODE_X=1"])).unwrap();
    write_sidecar(&build_dir, &env).unwrap();
    assert_eq!(of_store_binary(&exe), env);
    assert_eq!(
        of_store_binary(&store.join("1000002000001000310.noindex/bin/claude")),
        env,
        "a laid-aside tree's image path reads the build's own sidecar"
    );
    assert_eq!(of_store_binary(&build_dir.join("lib/deep/node")), env);

    let bare = store.join("1000002000001000311");
    std::fs::create_dir_all(bare.join("bin")).unwrap();
    assert_eq!(of_store_binary(&bare.join("bin/claude")), ShimEnv::NONE);
    for decoy in [
        root.join("pkg/other/claude/1000002000001000310"),
        root.join("pkg/store/1000002000001000310"),
        store.join("01000002000001000310"),
    ] {
        write_sidecar(&decoy, &env).unwrap();
        let other = decoy.join("bin/claude");
        assert_eq!(
            of_store_binary(&other),
            ShimEnv::NONE,
            "{}",
            other.display()
        );
    }
    for other in [
        root.join("pkg/agents/claude"),
        build_dir.join("../../../elsewhere/bin/claude"),
        PathBuf::from("pkg/store/claude/1000002000001000310/bin/claude"),
    ] {
        assert_eq!(
            of_store_binary(&other),
            ShimEnv::NONE,
            "{}",
            other.display()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Each vendor row's declared env turns its own updater off, by the value aterm writes
/// (grok's 1.0.45 source, whose variable name the 1.0.67 binary carries, and kimi
/// 2.3.1's source each read `1` as off and `0` as on), so `which` and the
/// doctor's drift line say so; `0`, or a variable no CLI reads, is just an environment.
/// Other spellings each CLI also reads as off (`true`, `yes`, `on`) are never written by
/// aterm, so the record names only `1`.
#[test]
fn every_rows_declared_env_turns_its_own_updater_off() {
    for row in crate::vendor_direct::VENDORS {
        let env = row.shim_env();
        if env.is_empty() {
            continue;
        }
        let line = env.fix_line(row.program).unwrap();
        assert!(
            line.starts_with(&format!("{}'s own updater is off here (", row.product)),
            "{}: {line}",
            row.program
        );
    }
    let kimi = ShimEnv::admit(&raw(&["KIMI_CODE_NO_AUTO_UPDATE=1"])).unwrap();
    assert_eq!(
        kimi.fix_line("kimi").unwrap(),
        "Kimi Code's own updater is off here (KIMI_CODE_NO_AUTO_UPDATE=1)"
    );
    for (name, on) in [
        ("GROK_DISABLE_AUTOUPDATER", "0"),
        ("KIMI_CODE_NO_AUTO_UPDATE", "0"),
        ("GROK_AUTOUPDATER", "1"),
        ("KIMI_AUTO_UPDATE", "1"),
    ] {
        assert!(!turns_updater_off(name, on), "{name}={on}");
        let env = ShimEnv::admit(&raw(&[&format!("{name}={on}")])).unwrap();
        assert_eq!(
            env.fix_line("grok").unwrap(),
            format!("runs with {name}={on}")
        );
    }
    assert!(turns_updater_off("GROK_DISABLE_AUTOUPDATER", "1"));
}

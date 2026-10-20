// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership: the questions the installer asks before it replaces anything at
//! the app destination or the `aterm` bin entry — the uninstaller's own tests,
//! asked in the install direction too.

use std::path::Path;

use crate::support::{Lib, Scratch, bash, install_sh, q, s, sh, symlink, system_path};

/// Long enough that a script's first exec, waiting on the system's code
/// assessment on a loaded machine, still answers; short of the batch's hang
/// detector.
const ANSWER_SECS: u32 = 50;

const ATERM: &str = "#!/bin/sh\necho \"aterm 0.99.0\"\n";
const OTHER: &str = "#!/bin/sh\necho \"other 1.0\"\n";

fn plist(app: &Path, id: &str) {
    crate::support::write(
        &app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>\n"
        ),
    );
}

/// What may happen at `$DEST/aterm.app`: nothing there, an aterm bundle
/// (replaced), a damaged aterm whose binary answers (repaired), or anything
/// else (left in place, with the remedy when it might be a damaged aterm).
#[test]
fn the_app_destination_verdict() {
    let scratch = Scratch::new();
    let ours = scratch.mkdir("own/aterm.app");
    let other = scratch.mkdir("own/other/aterm.app");
    let bare = scratch.mkdir("own/bare.app");
    let damaged = scratch.mkdir("own/damaged.app");
    let impostor = scratch.mkdir("own/impostor.app");
    let linked = scratch.join("own/linked.app");
    plist(&ours, "com.aterm.aterm");
    plist(&other, "org.example.not-aterm");
    scratch.stand_in("own/damaged.app/Contents/MacOS/aterm", ATERM);
    scratch.stand_in("own/impostor.app/Contents/MacOS/aterm", OTHER);
    symlink(&damaged, &linked);
    let mut lib = Lib::new();
    lib.step(format!("VERSION_PROBE_SECS={ANSWER_SECS}"));
    let verdict =
        |lib: &mut Lib, app: &Path| lib.call(format!("app_dest_verdict {}", q(s(app))), &[]);
    let absent = verdict(&mut lib, &scratch.join("own/none.app"));
    let is_ours = verdict(&mut lib, &ours);
    let is_other = verdict(&mut lib, &other);
    let is_bare = verdict(&mut lib, &bare);
    let is_damaged = verdict(&mut lib, &damaged);
    let is_impostor = verdict(&mut lib, &impostor);
    let is_linked = verdict(&mut lib, &linked);
    let ours_bundle = lib.call(format!("bundle_is_ours {}", q(s(&ours))), &[]);
    let other_bundle = lib.call(format!("bundle_is_ours {}", q(s(&other))), &[]);
    let other_id = lib.call(format!("bundle_identifier {}", q(s(&other))), &[]);
    let bare_bundle = lib.call(format!("bundle_is_ours {}", q(s(&bare))), &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[absent].out, "absent");
    assert_eq!(
        recs[bare_bundle].rc, 1,
        "a bundle with no Info.plist is not ours"
    );
    let bare_verdict = &recs[is_bare].out;
    assert!(
        bare_verdict.starts_with("foreign ")
            && bare_verdict.contains(&format!("mv '{0}' '{0}.broken'", s(&bare))),
        "an unreadable bundle that cannot answer is left WITH the remedy: {bare_verdict}"
    );
    assert!(
        recs[is_damaged].out.starts_with("damaged "),
        "{}",
        recs[is_damaged].out
    );
    assert!(
        recs[is_impostor].out.starts_with("foreign "),
        "{}",
        recs[is_impostor].out
    );
    assert!(
        recs[is_linked].out.starts_with("foreign "),
        "a symlink at the destination is never run to prove it is aterm: {}",
        recs[is_linked].out
    );
    // Bundle identifiers are read with `defaults`, which only macOS has.
    if cfg!(target_os = "macos") {
        assert_eq!(recs[ours_bundle].rc, 0);
        assert_eq!(recs[is_ours].out, "ours");
        assert_eq!(recs[other_bundle].rc, 1);
        assert_eq!(recs[other_id].out, "org.example.not-aterm");
        assert_eq!(
            recs[is_other].out,
            "foreign not an aterm bundle (CFBundleIdentifier=org.example.not-aterm)"
        );
    }
}

/// What may happen at `$BIN_DIR/aterm`: nothing there, our symlink (into a
/// bundle or the store, dangling or not), a pre-store regular file that
/// answers as aterm (replaced), or anything else (left, SKIPPED).
#[test]
fn the_bin_entry_verdict() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("store");
    let bundle_exe = scratch.join("own/aterm.app/Contents/MacOS/aterm");
    let entry = |name: &str| scratch.join(&format!("bin.{name}/aterm"));
    let absent = entry("absent");
    let bundle = entry("bundle");
    let in_store = entry("store");
    let elsewhere = entry("elsewhere");
    let pre_store = entry("pre-store");
    let foreign = entry("foreign");
    let unrunnable = entry("unrunnable");
    for link in [&bundle, &in_store, &elsewhere] {
        std::fs::create_dir_all(link.parent().expect("dir")).expect("mkdir");
    }
    symlink(&bundle_exe, &bundle);
    symlink(store.join("aterm"), &in_store);
    symlink("/usr/local/Cellar/other/bin/aterm", &elsewhere);
    scratch.stand_in("bin.pre-store/aterm", "#!/bin/sh\necho \"aterm 0.4.0\"\n");
    scratch.stand_in(
        "bin.foreign/aterm",
        "#!/bin/sh\necho \"aterm-ng 2.0 (the X terminal)\"\n",
    );
    scratch.write("bin.unrunnable/aterm", "not executable\n");
    let mut lib = Lib::new();
    lib.step(format!("VERSION_PROBE_SECS={ANSWER_SECS}"));
    let rows = [
        (&absent, "absent".to_string()),
        (&bundle, "ours".into()),
        (&in_store, "ours".into()),
        (
            &elsewhere,
            "foreign symlink points outside an aterm bundle or store (/usr/local/Cellar/other/bin/aterm)"
                .into(),
        ),
        (&pre_store, "pre-store".into()),
        (
            &foreign,
            "foreign not a symlink — a hand-built binary this installer did not place".into(),
        ),
        (
            &unrunnable,
            "foreign not a symlink — a hand-built binary this installer did not place".into(),
        ),
    ];
    let calls: Vec<(usize, usize)> = rows
        .iter()
        .map(|(path, _)| {
            (
                lib.call(
                    format!("bin_aterm_verdict {} {}", q(s(path)), q(s(&store))),
                    &[],
                ),
                lib.call(
                    format!("bin_aterm_plan {} {}", q(s(path)), q(s(&store))),
                    &[],
                ),
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((path, want), (verdict, plan)) in rows.iter().zip(calls) {
        assert_eq!(recs[verdict].out, *want, "{}", path.display());
        // --dry-run asks the same question, so the plan promises only what the
        // run does: link, replace a pre-store file, or leave a foreign one.
        let plan = &recs[plan].out;
        let p = s(path);
        match want.split(' ').next() {
            Some("absent" | "ours") => {
                assert!(plan.contains(&format!("the {p} symlink")), "{plan}")
            }
            Some("pre-store") => assert!(plan.contains(&format!("would REPLACE {p}")), "{plan}"),
            _ => assert!(plan.contains(&format!("{p} would be SKIPPED")), "{plan}"),
        }
    }
}

/// The verdict runs a file the installer did not place: never on the
/// installer's stdin (under `curl … | bash` that is the rest of the script),
/// and cut off when it never exits — after 5 s in production.
#[test]
fn a_foreign_binary_can_neither_swallow_nor_stall_the_installer() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("store");
    let reader = scratch.stand_in(
        "reader/aterm",
        "#!/bin/sh\ncat >/dev/null\necho \"not-aterm 1.0\"\n",
    );
    let staller = scratch.stand_in("staller/aterm", "#!/bin/sh\nexec sleep 60\n");
    let script = format!(
        "printf 'echo LINE-AFTER-THE-PROBE\\n' | {bash} -c '
			ATERM_INSTALL_LIBRARY_ONLY=1; . \"$1\" >/dev/null 2>&1
			VERSION_PROBE_SECS={ANSWER_SECS}
			printf \"verdict=%s\\n\" \"$(bin_aterm_verdict \"$2\" \"$3\")\"
			while IFS= read -r line; do eval \"$line\"; done' _ {install} {reader} {store}",
        bash = q(s(&bash())),
        install = q(s(&install_sh())),
        reader = q(s(&reader)),
        store = q(s(&store)),
    );
    let home = scratch.mkdir("home");
    let run = sh(&script, &[("PATH", system_path()), ("HOME", s(&home))]);
    assert_eq!(
        run.stdout,
        "verdict=foreign not a symlink — a hand-built binary this installer did not place\nLINE-AFTER-THE-PROBE\n",
        "{}",
        run.stderr
    );
    let mut lib = Lib::new();
    let production = lib.call(":", &["VERSION_PROBE_SECS"]);
    lib.step("VERSION_PROBE_SECS=1");
    let stalled = lib.call(
        format!("bin_aterm_verdict {} {}", q(s(&staller)), q(s(&store))),
        &[],
    );
    let started = std::time::Instant::now();
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[production].var("VERSION_PROBE_SECS"),
        "5",
        "an install waits 5 s for a foreign binary's --version"
    );
    assert_eq!(
        recs[stalled].out,
        "foreign not a symlink — a hand-built binary this installer did not place"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "cut off, not waited out"
    );
}

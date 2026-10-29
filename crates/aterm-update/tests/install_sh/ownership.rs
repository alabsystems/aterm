// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership and hang-bound tests for `install.sh` app destination and `aterm` bin entry checks.

use std::path::Path;

use aterm_spec::wait::{MUST_HAPPEN, PARKED_SECS, until_within};

use crate::support::{Lib, Scratch, bash, install_sh, q, read, s, sh, symlink, system_path};

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
    scratch.program("bin.pre-store/aterm", "#!/bin/sh\necho \"aterm 0.4.0\"\n");
    scratch.program(
        "bin.foreign/aterm",
        "#!/bin/sh\necho \"aterm-ng 2.0 (the X terminal)\"\n",
    );
    scratch.write("bin.unrunnable/aterm", "not executable\n");
    let mut lib = Lib::new();
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

const READER: &str = "#!/bin/sh\ncat >/dev/null\necho \"not-aterm 1.0\"\n";

fn staller() -> String {
    format!(
        "#!/bin/sh\necho \"${{STALL_ANSWER:?}}\"\n[ \"$1\" = \"${{STALL_FINISHES:-}}\" ] && exit 0\n\
         sleep {PARKED_SECS} &\necho \"$$ $! $PPID\" >\"${{STALL_PARKED:?}}\"\nwait\n"
    )
}

/// Time passes at once while a bound waits on a parked staller: the run it started, or
/// the shell that started it (root asks as nobody through one).
const PARKED_CLOCK: &str = r#"sleep() {
	local run parent watched
	watched="$(set +u; printf %s "$!")"
	if read -r run _ parent 2>/dev/null <"$STALL_PARKED" &&
		[[ "$run" == "$watched" || "$parent" == "$watched" ]]; then
		SECONDS=$((SECONDS + 3600))
		echo look >>"$STALL_PARKED.looks"
		return 0
	fi
	command sleep "$@"
}
uname() { echo Darwin; }"#;

#[test]
fn a_binary_asked_what_it_is_cannot_swallow_the_installers_stdin() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("store");
    let tmp = scratch.mkdir("tmp");
    let home = scratch.mkdir("home");
    let reader = scratch.program("rd/aterm", READER);
    let bundle = scratch.mkdir("rd/reader.app");
    scratch.stand_in("rd/reader.app/Contents/MacOS/aterm", READER);
    let dest = scratch.mkdir("rd/dest");
    scratch.stand_in("rd/dest/aterm.app/Contents/MacOS/aterm", READER);
    let apps = scratch.mkdir("rd/apps");
    let answering = scratch.stand_in("rd/apps/aterm.app/Contents/MacOS/aterm", ATERM);
    let bin = scratch.mkdir("rd/bin");
    scratch.program("rd/bin/aterm-ctl", READER);
    let placed = scratch.mkdir("rd/store");
    scratch.stand_in("rd/store/aterm", READER);
    let none = scratch.mkdir("rd/none");
    let candidate = scratch.stand_in("rd/candidate/aterm-linux", READER);
    let cases: [(&str, &[&Path], String); 7] = [
        (
            "bin_aterm_verdict \"$2\" \"$3\"",
            &[&reader, &store],
            "foreign not a symlink — a hand-built binary this installer did not place".into(),
        ),
        (
            "app_dest_verdict \"$2\"",
            &[&bundle],
            format!(
                "foreign no readable CFBundleIdentifier and no binary that answers as aterm — if \
                 it is a damaged aterm install, move it aside (mv '{0}' '{0}.broken') and re-run",
                s(&bundle)
            ),
        ),
        (
            "DEST=\"$2\" ATERM_INSTALL_DIR=\"$3\" find_bundle_cli",
            &[&dest, &apps],
            s(&answering).into(),
        ),
        (
            "BIN_DIR=\"$2\" STORE_DIR=\"$3\" retire_exposed_siblings; [[ -e \"$2/aterm-ctl\" ]] && echo kept",
            &[&bin, &store],
            "kept".into(),
        ),
        (
            "setup_aterm self-uninstall \"$2\" \"$3\"",
            &[&none, &placed],
            String::new(),
        ),
        (
            "secrets_aterm \"$2\" \"$3\"",
            &[&none, &placed],
            String::new(),
        ),
        (
            "linux_candidate_answers_tag \"$2\" v0.99.0; echo \"$?:$LINUX_CANDIDATE_SAID\"",
            &[&candidate],
            "1:not-aterm 1.0".into(),
        ),
    ];
    for (call, args, said) in cases {
        let args: Vec<String> = args.iter().map(|path| q(s(path))).collect();
        let script = format!(
            "printf 'echo LINE-AFTER-THE-PROBE\\n' | {bash} -c '
			ATERM_INSTALL_LIBRARY_ONLY=1; . \"$1\" >/dev/null 2>&1
			uname() {{ echo Darwin; }}
			printf \"said=%s\\n\" \"$({call})\"
			while IFS= read -r line; do eval \"$line\"; done' _ {install} {args}",
            bash = q(s(&bash())),
            install = q(s(&install_sh())),
            args = args.join(" "),
        );
        let run = sh(
            &script,
            &[
                ("PATH", system_path()),
                ("HOME", s(&home)),
                ("TMPDIR", s(&tmp)),
            ],
        );
        assert_eq!(
            run.stdout,
            format!("said={said}\nLINE-AFTER-THE-PROBE\n"),
            "{call}: {}",
            run.stderr
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_whole_run_hands_its_stdin_to_no_binary() {
    let scratch = Scratch::new();
    let home = scratch.home("whole");
    let read_log = scratch.join("whole/read.log");
    scratch.stand_in(
        "whole/home/Applications/aterm.app/Contents/MacOS/aterm",
        "#!/bin/sh\ncat >>\"${STUB_READ:?}\"\necho \"aterm 0.99.0\"\n",
    );
    let env = crate::support::hermetic(&home, &[("STUB_READ", s(&read_log))]);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let run = sh(
        &format!(
            "printf 'THE-INSTALLERS-STDIN\\n' | {} {} --no-app --no-toolchain --no-path",
            q(s(&bash())),
            q(s(&install_sh()))
        ),
        &env,
    );
    assert_eq!(run.code(), 0, "{}", run.all());
    assert!(
        run.stdout.contains("install.sh: linked aterm 0.99.0 -> "),
        "the bundle lane ran: {}",
        run.all()
    );
    assert_eq!(read(&read_log), "", "a run read the installer's stdin");
}

#[test]
fn a_binary_that_never_finishes_is_cut_off_and_answers_nothing() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("store");
    let stall = staller();
    let lone = scratch.program("stall/aterm", &stall);
    let damaged = scratch.mkdir("stall/damaged.app");
    scratch.stand_in("stall/damaged.app/Contents/MacOS/aterm", &stall);
    let dest = scratch.mkdir("stall/dest");
    scratch.stand_in("stall/dest/aterm.app/Contents/MacOS/aterm", &stall);
    let apps = scratch.mkdir("stall/apps");
    let answering = scratch.stand_in("stall/apps/aterm.app/Contents/MacOS/aterm", ATERM);
    let bin = scratch.mkdir("stall/bin");
    scratch.program("stall/bin/aterm-ctl", &stall);
    let placed = scratch.mkdir("stall/store");
    scratch.stand_in("stall/store/aterm", &stall);
    let none = scratch.mkdir("stall/none");
    let candidate = scratch.stand_in("stall/candidate/aterm-linux", &stall);
    let cases = [
        (
            "version",
            "aterm 0.99.0",
            "",
            format!("bin_aterm_verdict {} {}", q(s(&lone)), q(s(&store))),
            "foreign not a symlink — a hand-built binary this installer did not place".to_string(),
        ),
        (
            "damaged",
            "aterm 0.99.0",
            "",
            format!("app_dest_verdict {}", q(s(&damaged))),
            format!(
                "foreign no readable CFBundleIdentifier and no binary that answers as aterm — if \
                 it is a damaged aterm install, move it aside (mv '{0}' '{0}.broken') and re-run",
                s(&damaged)
            ),
        ),
        (
            "bundle",
            "aterm 0.99.0",
            "",
            format!(
                "DEST={} ATERM_INSTALL_DIR={} find_bundle_cli",
                q(s(&dest)),
                q(s(&apps))
            ),
            s(&answering).to_string(),
        ),
        (
            "ctl",
            "aterm-ctl 0.99.0",
            "",
            format!(
                "BIN_DIR={0} STORE_DIR={1} retire_exposed_siblings; [[ -e {0}/aterm-ctl ]] && echo kept",
                q(s(&bin)),
                q(s(&store))
            ),
            "kept".to_string(),
        ),
        (
            "verb",
            "aterm 0.99.0",
            "--version",
            format!(
                "setup_aterm self-uninstall {} {}",
                q(s(&none)),
                q(s(&placed))
            ),
            String::new(),
        ),
        (
            "secrets",
            "aterm 0.99.0",
            "--version",
            format!("secrets_aterm {} {}", q(s(&none)), q(s(&placed))),
            String::new(),
        ),
        (
            "linux",
            "aterm 0.99.0",
            "",
            format!(
                "linux_candidate_answers_tag {} v0.99.0; echo \"$?:$LINUX_CANDIDATE_SAID\"",
                q(s(&candidate))
            ),
            "2:".to_string(),
        ),
    ];
    let parked = |name: &str| scratch.join(&format!("stall/{name}.parked"));
    let mut lib = Lib::new();
    lib.step(PARKED_CLOCK);
    let calls: Vec<usize> = cases
        .iter()
        .map(|(name, answer, finishes, call, _)| {
            lib.step(format!(
                "export STALL_PARKED={} STALL_ANSWER={} STALL_FINISHES={}",
                q(s(&parked(name))),
                q(answer),
                q(finishes)
            ));
            lib.call(call.as_str(), &[])
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((name, _, _, call, want), at) in cases.iter().zip(calls) {
        assert_eq!(
            &recs[at].out, want,
            "{call}: a run cut off answered nothing"
        );
        assert_eq!(
            read(&scratch.join(&format!("stall/{name}.parked.looks")))
                .lines()
                .count(),
            1,
            "{call}: the bound reads the clock, so its first look past it is its last"
        );
        let ran = read(&parked(name));
        let child: u32 = ran
            .split_whitespace()
            .nth(1)
            .and_then(|pid| pid.parse().ok())
            .unwrap_or_else(|| panic!("{call}: the staller's line: {ran:?}"));
        until_within(
            MUST_HAPPEN,
            &format!("{call}: the cut-off run's process group, its parked child too, is killed"),
            || (!aterm_uds::process::pid_alive(child)).then_some(()),
        );
    }
}

/// A stand-in that parks without a line, as a first exec held at macOS's check does.
fn silent_staller() -> String {
    format!(
        "#!/bin/sh\nsleep {PARKED_SECS} &\necho \"$$ $! $PPID\" >\"${{STALL_PARKED:?}}\"\nwait\n"
    )
}

#[test]
fn a_bundle_with_an_unread_program_inventory_is_never_swapped() {
    let scratch = Scratch::new();
    let dest = scratch.mkdir("inventory/dest");
    let app = scratch.mkdir("inventory/dest/aterm.app");
    let programs = scratch.mkdir("inventory/dest/aterm.app/Contents/MacOS");
    let exe = scratch.write("inventory/dest/aterm.app/Contents/MacOS/a\nb", "program");
    symlink(&exe, &programs.join("linked"));
    scratch.mkdir("inventory/dest/aterm.app/Contents/MacOS/folder");
    let stage = scratch.mkdir("inventory/staged");
    let asked = scratch.join("inventory/asked");
    let mut lib = Lib::new();
    let probe = format!(
        "showed_it_ran() {{ printf '%s\\0' \"$1\" >> {}; }}; ",
        q(s(&asked))
    );
    let failed = lib.call(
        format!(
            "( {probe}find() {{ command find \"$@\"; return 1; }}; STAGE={}; \
             swap_staged_bundle {} )",
            q(s(&stage)),
            q(s(&dest))
        ),
        &[],
    );
    let known = lib.call(
        format!("( {probe}bundle_showed_it_ran {} )", q(s(&app))),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_ne!(
        recs[failed].rc, 0,
        "a partial inventory is no permission to swap"
    );
    assert!(recs[failed].err.contains("could not be inventoried"));
    assert!(exe.is_file() && stage.is_dir(), "the swap moved nothing");
    assert_eq!(recs[known].rc, 0, "{}", recs[known].err);
    let mut expected = s(&exe).as_bytes().to_vec();
    expected.push(0);
    assert_eq!(
        std::fs::read(&asked).unwrap(),
        expected,
        "only the complete inventory probes, preserving newlines and excluding links/directories"
    );

    std::fs::remove_file(&exe).unwrap();
    let mut lib = Lib::new();
    let empty = lib.call(format!("may_replace {}", q(s(&app))), &[]);
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[empty].rc, 0,
        "a known empty program folder is replaceable"
    );
}

#[test]
fn a_program_that_has_not_shown_it_ran_is_left_where_it_is() {
    let scratch = Scratch::new();
    let silent = silent_staller();
    let answers = scratch.program("proof/answers", ATERM);
    let fails = scratch.stand_in("proof/fails", "#!/bin/sh\nexit 3\n");
    // As an old bundle's aterm-drive answers `--version`: its error on stderr, exit 1.
    let quiet = scratch.stand_in("proof/quiet", "#!/bin/sh\necho 'unknown' >&2\nexit 1\n");
    let killed = scratch.stand_in("proof/killed", "#!/bin/sh\nkill -KILL $$\n");
    // A Mach-O header for a CPU no Mac runs now (32-bit PowerPC): the kernel refuses its exec.
    let header: Vec<u8> = [0xfeed_face_u32, 18, 0, 2, 0, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let refused = scratch.write("proof/refused", header);
    crate::support::chmod(&refused, 0o755);
    // A script whose interpreter is gone: the shell's own exit 126, no run.
    let unstarted = scratch.stand_in("proof/unstarted", "#!/nonexistent/interpreter\n");
    // A blank line is a line: it ran, whatever status it ends with, and so is a NUL.
    let blank = scratch.stand_in("proof/blank", "#!/bin/sh\necho\nexit 126\n");
    let nuls = scratch.stand_in("proof/nuls", "#!/bin/sh\nprintf '\\0\\0'\nexit 126\n");
    let blank_then_parked = scratch.stand_in(
        "proof/blank-parked",
        &format!(
            "#!/bin/sh\necho\nsleep {PARKED_SECS} &\necho \"$$ $! $PPID\" >\"${{STALL_PARKED:?}}\"\nwait\n"
        ),
    );
    // A home whose .bashrc prints, which bash reads under sshd's SSH_CLIENT.
    let home = scratch.mkdir("proof/home");
    scratch.write("proof/home/.bashrc", "echo bashrc\n");
    let said_then_parked = scratch.stand_in("proof/said", &staller());
    let stalls = scratch.stand_in("proof/stalls", &silent);
    let missing = scratch.join("proof/missing");
    let inert = scratch.write("proof/inert", "#!/bin/sh\nexit 0\n");
    let toolset = scratch.mkdir("proof/toolset/aterm.app");
    let siblings = toolset.join("Contents/MacOS");
    scratch.program("proof/toolset/aterm.app/Contents/MacOS/aterm", ATERM);
    scratch.program("proof/toolset/aterm.app/Contents/MacOS/aterm-cli", ATERM);
    scratch.program("proof/toolset/aterm.app/Contents/MacOS/atpkg", &silent);
    let whole = scratch.mkdir("proof/whole/aterm.app");
    scratch.program("proof/whole/aterm.app/Contents/MacOS/aterm", ATERM);
    symlink(&stalls, &whole.join("Contents/MacOS/atpkg"));
    let dest = scratch.mkdir("proof/dest");
    let held = scratch.program("proof/dest/aterm.app/Contents/MacOS/aterm", &silent);
    let staged = scratch.mkdir("proof/dest/aterm.app.installing.x");
    scratch.mkdir("proof/store");
    let placed = scratch.program("proof/store/aterm", &silent);
    let link = scratch.join("proof/store/aterm-ctl");
    symlink(&stalls, &link);
    let parked = |name: &str| scratch.join(&format!("proof/{name}.parked"));
    let root = |stand_in: &str| {
        format!(
            "installer_uid() {{ echo 0; }}; may_run() {{ return 1; }}; \
             run_as_user() {{ echo \"$1\" >>\"$TMPDIR/as\"; shift; {stand_in}; }}"
        )
    };
    let mut lib = Lib::new();
    lib.step(PARKED_CLOCK);
    lib.step(format!(
        "export STALL_PARKED={} STALL_ANSWER=x STALL_FINISHES=",
        q(s(&parked("none")))
    ));
    let stalled = |lib: &mut Lib, name: &str, call: String| {
        lib.step(format!("export STALL_PARKED={}", q(s(&parked(name)))));
        lib.call(call, &["UNPROVEN"])
    };
    let mut proven: Vec<usize> = [
        &answers, &fails, &killed, &missing, &inert, &refused, &blank,
    ]
    .into_iter()
    .map(|exe| lib.call(format!("showed_it_ran {}", q(s(exe))), &[]))
    .collect();
    proven.push(stalled(
        &mut lib,
        "said",
        format!("showed_it_ran {}", q(s(&said_then_parked))),
    ));
    for exe in [&answers, &quiet, &refused, &blank, &nuls] {
        proven.push(lib.call(
            format!("( {}; showed_it_ran {} )", root("exec \"$@\""), q(s(exe))),
            &[],
        ));
    }
    proven.push(lib.call(format!("bundle_showed_it_ran {}", q(s(&whole))), &[]));
    for path in [&whole, &answers, &missing, &link] {
        proven.push(lib.call(format!("may_replace {}", q(s(path))), &[]));
    }
    let cut_off = stalled(
        &mut lib,
        "stalls",
        format!("showed_it_ran {}", q(s(&stalls))),
    );
    let mut interrupted = Vec::new();
    for status in [129, 130, 131, 143] {
        for as_root in [false, true] {
            let identity = if as_root {
                root("return 1") + "; "
            } else {
                String::new()
            };
            interrupted.push(lib.call(
                format!(
                    "( {identity}bounded() {{ return {status}; }}; showed_it_ran {}; rc=$?; \
                     printf %s \"$UNPROVEN\"; exit $rc )",
                    q(s(&answers))
                ),
                &[],
            ));
        }
    }
    let sudo_refused = lib.call(
        format!("( {}; showed_it_ran {} )", root("return 1"), q(s(&answers))),
        &[],
    );
    let unasked = lib.call(
        format!("( {}; may_replace {} )", root("return 1"), q(s(&answers))),
        &[],
    );
    let as_nobody_cut_off = stalled(
        &mut lib,
        "nobody",
        format!(
            "( {}; showed_it_ran {} )",
            root("exec \"$@\""),
            q(s(&stalls))
        ),
    );
    let said = |stand_in: Option<&str>| {
        format!(
            "( {}showed_it_ran {}; rc=$?; printf '%s\\n%s' \"$UNPROVEN\" \"$UNPROVEN_THEN\"; \
             exit $rc )",
            stand_in.map_or(String::new(), |s| root(s) + "; "),
            q(s(&unstarted))
        )
    };
    let not_started = lib.call(said(None), &[]);
    let not_started_as_nobody = lib.call(said(Some("exec \"$@\"")), &[]);
    proven.push(stalled(
        &mut lib,
        "blank",
        format!(
            "( {}; showed_it_ran {} )",
            root("exec \"$@\""),
            q(s(&blank_then_parked))
        ),
    ));
    let sshd_rc = lib.call(
        format!(
            "( export HOME={} SSH_CLIENT='10.0.0.2 50000 22'; unset SHLVL; {}; showed_it_ran {} )",
            q(s(&home)),
            root("exec \"$@\""),
            q(s(&unstarted))
        ),
        &[],
    );
    let asked_as = lib.call("cat \"$TMPDIR/as\"", &[]);
    let sibling = stalled(
        &mut lib,
        "toolset",
        format!("bundle_showed_it_ran {}", q(s(&toolset))),
    );
    let early = stalled(
        &mut lib,
        "dest",
        format!("may_replace {}/aterm.app", q(s(&dest))),
    );
    let swap = stalled(
        &mut lib,
        "swap",
        format!(
            "( STAGE={}; swap_staged_bundle {} )",
            q(s(&staged)),
            q(s(&dest))
        ),
    );
    let replace = stalled(
        &mut lib,
        "store",
        format!(
            "( STORE_DIR={}; place_store_binary {} )",
            q(s(placed.parent().unwrap())),
            q(s(&answers))
        ),
    );
    let recs = lib.run(&scratch);
    for i in &proven {
        assert_eq!(
            recs[*i].rc, 0,
            "a run that ended or wrote a line, or no program: {}",
            recs[*i].err
        );
    }
    for i in [
        cut_off,
        sudo_refused,
        unasked,
        as_nobody_cut_off,
        sibling,
        not_started,
        not_started_as_nobody,
        sshd_rc,
    ] {
        assert_eq!(recs[i].rc, 1, "no proof: {}", recs[i].err);
    }
    assert_eq!(
        recs[cut_off].var("UNPROVEN"),
        format!("{} did not answer within 60s", s(&stalls))
    );
    for i in interrupted {
        assert_eq!(recs[i].rc, 1, "no proof: {}", recs[i].err);
        assert_eq!(
            recs[i].out,
            format!("{} was interrupted before it answered", s(&answers))
        );
    }
    assert_eq!(
        recs[sibling].var("UNPROVEN"),
        format!("{} did not answer within 60s", s(&siblings.join("atpkg"))),
        "a toolset's aterm-cli starts atpkg"
    );
    assert_eq!(
        recs[asked_as].out,
        "nobody\n".repeat(11).trim_end(),
        "root asks a program it may not run only as nobody"
    );
    let u = s(&unstarted);
    // bash 3.2 (macOS's /bin/bash) ends a missing interpreter's exec with 126,
    // bash 5.1 and later (Homebrew's, Linux's) with 127.
    let not_run = |why: &str, by: &str| {
        let said = |rc| format!("{u} may not have started{by} (exit {rc}, saying `");
        (why.starts_with(&said(126)) && why.contains("bad interpreter"))
            || (why.starts_with(&said(127)) && why.contains("required file not found"))
    };
    let (why, then) = recs[not_started].out.split_once('\n').unwrap();
    assert!(
        not_run(why, ""),
        "the shell's own failure to exec is no run: {why}"
    );
    assert_eq!(
        then,
        "after a restart, when macOS is checking nothing, remove it yourself and run this again"
    );
    let (why, then) = recs[not_started_as_nobody].out.split_once('\n').unwrap();
    assert!(not_run(why, " as nobody"), "{why}");
    assert_eq!(
        then,
        "run install.sh without sudo, as the account whose aterm it is, which asks it itself"
    );
    assert!(
        recs[unasked].err.contains(&format!(
            "{} was left as it is: {} could not be run as nobody to ask it, so macOS",
            s(&answers),
            s(&answers)
        )) && recs[unasked].err.contains(
            "run install.sh without sudo, as the account whose aterm it is, which asks it itself"
        ),
        "a probe that could not ask says what helps: {}",
        recs[unasked].err
    );
    for (i, path, program) in [
        (
            early,
            s(&dest).to_string() + "/aterm.app",
            s(&held).to_string(),
        ),
        (
            swap,
            s(&dest).to_string() + "/aterm.app",
            s(&held).to_string(),
        ),
        (replace, s(&placed).to_string(), s(&placed).to_string()),
    ] {
        assert_eq!(recs[i].rc, 1, "{path}: {}", recs[i].err);
        assert!(
            recs[i].err.contains(&format!(
                "{path} was left as it is: {program} did not answer within 60s, so macOS"
            )) && recs[i]
                .err
                .contains("run this again in 10 minutes, or after a restart"),
            "{}",
            recs[i].err
        );
    }
    assert!(held.is_file() && staged.is_dir(), "the swap moved nothing");
    assert_eq!(read(&placed), silent, "the store's aterm stays as it is");
}

#[test]
fn setup_aterm_asks_each_binary_once() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("once/store");
    let bin = scratch.mkdir("once/bin");
    let log = scratch.join("once/asked.log");
    let aterm = scratch.stand_in(
        "once/store/aterm",
        "#!/bin/sh\necho \"$*\" >>\"${STUB_LOG:?}\"\n[ \"$1\" = --version ] || exit 1\n\
         echo \"aterm 0.99.0\"\n",
    );
    symlink(&aterm, &bin.join("aterm"));
    let mut lib = Lib::new();
    lib.env("STUB_LOG", s(&log));
    let found = lib.call(
        format!(
            "PLACED_ATERM={} setup_aterm shell-setup {} {}",
            q(s(&aterm)),
            q(s(&bin)),
            q(s(&store))
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[found].out, "", "it knows no shell-setup");
    assert_eq!(
        read(&log),
        "--version\npkg help shell-setup\n",
        "the placed path, the bin link and the store's path are one binary, asked once"
    );
}

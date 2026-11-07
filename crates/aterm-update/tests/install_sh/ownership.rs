// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership and hang-bound tests for `install.sh` app destination and `aterm` bin entry checks.

use std::path::{Path, PathBuf};

use aterm_spec::wait::{MUST_HAPPEN, PARKED_SECS, until_within};

use crate::support::{
    HELD, Lib, Scratch, bash, install_sh, q, read, s, sh, symlink, system_path, tree,
};

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
    bin_entry_verdict(&Scratch::new(), None);
}

/// [`the_bin_entry_verdict`] in `scratch`, each run of `held` cut off unrun ([`Lib::hold`]).
fn bin_entry_verdict(scratch: &Scratch, held: Option<&Path>) {
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
    if let Some(held) = held {
        lib.hold(held);
    }
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
    // `foreign`'s verdict is the same whether it answered or the bound cut it off unrun: its
    // answer shows it ran.
    let answered = lib.call(
        format!("first_line_bounded {} --version", q(s(&foreign))),
        &[],
    );
    let recs = lib.run(scratch);
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
    let answered = &recs[answered];
    each_showed_it_ran([(
        foreign.as_path(),
        answered.code.as_str(),
        !answered.out.is_empty(),
    )]);
}

/// The source lane puts its `aterm` on PATH by the bin entry verdict: a pre-store aterm there is
/// replaced by the link to the binary the lane placed, and a foreign entry is left as it is, the
/// lane then saying its command is not on PATH.
#[test]
fn the_source_lane_links_its_bin_entry_only_where_it_may() {
    let scratch = Scratch::new();
    let built = scratch.stand_in("lane/built/aterm", r#"#!/bin/sh
if [ "$1" = pkg ]; then
    [ "$#" = 4 ] && [ "$2" = shell-setup ] && [ "$3" = --installed-command ] && [ "$4" = "$0" ] || exit 98
    printf '%s\n' "$0" >>"${DECLARED:?}"
    exit "${RECORD_STATUS:?}"
fi
echo 'aterm 0.99.0'
"#);
    scratch.program(
        "lane/pre-store/bin/aterm",
        "#!/bin/sh\necho \"aterm 0.4.0\"\n",
    );
    let elsewhere = Path::new("/usr/local/Cellar/other/bin/aterm");
    symlink(elsewhere, &scratch.mkdir("lane/foreign/bin").join("aterm"));
    let mut lib = Lib::new();
    let lanes: Vec<(&str, usize, PathBuf, PathBuf)> = ["pre-store", "foreign", "record-refused"]
        .into_iter()
        .map(|name| {
            let store = scratch.mkdir(&format!("lane/{name}/store"));
            let bin = scratch.mkdir(&format!("lane/{name}/bin"));
            let at = lib.call(
                format!(
                    "(export DECLARED={} RECORD_STATUS={}; {})",
                    q(s(&scratch.join(&format!("lane/{name}/declared")))),
                    if name == "record-refused" { 37 } else { 0 },
                    source_lane(
                        &q(s(&scratch.mkdir(&format!("lane/{name}/source")))),
                        &q(s(&store)),
                        &q(s(&bin)),
                        &q(s(&built)),
                    )
                ),
                &[],
            );
            (name, at, store, bin)
        })
        .collect();
    let recs = lib.run(&scratch);
    for (name, at, store, bin) in lanes {
        let rec = &recs[at];
        let line = |start: &str| rec.out.lines().any(|line| line.starts_with(start));
        assert_eq!(rec.rc, 0, "{name}: {}", rec.err);
        assert!(
            line(&format!(
                "install.sh: installed aterm 0.99.0 -> {}",
                s(&store)
            )),
            "{name}: the placed binary answered: {}",
            rec.out
        );
        let entry = bin.join("aterm");
        let now = std::fs::read_link(&entry).expect("a link at the bin entry");
        assert_eq!(
            read(&scratch.join(&format!("lane/{name}/declared"))),
            format!("{}\n", s(&store.join("aterm"))),
            "the just-placed runtime is invoked once, independently of PATH"
        );
        if name != "foreign" {
            assert_eq!(now, store.join("aterm"), "{name}: {}", rec.err);
            assert!(line("  ONE command on PATH: "), "{name}: {}", rec.out);
        } else {
            assert_eq!(now, elsewhere, "{name}: the foreign entry is left as it is");
            assert!(line("  NOT on PATH: "), "{name}: {}", rec.out);
            assert!(
                rec.err
                    .starts_with(&format!("install.sh: SKIPPED {}", s(&entry))),
                "{name}: {}",
                rec.err
            );
        }
        if name == "record-refused" {
            assert!(
                rec.err
                    .contains("installed command declaration was not recorded")
            );
            assert!(
                store.join("aterm").exists(),
                "a recording refusal keeps the installed image"
            );
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

/// `install_cli_from_source` in a subshell (a failed step of it `exit`s), its build a copy of
/// `BUILT` into the checkout's `target/release`. Each argument is a shell word.
fn source_lane(root: &str, store: &str, bin: &str, built: &str) -> String {
    format!(
        "(cargo_build_pinned() {{ mkdir -p \"$1/target/release\" && cp \"$BUILT\" \"$1/target/release/aterm\"; }}; \
         ROOT={root} STORE_DIR={store} BIN_DIR={bin} BUILT={built} CARGO_BIN=cargo CARGO_VERSION=1 \
         CARGO_TOOLCHAIN=trust; install_cli_from_source)"
    )
}

/// `install_cli` in a subshell, on its bundle lane: find_bundle_cli, whose own ask has rows of
/// its own, finds `TARGET`, and the shell setup after the link is left out. Each argument is a
/// shell word.
fn bundle_lane(target: &str, bin: &str, store: &str) -> String {
    format!(
        "(find_bundle_cli() {{ printf %s \"$TARGET\"; }}; setup_completions_and_man() {{ :; }}; \
         TARGET={target} BIN_DIR={bin} STORE_DIR={store} LINUX_APP_INSTALLED=0 CLI_CARGO_SKIP=; \
         install_cli)"
    )
}

/// The lines of `lane` that start with `line` (the one a row is about), then `status` and the
/// pipeline's exit status: under pipefail, 0 only when the lane ended 0 and printed that line.
fn kept(lane: &str, line: &str) -> String {
    format!("{lane} | grep \"^{line}\"; echo \"status $?\"")
}

/// The pids a staller wrote to `parked` as it parked (`$$ $! $PPID`), which show it ran, past
/// macOS's check of its first exec, before the bound cut it off; `None` when it wrote none, as
/// one the check still held at the bound writes none.
fn parked_pids(parked: &Path) -> Option<[u32; 3]> {
    read(parked)
        .split_whitespace()
        .map(|pid| pid.parse().ok())
        .collect::<Option<Vec<u32>>>()
        .and_then(|pids| pids.try_into().ok())
}

/// Fails the test unless each program in `runs` (with the call that ran it, and whether a line
/// it wrote shows it ran) showed it ran, naming each that did not: a program install.sh's bound
/// may have cut off unrun, as it cuts off a first exec macOS's check still holds, where that
/// cut-off changes nothing else the test checks. The failing test keeps its scratch (`Drop for
/// Scratch`); a pass would remove a file macOS may still be checking. A test checks it last,
/// after every check that shows its other programs ran.
#[track_caller]
fn each_showed_it_ran<'a>(runs: impl IntoIterator<Item = (&'a Path, &'a str, bool)>) {
    let unshown: Vec<String> = runs
        .into_iter()
        .filter(|(_, _, shown)| !shown)
        .map(|(program, call, _)| format!("\n  {}: {call}", program.display()))
        .collect();
    assert!(unshown.is_empty(), "{UNSHOWN}{}", unshown.concat());
}

/// What [`each_showed_it_ran`] fails with, before the programs it names.
const UNSHOWN: &str = "cut off before it showed it ran, so this test keeps its scratch:";

#[test]
fn a_binary_asked_what_it_is_cannot_swallow_the_installers_stdin() {
    asked_what_it_is(&Scratch::new(), None);
}

/// [`a_binary_asked_what_it_is_cannot_swallow_the_installers_stdin`] in `scratch`, each run of
/// `held` cut off unrun ([`HELD`]).
fn asked_what_it_is(scratch: &Scratch, held: Option<&Path>) {
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
    let ctl = scratch.program("rd/bin/aterm-ctl", READER);
    let placed = scratch.mkdir("rd/store");
    scratch.stand_in("rd/store/aterm", READER);
    let none = scratch.mkdir("rd/none");
    let candidate = scratch.stand_in("rd/candidate/aterm-linux", READER);
    let on_path = scratch.join("rd");
    let checkout = scratch.mkdir("rd/co");
    scratch.write(
        "rd/co/rust-toolchain.toml",
        "[toolchain]\nchannel = \"trust\"\n",
    );
    let source = scratch.mkdir("rd/source");
    let source_store = scratch.mkdir("rd/source-store");
    let source_bin = scratch.mkdir("rd/source-bin");
    // Two lines: the `installed` and `linked` lines name the binary by its first alone.
    let two_lines = "#!/bin/sh\ncat >/dev/null\necho \"aterm 0.99.0\"\necho \"a second line\"\n";
    let built = scratch.stand_in("rd/built/aterm", two_lines);
    let source_call = kept(
        &source_lane("\"$2\"", "\"$3\"", "\"$4\"", "\"$5\""),
        "install.sh: installed ",
    );
    let target = scratch.stand_in("rd/linked.app/Contents/MacOS/aterm", two_lines);
    let linked_bin = scratch.mkdir("rd/linked-bin");
    let bundle_call = kept(
        &bundle_lane("\"$2\"", "\"$3\"", "\"$4\""),
        "install.sh: linked ",
    );
    let cases: [(&str, &[&Path], String); 10] = [
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
        (
            "PATH=\"$2:$PATH\" find_managed_targo \"$3\" || printf %s \"$MANAGED_DECLINE\"",
            &[&on_path, &checkout],
            "no aterm-managed Trust store is in reach (no `aterm` to ask, no `targo` on PATH)"
                .into(),
        ),
        (
            source_call.as_str(),
            &[&source, &source_store, &source_bin, &built],
            format!(
                "install.sh: installed aterm 0.99.0 -> {}\nstatus 0",
                s(&source_store)
            ),
        ),
        (
            bundle_call.as_str(),
            &[&target, &linked_bin, &store],
            format!(
                "install.sh: linked aterm 0.99.0 -> {}/aterm (follows the app's updates)\nstatus 0",
                s(&linked_bin)
            ),
        ),
    ];
    for (call, args, said) in cases {
        let args: Vec<String> = args.iter().map(|path| q(s(path))).collect();
        let script = format!(
            "printf 'echo LINE-AFTER-THE-PROBE\\n' | {bash} -c '
			ATERM_INSTALL_LIBRARY_ONLY=1; . \"$1\" >/dev/null 2>&1
			uname() {{ echo Darwin; }}
			{hold}
			printf \"said=%s\\n\" \"$({call})\"
			while IFS= read -r line; do eval \"$line\"; done' _ {install} {args}",
            bash = q(s(&bash())),
            hold = held.map_or("", |_| HELD),
            install = q(s(&install_sh())),
            args = args.join(" "),
        );
        let run = sh(
            &script,
            &[
                ("PATH", system_path()),
                ("HOME", s(&home)),
                ("TMPDIR", s(&tmp)),
                ("HELD_EXE", held.map_or("", s)),
            ],
        );
        assert_eq!(
            run.stdout,
            format!("said={said}\nLINE-AFTER-THE-PROBE\n"),
            "{call}: {}",
            run.stderr
        );
    }
    // The verdicts on `reader` and `ctl` are the same whether they answered or the bound cut
    // them off unrun: their answers show they ran.
    let mut lib = Lib::new();
    if let Some(held) = held {
        lib.hold(held);
    }
    let asked = [&reader, &ctl].map(|program| {
        let call = lib.call(
            format!("first_line_bounded {} --version", q(s(program))),
            &[],
        );
        (program, call)
    });
    let recs = lib.run(scratch);
    each_showed_it_ran(asked.map(|(program, at)| {
        (
            program.as_path(),
            recs[at].code.as_str(),
            !recs[at].out.is_empty(),
        )
    }));
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
    let on_path = scratch.join("stall");
    let checkout = scratch.mkdir("stall/co");
    scratch.write(
        "stall/co/rust-toolchain.toml",
        "[toolchain]\nchannel = \"trust\"\n",
    );
    let source_store = scratch.mkdir("stall/source-store");
    let source = kept(
        &source_lane(
            &q(s(&scratch.mkdir("stall/source"))),
            &q(s(&source_store)),
            &q(s(&scratch.mkdir("stall/source-bin"))),
            &q(s(&scratch.stand_in("stall/built/aterm", &stall))),
        ),
        "install.sh: installed ",
    );
    // The source lane asks the binary it placed twice, each ask bounded: once to record it
    // (`pkg shell-setup --installed-command`), then its version. Each case parks one of them.
    let record_store = scratch.mkdir("stall/record-store");
    let record = kept(
        &source_lane(
            &q(s(&scratch.mkdir("stall/record"))),
            &q(s(&record_store)),
            &q(s(&scratch.mkdir("stall/record-bin"))),
            &q(s(&scratch.stand_in("stall/record-built/aterm", &stall))),
        ),
        "install.sh: installed ",
    );
    let linked_bin = scratch.mkdir("stall/linked-bin");
    let linked = kept(
        &bundle_lane(
            &q(s(
                &scratch.stand_in("stall/linked.app/Contents/MacOS/aterm", &stall)
            )),
            &q(s(&linked_bin)),
            &q(s(&store)),
        ),
        "install.sh: linked ",
    );
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
        (
            "managed",
            "targo → /nowhere/store/trust/1/bin/targo — managed 1",
            "",
            format!(
                "PATH={}:$PATH find_managed_targo {} || printf %s \"$MANAGED_DECLINE\"",
                q(s(&on_path)),
                q(s(&checkout))
            ),
            "no aterm-managed Trust store is in reach (no `aterm` to ask, no `targo` on PATH)"
                .to_string(),
        ),
        (
            "usage",
            "  --own-man",
            "",
            format!(
                "help_names {} shell-setup --own-man || echo unnamed",
                q(s(&lone))
            ),
            "unnamed".to_string(),
        ),
        (
            "source",
            "aterm 0.99.0",
            "pkg",
            source,
            format!(
                "install.sh: installed aterm (it did not say its version) -> {}\nstatus 0",
                s(&source_store)
            ),
        ),
        (
            "record",
            "aterm 0.99.0",
            "--version",
            record,
            format!(
                "install.sh: installed aterm 0.99.0 -> {}\nstatus 0",
                s(&record_store)
            ),
        ),
        (
            "linked",
            "aterm 0.99.0",
            "",
            linked,
            format!(
                "install.sh: linked aterm (it did not say its version) -> {}/aterm (follows the \
                 app's updates)\nstatus 0",
                s(&linked_bin)
            ),
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
        let Some([_, child, _]) = parked_pids(&parked(name)) else {
            panic!("{call}: the staller's line: {:?}", read(&parked(name)))
        };
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
    left_where_it_is(&Scratch::new(), None);
}

/// [`a_program_that_has_not_shown_it_ran_is_left_where_it_is`] in `scratch`, each run of
/// `held` cut off unrun ([`Lib::hold`]).
fn left_where_it_is(scratch: &Scratch, held: Option<&Path>) {
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
    let dest_aterm = scratch.program("proof/dest/aterm.app/Contents/MacOS/aterm", &silent);
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
    if let Some(held) = held {
        lib.hold(held);
    }
    lib.step(format!(
        "export STALL_PARKED={} STALL_ANSWER=x STALL_FINISHES=",
        q(s(&parked("none")))
    ));
    // Every staller the bound cuts off, and the line it writes as it parks (parked_pids).
    let mut cut = Vec::new();
    let mut stalled = |lib: &mut Lib, name: &str, staller: &Path, call: String| {
        cut.push((parked(name), staller.to_path_buf(), call.clone()));
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
        &said_then_parked,
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
        &stalls,
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
        &stalls,
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
        &blank_then_parked,
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
        &siblings.join("atpkg"),
        format!("bundle_showed_it_ran {}", q(s(&toolset))),
    );
    let early = stalled(
        &mut lib,
        "dest",
        &dest_aterm,
        format!("may_replace {}/aterm.app", q(s(&dest))),
    );
    let swap = stalled(
        &mut lib,
        "swap",
        &dest_aterm,
        format!(
            "( STAGE={}; swap_staged_bundle {} )",
            q(s(&staged)),
            q(s(&dest))
        ),
    );
    let replace = stalled(
        &mut lib,
        "store",
        &placed,
        format!(
            "( STORE_DIR={}; place_store_binary {} )",
            q(s(placed.parent().unwrap())),
            q(s(&answers))
        ),
    );
    // The source lane asks before its build, so a store binary it may not replace costs none.
    let checkout = scratch.mkdir("proof/source");
    let unbuilt = stalled(
        &mut lib,
        "source",
        &placed,
        source_lane(
            &q(s(&checkout)),
            &q(s(placed.parent().unwrap())),
            &q(s(&scratch.mkdir("proof/source-bin"))),
            &q(s(&answers)),
        ),
    );
    let recs = lib.run(scratch);
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
            s(&dest_aterm).to_string(),
        ),
        (
            swap,
            s(&dest).to_string() + "/aterm.app",
            s(&dest_aterm).to_string(),
        ),
        (replace, s(&placed).to_string(), s(&placed).to_string()),
        (unbuilt, s(&placed).to_string(), s(&placed).to_string()),
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
    assert!(
        dest_aterm.is_file() && staged.is_dir(),
        "the swap moved nothing"
    );
    assert_eq!(read(&placed), silent, "the store's aterm stays as it is");
    assert!(
        !checkout.join("target").exists(),
        "the source lane built nothing: {}",
        recs[unbuilt].out
    );
    each_showed_it_ran(cut.iter().map(|(parked, staller, call)| {
        (
            staller.as_path(),
            call.as_str(),
            parked_pids(parked).is_some(),
        )
    }));
}

/// A program install.sh's bound cut off before it showed it ran fails its test, whatever else
/// the test checks, so its scratch keeps the program: each test here with a program whose
/// verdicts are the same answered or cut off unrun, run with that program held (each run of it
/// cut off at once, unrun, as the bound cuts off a first exec macOS's check still holds), fails
/// naming that program alone ([`each_showed_it_ran`]), and the program is still there. NEGATIVE
/// CONTROL: each test passes unheld (its own `#[test]`), every program having shown it ran.
#[test]
fn a_program_held_past_its_bound_fails_its_test_and_keeps_its_scratch() {
    type Test = fn(&Scratch, Option<&Path>);
    let tests: [(Test, &str); 6] = [
        (
            left_where_it_is,
            "proof/toolset/aterm.app/Contents/MacOS/atpkg",
        ),
        (
            left_where_it_is,
            "proof/dest/aterm.app/Contents/MacOS/aterm",
        ),
        (left_where_it_is, "proof/store/aterm"),
        (bin_entry_verdict, "bin.foreign/aterm"),
        (asked_what_it_is, "rd/aterm"),
        (asked_what_it_is, "rd/bin/aterm-ctl"),
    ];
    for (test, rel) in tests {
        let mut laid = None;
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let scratch = Scratch::new();
            let held = scratch.join(rel);
            laid = Some((scratch.path().to_path_buf(), held.clone()));
            test(&scratch, Some(&held));
        }));
        let why = failed.expect_err(&format!("{rel}: its test passed held"));
        let why = why.downcast_ref::<String>().map_or("", String::as_str);
        let (root, held) = laid.expect("laid");
        let named: Vec<&str> = why
            .strip_prefix(UNSHOWN)
            .map_or(Vec::new(), |named| named.lines().skip(1).collect());
        let at = format!("  {}: ", held.display());
        assert!(
            !named.is_empty() && named.iter().all(|line| line.starts_with(&at)),
            "{rel}: {why}"
        );
        assert!(
            held.is_file(),
            "{} went with its failing test",
            held.display()
        );
        // Every other program there showed it ran, and the held one never started: nothing in
        // the kept scratch is being checked.
        std::fs::remove_dir_all(&root).expect("the kept scratch removed");
    }
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

/// A stand-in that logs each run (`$0`) to STALL_LOG, then parks with no line, as a binary
/// macOS's first-exec check holds does; `tag` makes it a file of its own.
fn logged_staller(tag: &str) -> String {
    format!(
        "#!/bin/sh\n# {tag}\necho \"$0\" >>\"${{STALL_LOG:?}}\"\nsleep {PARKED_SECS} &\n\
         echo \"$$ $! $PPID\" >\"${{STALL_PARKED:?}}\"\nwait\n"
    )
}

/// A stand-in that logs each run (`$0`) to STALL_LOG and parks with no line on its first run
/// only, by whatever path, as a first exec macOS's check holds past one detector does, then
/// answers as aterm; `tag` makes it a file of its own and names its mark beside STALL_PARKED.
fn held_once(tag: &str) -> String {
    format!(
        "#!/bin/sh\n# {tag}\necho \"$0\" >>\"${{STALL_LOG:?}}\"\nheld=\"${{STALL_PARKED:?}}.{tag}.held\"\n\
         if [ ! -e \"$held\" ]; then\n: >\"$held\"\nsleep {PARKED_SECS} &\n\
         echo \"$$ $! $PPID\" >\"$STALL_PARKED\"\nwait\nfi\necho \"aterm 0.99.0\"\n"
    )
}

/// The `<device> <inode>` install.sh's record of what a run asked keeps for the file `path`
/// names, through links.
fn identity(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).expect("a stand-in's file");
    format!("{} {}", meta.dev(), meta.ino())
}

/// A library batch over parked stand-ins that log to `log`, with the run's record named
/// (keep_asked) and the machine's own /Applications, which find_bundle_cli also looks in, left
/// out.
fn record_batch(scratch: &Scratch, log: &Path, parked: &str) -> Lib {
    let mut lib = Lib::new();
    lib.env("STALL_LOG", s(log));
    lib.step(PARKED_CLOCK);
    lib.step("may_run() { [[ \"$1\" != /Applications/* ]]; }");
    lib.step(format!(
        "export STALL_PARKED={}",
        q(s(&scratch.join(parked)))
    ));
    lib.step("keep_asked");
    lib
}

/// No run of a batch leaves its record of what it asked in TMPDIR.
fn no_record_left(scratch: &Scratch) {
    let left: Vec<String> = tree(&scratch.join("tmp"))
        .into_iter()
        .filter(|entry| entry.starts_with("aterm-asked."))
        .collect();
    assert!(left.is_empty(), "the run's record outlived it: {left:?}");
}

/// A bundle that never answers runs twice in a run, its first ask's two detectors, however many
/// steps look for an aterm and by whatever path: find_bundle_cli (DEST is also
/// ATERM_INSTALL_DIR), toolset_driver, setup_aterm, secrets_aterm, find_managed_targo by
/// PATH's link to it, and toolset_driver and setup_aterm by a BIN_DIR link to it, which
/// toolset_driver does not hand to `pkg seed`. Another binary is still asked, an answering
/// bundle after it still found, and a binary renamed into the wedged one's place since is
/// asked afresh.
#[test]
fn a_binary_cut_off_twice_is_not_run_again_in_the_run() {
    let scratch = Scratch::new();
    let wedged = scratch.mkdir("cut/dest/aterm.app");
    plist(&wedged, "com.aterm.aterm");
    let wedged_exe = scratch.stand_in(
        "cut/dest/aterm.app/Contents/MacOS/aterm",
        &logged_staller("wedged"),
    );
    // Read before the run renames another binary into the wedged one's place.
    let wedged_id = identity(&wedged_exe);
    let other = scratch.mkdir("cut/other/aterm.app");
    plist(&other, "com.aterm.aterm");
    let other_exe = scratch.stand_in(
        "cut/other/aterm.app/Contents/MacOS/aterm",
        &logged_staller("other"),
    );
    let apps = scratch.mkdir("cut/apps");
    let answering = scratch.stand_in("cut/apps/aterm.app/Contents/MacOS/aterm", ATERM);
    let bin = scratch.mkdir("cut/bin");
    let store = scratch.mkdir("cut/store");
    let linked = scratch.mkdir("cut/linked");
    symlink(&wedged_exe, &linked.join("aterm"));
    let on_path = scratch.mkdir("cut/path");
    symlink(&wedged_exe, &on_path.join("aterm"));
    let checkout = scratch.mkdir("cut/co");
    scratch.write(
        "cut/co/rust-toolchain.toml",
        "[toolchain]\nchannel = \"trust\"\n",
    );
    let renamed = scratch.stand_in(
        "cut/renamed/aterm",
        "#!/bin/sh\n# renamed\necho \"aterm 0.99.1\"\n",
    );
    let log = scratch.join("cut/runs.log");
    let folder = |app: &Path| q(s(app.parent().expect("an app's folder")));
    let mut lib = record_batch(&scratch, &log, "cut/parked");
    lib.step(format!(
        "DEST={0} ATERM_INSTALL_DIR={0} BIN_DIR={1} STORE_DIR={2}",
        folder(&wedged),
        q(s(&bin)),
        q(s(&store))
    ));
    let found = lib.call("find_bundle_cli", &[]);
    let record = lib.call("cat \"$ASKED\"", &[]);
    let driver = lib.call("toolset_driver", &[]);
    let setup = lib.call(
        "setup_aterm shell-setup \"$BIN_DIR\" \"$STORE_DIR\" \"$DEST\"",
        &[],
    );
    let secrets = lib.call("secrets_aterm \"$BIN_DIR\" \"$STORE_DIR\" \"$DEST\"", &[]);
    let managed = lib.call(
        format!(
            "PATH={}:$PATH find_managed_targo {} || printf %s \"$MANAGED_DECLINE\"",
            q(s(&on_path)),
            q(s(&checkout))
        ),
        &[],
    );
    let linked_driver = lib.call(format!("BIN_DIR={} toolset_driver", q(s(&linked))), &[]);
    let linked_setup = lib.call(
        format!("setup_aterm shell-setup {} \"$STORE_DIR\"", q(s(&linked))),
        &[],
    );
    let later = lib.call(
        format!(
            "DEST={} ATERM_INSTALL_DIR={} find_bundle_cli",
            folder(&other),
            q(s(&apps))
        ),
        &[],
    );
    lib.step(format!("mv -f {} {}", q(s(&renamed)), q(s(&wedged_exe))));
    let swapped = lib.call("find_bundle_cli", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[found].out, "", "{}", recs[found].err);
    assert_eq!(
        recs[record].out,
        format!("cut {wedged_id}"),
        "the record names the wedged binary by its file"
    );
    assert_eq!(
        recs[driver].out, "skip no installed `aterm` to drive it",
        "{}",
        recs[driver].err
    );
    assert_eq!(recs[setup].out, "", "{}", recs[setup].err);
    assert_eq!(recs[secrets].out, "", "{}", recs[secrets].err);
    assert_eq!(
        recs[managed].out,
        "no aterm-managed Trust store is in reach (no `aterm` to ask, no `targo` on PATH)",
        "{}",
        recs[managed].err
    );
    assert_eq!(
        recs[linked_driver].out,
        format!(
            "skip {}/aterm did not answer when this run asked it what it is (cut off twice at \
             60s), so it is not run with no deadline — run install.sh again in 10 minutes, or \
             after a restart",
            s(&linked)
        ),
        "a link to a wedged binary is not handed to an unbounded `pkg seed`"
    );
    assert_eq!(recs[linked_setup].out, "", "{}", recs[linked_setup].err);
    assert_eq!(
        recs[later].out,
        s(&answering),
        "an answering bundle is found after a binary cut off"
    );
    assert_eq!(
        recs[swapped].out,
        s(&wedged_exe),
        "a binary renamed into a wedged one's place is asked afresh: {}",
        recs[swapped].err
    );
    assert_eq!(
        read(&log),
        format!(
            "{w}\n{w}\n{o}\n{o}\n",
            w = s(&wedged_exe),
            o = s(&other_exe)
        ),
        "each wedged binary ran in its first ask's two detectors, and never again"
    );
    no_record_left(&scratch);
}

/// A binary whose first exec macOS's check holds past one detector, as it can while syspolicyd
/// is backed up, answers its first ask's second detector: the bundle a run swapped in is the one
/// install_cli links, and the store binary the source lane placed, first asked for its
/// `installed` line, the one setup_aterm finds by the link the lane put in BIN_DIR.
#[test]
fn a_binary_held_past_one_detector_is_asked_again_at_once() {
    let scratch = Scratch::new();
    let app = scratch.mkdir("held/dest/aterm.app");
    plist(&app, "com.aterm.aterm");
    let bundled = scratch.stand_in(
        "held/dest/aterm.app/Contents/MacOS/aterm",
        &held_once("bundle"),
    );
    let store = scratch.mkdir("held/store");
    let bin = scratch.mkdir("held/bin");
    let log = scratch.join("held/runs.log");
    let mut lib = record_batch(&scratch, &log, "held/parked");
    let found = lib.call(
        format!(
            "DEST={} find_bundle_cli",
            q(s(app.parent().expect("an app's folder")))
        ),
        &[],
    );
    let said = lib.call(
        kept(
            &source_lane(
                &q(s(&scratch.mkdir("held/source"))),
                &q(s(&store)),
                &q(s(&bin)),
                &q(s(&scratch.stand_in("held/built/aterm", &held_once("store")))),
            ),
            "install.sh: installed ",
        ),
        &[],
    );
    let setup = lib.call(
        format!("setup_aterm shell-setup {} {}", q(s(&bin)), q(s(&store))),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[found].out, s(&bundled), "{}", recs[found].err);
    assert_eq!(
        recs[said].out,
        format!(
            "install.sh: installed aterm 0.99.0 -> {}\nstatus 0",
            s(&store)
        ),
        "{}",
        recs[said].err
    );
    let linked = bin.join("aterm");
    assert_eq!(recs[setup].out, s(&linked), "{}", recs[setup].err);
    assert_eq!(
        read(&log),
        format!(
            "{b}\n{b}\n{p}\n{p}\n{l}\n{l}\n",
            b = s(&bundled),
            p = s(&store.join("aterm")),
            l = s(&linked)
        ),
        "each ran twice in its first ask, then once per question"
    );
    no_record_left(&scratch);
}

/// A binary that has answered in a run is past its first exec's check: a later ask of it that is
/// cut off (a `pkg help` held on a lock) is that ask's alone, and every later ask runs it.
#[test]
fn a_binary_that_answered_is_asked_again_after_a_later_cut_off() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("answered/store");
    let bin = scratch.mkdir("answered/bin");
    let log = scratch.join("answered/asked.log");
    let aterm = scratch.stand_in(
        "answered/store/aterm",
        &format!(
            "#!/bin/sh\necho \"$*\" >>\"${{STALL_LOG:?}}\"\ncase \"$1\" in\n\
             --version) echo \"aterm 0.99.0\" ;;\nsecrets) echo \"aterm secrets: the vault\" ;;\n\
             *)\n\tsleep {PARKED_SECS} &\n\techo \"$$ $! $PPID\" >\"${{STALL_PARKED:?}}\"\n\twait\n\
             \t;;\nesac\n"
        ),
    );
    let mut lib = record_batch(&scratch, &log, "answered/parked");
    let setup = |lib: &mut Lib| {
        lib.call(
            format!("setup_aterm shell-setup {} {}", q(s(&bin)), q(s(&store))),
            &[],
        )
    };
    let first = setup(&mut lib);
    let secrets = lib.call(
        format!("secrets_aterm {} {}", q(s(&bin)), q(s(&store))),
        &[],
    );
    let again = setup(&mut lib);
    let recs = lib.run(&scratch);
    assert_eq!(recs[first].out, "", "{}", recs[first].err);
    assert_eq!(
        recs[secrets].out,
        s(&aterm),
        "a cut-off `pkg help` is no verdict on the binary: {}",
        recs[secrets].err
    );
    assert_eq!(recs[again].out, "", "{}", recs[again].err);
    assert_eq!(
        read(&log),
        "--version\npkg help shell-setup\n--version\nsecrets --help\n--version\npkg help shell-setup\n",
        "every ask of a binary that answered runs it, the cut-off one once"
    );
    no_record_left(&scratch);
}

/// A line that reports placing a binary that did not say its version says so, and with no
/// installed aterm that runs `aterm pkg shell-setup` the shell setup says why: the binary the run
/// placed was taken as wedged, or none has the verb yet.
#[test]
fn the_shell_setup_says_why_no_aterm_ran_it() {
    let scratch = Scratch::new();
    let store = scratch.mkdir("note/store");
    let bin = scratch.mkdir("note/bin");
    let wedged = scratch.stand_in("note/store/aterm", &logged_staller("note"));
    let verbless = scratch.stand_in(
        "note/old/aterm",
        "#!/bin/sh\n[ \"$1\" = --version ] || exit 1\necho \"aterm 0.40.0\"\n",
    );
    let log = scratch.join("note/runs.log");
    let mut lib = record_batch(&scratch, &log, "note/parked");
    lib.step(format!(
        "BIN_DIR={} STORE_DIR={} ROOT= DEST= BIN_ATERM_EXPOSED=1",
        q(s(&bin)),
        q(s(&store))
    ));
    let said = lib.call(format!("placed_name {}", q(s(&wedged))), &[]);
    let setup = |lib: &mut Lib, placed: &Path| {
        lib.call(
            format!("PLACED_ATERM={} setup_completions_and_man", q(s(placed))),
            &[],
        )
    };
    let unanswered = setup(&mut lib, &wedged);
    let verbs = setup(&mut lib, &verbless);
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[said].out, "aterm (it did not say its version)",
        "{}",
        recs[said].err
    );
    assert!(
        recs[unanswered].err.starts_with(&format!(
            "install.sh: NOTE: {} did not answer when this run asked it what it is",
            s(&wedged)
        )),
        "{}",
        recs[unanswered].err
    );
    assert!(
        recs[verbs]
            .err
            .starts_with("install.sh: NOTE: no installed aterm has `aterm pkg shell-setup` yet"),
        "{}",
        recs[verbs].err
    );
    assert_eq!(
        read(&log),
        format!("{w}\n{w}\n", w = s(&wedged)),
        "the wedged binary ran in its first ask's two detectors only"
    );
    no_record_left(&scratch);
}

/// A previous generation's bundle is asked through its front door, aterm-cli, by the run's
/// record too: wedged, it runs in the first look's two detectors and in no later look.
#[test]
fn a_previous_generations_wedged_front_door_is_not_asked_again() {
    let scratch = Scratch::new();
    let app = scratch.mkdir("toolset/dest/aterm.app");
    plist(&app, "com.aterm.aterm");
    scratch.stand_in(
        "toolset/dest/aterm.app/Contents/MacOS/aterm",
        "#!/bin/sh\necho \"aterm-gui 0.62.0\"\n",
    );
    scratch.stand_in(
        "toolset/dest/aterm.app/Contents/MacOS/aterm-cli",
        &logged_staller("front"),
    );
    for sibling in ["aterm-ctl", "atpkg", "aterm-fleet", "aterm-drive"] {
        scratch.stand_in(
            &format!("toolset/dest/aterm.app/Contents/MacOS/{sibling}"),
            "#!/bin/sh\nexit 0\n",
        );
    }
    let log = scratch.join("toolset/runs.log");
    let mut lib = record_batch(&scratch, &log, "toolset/parked");
    lib.step(format!(
        "DEST={}",
        q(s(app.parent().expect("an app's folder")))
    ));
    let looks: Vec<(usize, usize)> = (0..2)
        .map(|_| {
            let found = lib.call("find_bundle_cli", &[]);
            (found, lib.call("grep -c . \"$STALL_LOG\"", &[]))
        })
        .collect();
    let recs = lib.run(&scratch);
    for (look, (found, runs)) in looks.into_iter().enumerate() {
        assert_eq!(recs[found].out, "", "look {look}: {}", recs[found].err);
        assert_eq!(
            recs[runs].out, "2",
            "look {look}: the wedged aterm-cli ran in the first look's two detectors only"
        );
    }
    no_record_left(&scratch);
}

/// find_bundle_cli asks each folder once, whatever names it: DEST and ATERM_INSTALL_DIR spelled
/// alike or with a trailing slash, a link to the folder, or /Applications and ~/Applications
/// themselves. It asks only on a Mac, so `uname` says Darwin here on every host.
#[test]
fn each_folder_a_bundle_may_be_in_is_asked_once() {
    let scratch = Scratch::new();
    let dest = scratch.mkdir("dirs/dest");
    let app = scratch.mkdir("dirs/dest/aterm.app");
    plist(&app, "com.aterm.aterm");
    scratch.stand_in(
        "dirs/dest/aterm.app/Contents/MacOS/aterm",
        "#!/bin/sh\necho \"$0\" >>\"${STUB_LOG:?}\"\necho \"other 1.0\"\n",
    );
    let link = scratch.join("dirs/link");
    symlink(&dest, &link);
    let log = scratch.join("dirs/asked.log");
    let home = s(&scratch.join("home")).to_string();
    let mut lib = Lib::new();
    lib.env("STUB_LOG", s(&log));
    lib.step("may_run() { [[ \"$1\" != /Applications/* ]]; }");
    lib.step("uname() { echo Darwin; }");
    let slashed = lib.call(
        format!("DEST={0} ATERM_INSTALL_DIR={0}/ bundle_dirs", q(s(&dest))),
        &[],
    );
    let linked = lib.call(
        format!(
            "DEST={} ATERM_INSTALL_DIR={} bundle_dirs",
            q(s(&link)),
            q(s(&dest))
        ),
        &[],
    );
    let standard = lib.call(
        "DEST=\"$HOME/Applications\" ATERM_INSTALL_DIR=/Applications bundle_dirs",
        &[],
    );
    let found = lib.call(
        format!(
            "DEST={0} ATERM_INSTALL_DIR={0} find_bundle_cli",
            q(s(&dest))
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[slashed].out,
        format!("{}\n/Applications\n{home}/Applications", s(&dest))
    );
    assert_eq!(
        recs[linked].out,
        format!("{}\n/Applications\n{home}/Applications", s(&link))
    );
    assert_eq!(
        recs[standard].out,
        format!("{home}/Applications\n/Applications")
    );
    assert_eq!(recs[found].out, "", "{}", recs[found].err);
    assert_eq!(
        read(&log),
        format!("{}/aterm.app/Contents/MacOS/aterm\n", s(&dest)),
        "a bundle in a folder named twice is asked once"
    );
}

/// The run's record is named once and made by its first note, mode 0600, so a run that asks no
/// binary (a dry run's pre-flight may ask none) makes none; and every EXIT trap a run sets
/// removes it: make_download_dir's, which replaces keep_asked's, and linux_verifier_ready's,
/// even when the verifier's folder cannot all be removed (set -e holds in a trap).
#[test]
fn the_runs_record_is_made_by_its_first_note_and_goes_with_each_exit_trap() {
    let scratch = Scratch::new();
    let answering = scratch.stand_in("trap/aterm", ATERM);
    let mut lib = Lib::new();
    lib.step("keep_asked");
    let unmade = lib.call(
        "[[ -n \"$ASKED\" && ! -e \"$ASKED\" && ! -L \"$ASKED\" ]]",
        &[],
    );
    let said = lib.call(
        format!("first_line_bounded {} --version", q(s(&answering))),
        &[],
    );
    let made = lib.call("ls -l \"$ASKED\" | cut -c1-10; cat \"$ASKED\"", &[]);
    let download = lib.call(
        "OLD= MNT= STAGE= STAGE_DIR= CONTAINER_KIND=zip; make_download_dir install_app_cleanup; \
         [[ -d \"$TMP\" ]]",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[unmade].rc, 0,
        "a named record is made by its first note"
    );
    assert_eq!(recs[said].out, "aterm 0.99.0", "{}", recs[said].err);
    assert_eq!(
        recs[made].out,
        format!("-rw-------\nran {}", identity(&answering)),
        "{}",
        recs[made].err
    );
    assert_eq!(recs[download].rc, 0, "{}", recs[download].err);
    let left: Vec<String> = tree(&scratch.join("tmp"))
        .into_iter()
        .filter(|entry| entry.starts_with("aterm-asked.") || entry.starts_with("aterm-install."))
        .collect();
    assert!(
        left.is_empty(),
        "make_download_dir's trap removes the download folder and the record: {left:?}"
    );

    let scratch = Scratch::new();
    let answering = scratch.stand_in("verifier/aterm", ATERM);
    let mut lib = Lib::new();
    lib.step("keep_asked");
    let said = lib.call(
        format!("first_line_bounded {} --version", q(s(&answering))),
        &[],
    );
    lib.step(format!(
        "release_asset_size() {{ echo 5; }}\n\
         download_release_asset() {{ mkdir \"${{4%/*}}/held\" && : >\"${{4%/*}}/held/entry\" && \
         chmod 500 \"${{4%/*}}/held\" && printf built >\"$4\"; }}\n\
         LINUX_VERIFIER_REPO=pinned-org/aterm LINUX_VERIFIER_TAG=v0.118.0 \
         LINUX_VERIFIER_SHA256_X86_64={}",
        "0".repeat(64)
    ));
    let ready = lib.call("linux_verifier_ready x86_64", &["LINUX_VERIFIER_DIR"]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[said].out, "aterm 0.99.0", "{}", recs[said].err);
    assert_eq!(
        recs[ready].rc, 2,
        "the stand-in is not the pinned build: {}",
        recs[ready].err
    );
    let held = Path::new(recs[ready].var("LINUX_VERIFIER_DIR")).join("held");
    let kept = held.join("entry").exists();
    crate::support::chmod(&held, 0o700);
    assert!(kept, "the trap's rm could not remove the verifier's folder");
    no_record_left(&scratch);
}

/// A whole run keeps one record of the binaries it asked while it runs (main names it before
/// the pre-flight), which a binary asked a second time sees, and none once the run ends.
#[cfg(target_os = "macos")]
#[test]
fn a_whole_run_keeps_its_record_of_what_it_asked_until_it_ends() {
    let scratch = Scratch::new();
    let home = scratch.home("record");
    let seen = scratch.join("record/seen.log");
    let exe = scratch.stand_in(
        "record/home/Applications/aterm.app/Contents/MacOS/aterm",
        "#!/bin/sh\ncat \"$TMPDIR\"/aterm-asked.* >>\"${STUB_SEEN:?}\" 2>/dev/null\n\
         echo \"aterm 0.99.0\"\n",
    );
    let run = crate::support::installer(
        &install_sh(),
        &home,
        &[("STUB_SEEN", s(&seen))],
        &["--no-app", "--no-toolchain", "--no-path"],
    );
    assert_eq!(run.code(), 0, "{}", run.all());
    assert!(
        run.stdout.contains("install.sh: linked aterm 0.99.0 -> "),
        "the bundle lane ran: {}",
        run.all()
    );
    let ran = format!("ran {}", identity(&exe));
    assert!(
        read(&seen).lines().any(|line| line == ran),
        "a later ask saw the record of the first ({ran}): {:?}",
        read(&seen)
    );
    let left: Vec<String> = tree(&home.tmp)
        .into_iter()
        .filter(|entry| entry.starts_with("aterm-asked."))
        .collect();
    assert!(left.is_empty(), "the run's record outlived it: {left:?}");
}

/// help_names reads a verb's whole usage, asked as setup_aterm asks it: a word on any line of a
/// usage that ends 0 names it, and one that fails names nothing.
#[test]
fn a_verbs_usage_names_a_word_only_when_it_answers() {
    let scratch = Scratch::new();
    let aterm = scratch.stand_in(
        "usage/aterm",
        "#!/bin/sh\ncase \"$3\" in\nshell-setup) printf 'usage: aterm pkg shell-setup\\n  --own-man\\n' ;;\n\
         gone) echo '--own-man'; exit 1 ;;\n*) echo 'usage: aterm pkg other' ;;\nesac\n",
    );
    let mut lib = Lib::new();
    let ask = |lib: &mut Lib, verb: &str| {
        lib.call(format!("help_names {} {verb} --own-man", q(s(&aterm))), &[])
    };
    let names = ask(&mut lib, "shell-setup");
    let other = ask(&mut lib, "self-uninstall");
    let failed = ask(&mut lib, "gone");
    let recs = lib.run(&scratch);
    assert_eq!(recs[names].rc, 0, "{}", recs[names].err);
    assert_eq!(recs[other].rc, 1, "{}", recs[other].err);
    assert_eq!(recs[failed].rc, 1, "{}", recs[failed].err);
}

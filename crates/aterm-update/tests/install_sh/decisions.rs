// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The installer's pure decisions: the manifest's identity fields, the official channel's signing-team pin, the app half's idempotence and its signature gate, the container election, which repository a checkout installs from, and the Linux candidate's identity line.

use crate::support::{Lib, Scratch, group_run, install_sh, q, root, s};

/// A required identity field occurs exactly once as a one-line string; an optional one at most once.
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

/// On the official repository the Developer-ID team is pinned in the script: a manifest that omits
/// or contradicts it is refused unless `ATERM_TEAM_ID` names a team.
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

/// An unpinned re-run against the installed elected version downloads nothing; a pin always reinstalls; a foreign bundle never short-circuits.
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

/// The idempotence gate blesses only a bundle the fresh install would have accepted, and tells a verifier that did not answer (124 codesign, 125 Gatekeeper) from one that declined (1).
#[test]
fn the_installed_bundle_gate_verifies_like_a_fresh_install() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
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
    lib.step("codesign() { sleep 60; }; VERIFY_WAIT_SECS=1");
    let hung = lib.call(format!("installed_bundle_verified {app} ''"), &[]);
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, row.2, "{row:?}");
    }
    assert_eq!(recs[hung].rc, 124);
}

/// A Ctrl-C while bounded waits ends the run with that SIGINT, as it would
/// without bounded, and the run's EXIT trap still runs. The group leader stands
/// for a terminal's foreground group, which a Ctrl-C reaches whole, and the
/// stand-in sends it one from inside bounded's job. Before, bounded's trap
/// exited 130, bash went on (wait-and-cooperative-exit), and the
/// installed-bundle gate read the 130 as a FAILED signature and reinstalled. The
/// group SIGKILL may find CMD already gone (it ended just before the Ctrl-C),
/// and kill's 1 then ended the trap under set -e, a status 1 read as a definite
/// answer. The files callers name in BOUNDED_TEMPS go with the run, those of
/// each caller a nested one extends. The script resets the run's INT trap, as
/// the subshells and command substitutions bounded mostly runs in do, so
/// bounded's own traps are what end it and what remove them (the temps row);
/// first_line_bounded sets the run's again (end_on_ctrl_c), whose trap removes
/// them as well, its own file among them (the line row). In every row CMD is
/// killed. A Ctrl-\ (the quit row) kills it too: bounded's subshell used to die
/// of the signal and leave CMD running, in a process group of its own the
/// terminal's signal never reaches. CMD's output goes nowhere the call reads, so
/// one that lives on is named (orphan) and killed once the call's minute is up;
/// the call ticks the batch while it waits, so the batch's own detector, a
/// minute too, never ends it first. A QUIT ends bounded's trap by exit 131,
/// never by a death of it, for which macOS writes a crash report (the cut row
/// runs the trap in the process the group leader waits on, which bash 3.2's
/// self-kill of it would end by it).
#[test]
fn a_ctrl_c_while_bounded_waits_ends_the_run() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "cat >\"$TMPDIR/ctrl-c.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
trap - INT
G=$PPID row=$1 go=\"$TMPDIR/go-$1\"
trap 'echo cleaned >\"$TMPDIR/cleaned-$row\"' EXIT
mkfifo \"$go\"
# bounded sleeps only once its traps are set. Its first sleep opens go to
# read, which the stand-in's open to write waits for, so the Ctrl-C reaches
# bounded's job while it runs its own code, never while it forks or waits.
sleep() {
\t[[ -e \"$go.read\" ]] || { : >\"$go.read\"; read -r _ <\"$go\" || :; }
\t/bin/sleep \"$@\"
}
# The read ends on a line, not on the close: on macOS a FIFO read begun after
# the only writer closed can wait for a writer that never comes, and so did
# the reader, until the sleep ended. The close is an exec of its own: bash 3.2
# saves the fd an `exec 9>&- cmd` closes to fd 10 and leaves it open in cmd.
# CMD names its pid (sh's parent) before the signal, for the call to see it end.
ctrl_c() {
\tsh -c 'echo \"$PPID\"' >\"$TMPDIR/cmd-$row\"
\texec 9>\"$go\"
\tkill -\"$SIG\" -- \"-$G\"
\techo >&9
\texec 9>&-
\texec /bin/sleep 300 >/dev/null 2>&1
}
codesign() { ctrl_c; }
around() {
\tlocal t
\tt=\"$(mktemp \"$TMPDIR/aterm-probe.around.XXXXXX\")\"
\tlocal BOUNDED_TEMPS=(\"$t\")
\t\"$@\"
}
inner() {
\tlocal t
\tt=\"$(mktemp \"$TMPDIR/aterm-probe.inner.XXXXXX\")\"
\tlocal BOUNDED_TEMPS=(\"${BOUNDED_TEMPS[@]}\" \"$t\")
\tbounded 60 ctrl_c
}
s=0 SIG=INT
case \"$1\" in
bounded) bounded 60 ctrl_c || s=$? ;;
quit) SIG=QUIT; bounded 60 ctrl_c || s=$? ;;
gate) installed_bundle_verified /nonexistent/aterm.app '' || s=$? ;;
gone) kill() { if [[ \"$1\" == -KILL ]]; then builtin kill \"$@\"; return 1; fi; builtin kill \"$@\"; }; bounded 60 ctrl_c || s=$? ;;
temps) around inner || s=$? ;;
line) around first_line_bounded ctrl_c || s=$? ;;
cut) set -m; /bin/sleep 300 >/dev/null 2>&1 & pid=$!; set +m; echo \"$pid\" >\"$TMPDIR/cmd-$row\"; bounded_cut QUIT 131 ;;
esac
echo \"went on ($s)\"
EOF",
    );
    let tags = ["bounded", "gate", "gone", "temps", "line", "quit", "cut"];
    let calls: Vec<usize> = tags
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "r=\"$({})\"; p=\"$(cat \"$TMPDIR/cmd-{tag}\" 2>/dev/null)\" i=0 k=nocmd; \
                     while [ -n \"$p\" ] && kill -0 \"$p\" 2>/dev/null && [ $i -lt 600 ]; do sleep 0.1; i=$((i + 1)); __tick; done; \
                     if [ -n \"$p\" ]; then if kill -0 \"$p\" 2>/dev/null; then k=orphan; kill -KILL \"$p\"; else k=killed; fi; fi; \
                     echo \"$r $(cat \"$TMPDIR/cleaned-{tag}\" 2>/dev/null), $(ls \"$TMPDIR\" | grep -c '^aterm-probe') left, $k\"",
                    group_run(&format!("\"$TMPDIR/ctrl-c.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    // A Ctrl-\ ends the run by exit 131 (run_cut), CMD killed all the same.
    for (tag, call) in tags.iter().zip(calls) {
        let want = if matches!(*tag, "quit" | "cut") {
            "exit 131 cleaned, 0 left, killed"
        } else {
            "signal 2 cleaned, 0 left, killed"
        };
        assert_eq!(recs[call].out, want, "{tag}: {}", recs[call].err);
    }
}

/// A Ctrl-C ends the run by it when the child it lands in catches it and exits,
/// as python3 does while it starts up. Before, bash went on
/// (wait-and-cooperative-exit): the tools probe read the exit as no Python 3.11,
/// and a Linux run skipped its app and seeded the toolset, and the binary's
/// verify exited 1 with Python's error. The run's trap also removes the temp
/// files the step it cut named in BOUNDED_TEMPS, which only bounded's own did.
#[test]
fn a_ctrl_c_a_child_catches_ends_the_run() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && printf '#!/bin/sh\\nexit 0\\n' >\"$TMPDIR/bin/openssl\" && \
         printf '#!/bin/sh\\ntrap \"exit 1\" INT\\nkill -s INT 0\\nexit 1\\n' >\"$TMPDIR/bin/python3\" && \
         chmod +x \"$TMPDIR/bin/openssl\" \"$TMPDIR/bin/python3\" && cat >\"$TMPDIR/caught.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
unset ATERM_INSTALL_VERIFIER
PATH=\"$TMPDIR/bin:$PATH\" LINUX_VERIFIER_SHA256_AARCH64=\"\" LINUX_VERIFIER=\"\" LINUX_ARCH=aarch64 row=$1
trap 'echo cleaned >\"$TMPDIR/cleaned-$row\"' EXIT
case \"$1\" in
probe) linux_verifier_ready aarch64 || echo \"went on: $LINUX_VERIFIER_WHY\" ;;
verify) linux_release_verify \"$TMPDIR\" v1.2.0 example/aterm aarch64 \"$TMPDIR/store\" || echo \"went on ($?)\" ;;
temps)
\tt=\"$(mktemp \"$TMPDIR/aterm-probe.t.XXXXXX\")\"
\tf() { local BOUNDED_TEMPS=(\"$t\"); python3 -c 1 || echo \"went on ($?)\"; }
\tf
\t;;
esac
EOF",
    );
    let tags = ["probe", "verify", "temps"];
    let calls: Vec<usize> = tags
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "echo \"$({}) $(cat \"$TMPDIR/cleaned-{tag}\" 2>/dev/null) $(ls \"$TMPDIR\" | grep -c '^aterm-probe')\"",
                    group_run(&format!("\"$TMPDIR/caught.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (tag, call) in tags.iter().zip(calls) {
        assert_eq!(
            recs[call].out, "signal 2 cleaned 0",
            "{tag}: {}",
            recs[call].err
        );
    }
}

/// A Ctrl-\ (SIGQUIT) the terminal sends the whole group ends the run once its
/// EXIT trap has run, by exit 131: bash 5 ignores the signal, so the run read
/// the child's death of it as an answer and went on, and bash 3.2 died of it
/// without running the trap. A second one while the run's trap removes the
/// step's files (the twice row: the rm stand-in sends it), as a key held down
/// repeats it, skipped the EXIT trap on bash 3.2. Nothing dies of a QUIT here,
/// a death by a core-dump signal, for which macOS writes a crash report: the
/// python3 stand-in catches its own and exits, and the rm stand-in starts with
/// it ignored (run_cut).
#[test]
fn a_ctrl_backslash_ends_the_run_after_its_cleanup() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && printf '#!/bin/sh\\nexit 0\\n' >\"$TMPDIR/bin/openssl\" && \
         printf '#!/bin/sh\\ntrap \"exit 1\" QUIT\\nkill -s \"$SIG\" 0\\nexit 1\\n' >\"$TMPDIR/bin/python3\" && \
         chmod +x \"$TMPDIR/bin/openssl\" \"$TMPDIR/bin/python3\" && cat >\"$TMPDIR/rm\" <<'EOF'
#!/bin/sh
[ \"$1 $2\" != \"-f --\" ] || [ -e \"$TMPDIR/again\" ] || { : >\"$TMPDIR/again\"; kill -s \"$SIG\" 0; }
exec /bin/rm \"$@\"
EOF
chmod +x \"$TMPDIR/rm\" && cat >\"$TMPDIR/quit.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
unset ATERM_INSTALL_VERIFIER
PATH=\"$TMPDIR/bin:$PATH\" LINUX_VERIFIER_SHA256_AARCH64=\"\" LINUX_VERIFIER=\"\" LINUX_ARCH=aarch64 row=$1 SIG=QUIT
export SIG
[ \"$1\" != twice ] || cp \"$TMPDIR/rm\" \"$TMPDIR/bin/rm\"
trap 'echo cleaned >\"$TMPDIR/cleaned-$row\"' EXIT
linux_verifier_ready aarch64 || echo \"went on: $LINUX_VERIFIER_WHY\"
EOF",
    );
    let calls: Vec<usize> = ["once", "twice"]
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "echo \"$({}) $(cat \"$TMPDIR/cleaned-{tag}\" 2>/dev/null)\"",
                    group_run(&format!("\"$TMPDIR/quit.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for call in calls {
        assert_eq!(recs[call].out, "exit 131 cleaned", "{}", recs[call].err);
    }
}

/// A Ctrl-C while install_linux_app's EXIT trap cleans up never cuts it short:
/// the fetched verifier's folder and the download both go, and a refusal keeps
/// its status. Before, a Ctrl-C in the first removal ended the run before the
/// second.
#[test]
fn a_ctrl_c_never_cuts_the_linux_cleanup_short() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && cat >\"$TMPDIR/bin/rm\" <<'EOF'
#!/bin/sh
[ -e \"$TMPDIR/cut\" ] || { : >\"$TMPDIR/cut\"; kill -s INT 0; }
exec /bin/rm \"$@\"
EOF
chmod +x \"$TMPDIR/bin/rm\" && cat >\"$TMPDIR/linux.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
TMP=\"$TMPDIR/tmp\" LINUX_VERIFIER_DIR=\"$TMPDIR/verifier\"
mkdir -p \"$TMP\" \"$LINUX_VERIFIER_DIR\" && : >\"$TMP/aterm\" && : >\"$LINUX_VERIFIER_DIR/verify\"
PATH=\"$TMPDIR/bin:$PATH\"
trap install_linux_app_cleanup EXIT
exit 1
EOF",
    );
    let call = lib.call(
        format!(
            "echo \"$({})\" $(ls \"$TMPDIR\" | grep -E '^(tmp|verifier|cut)$')",
            group_run("\"$TMPDIR/linux.bash\"")
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].out, "exit 1 cut", "{}", recs[call].err);
}

/// A Ctrl-C while --no-toolchain rewrites an aterm.toml in place leaves the file
/// whole, and the run ends by it once the rewrite is done, its copy gone. Before,
/// it could land after cat's redirection had emptied the file and before cat
/// refilled it, which left the file empty and its only copy in TMPDIR. A Ctrl-\
/// is the same, and so is a TERM or a HUP sent to the shell alone, which bash
/// answered at once, leaving the copy. The cat stand-in sends each. A TERM
/// the instant the copy exists (the made row, sent from probe_temp's last
/// step) ends the run with the copy gone and the file untouched, and one as
/// the copy is about to go (the removed row) with it gone and the file whole:
/// the hold began only once the copy was made and ended before its removal,
/// and bash's own handler left it. Once the rewrite is done, a TERM ends the
/// run at once again (the after row).
#[test]
fn a_ctrl_c_never_empties_the_config() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && cat >\"$TMPDIR/bin/cat\" <<'EOF'
#!/bin/sh
case \"$1:$SIG\" in
*:) ;;
*/aterm-config.*:INT | */aterm-config.*:QUIT) kill -s \"$SIG\" 0 ;;
*/aterm-config.*:*) kill -s \"$SIG\" \"$PPID\" ;;
esac
exec /bin/cat \"$@\"
EOF
cat >\"$TMPDIR/bin/later\" <<'EOF'
#!/bin/sh
kill -s TERM \"$PPID\"
i=0
while kill -0 \"$PPID\" 2>/dev/null && [ $i -lt 600 ]; do /bin/sleep 0.1; i=$((i + 1)); done
if kill -0 \"$PPID\" 2>/dev/null; then echo waited >\"$TMPDIR/later-$row\"; else echo ended >\"$TMPDIR/later-$row\"; fi
EOF
chmod +x \"$TMPDIR/bin/cat\" \"$TMPDIR/bin/later\" && cat >\"$TMPDIR/config.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
export SIG=$1 row=$1
case $1 in after | made | removed) SIG= ;; esac
if [ \"$1\" = made ]; then
\tprintf() {
\t\t[ \"$1\" != -v ] || [ -e \"$TMPDIR/sent-made\" ] || { : >\"$TMPDIR/sent-made\"; kill -s TERM $$; }
\t\tbuiltin printf \"$@\"
\t}
elif [ \"$1\" = removed ]; then
\trm() {
\t\tcase \"$*\" in */aterm-config.*) [ -e \"$TMPDIR/sent-removed\" ] || { : >\"$TMPDIR/sent-removed\"; kill -s TERM $$; } ;; esac
\t\tcommand rm \"$@\"
\t}
fi
cfg=\"$TMPDIR/aterm-$1.toml\"
printf '[packages]\\nkeep = 1\\n' >\"$cfg\"
PATH=\"$TMPDIR/bin:$PATH\"
trap 'echo cleaned >\"$TMPDIR/cleaned-$row\"' EXIT
persist_no_toolchain \"$cfg\" >/dev/null || echo \"went on ($?)\"
[ \"$1\" != after ] || later
EOF",
    );
    let rows = [
        ("INT", "signal 2"),
        ("QUIT", "exit 131"),
        ("HUP", "signal 1"),
        ("TERM", "signal 15"),
        ("after", "signal 15"),
        ("made", "signal 15"),
        ("removed", "signal 15"),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(sig, _)| {
            lib.call(
                format!(
                    "echo \"$({}) $(cat \"$TMPDIR/cleaned-{sig}\" 2>/dev/null)\" \
                     $(printf '[packages]\\nauto_install = false\\nkeep = 1\\n' | cmp -s - \"$TMPDIR/aterm-{sig}.toml\" && echo whole) \
                     $(printf '[packages]\\nkeep = 1\\n' | cmp -s - \"$TMPDIR/aterm-{sig}.toml\" && echo untouched) \
                     $(ls \"$TMPDIR\" | grep -c '^aterm-config') $(cat \"$TMPDIR/later-{sig}\" 2>/dev/null)",
                    group_run(&format!("\"$TMPDIR/config.bash\" {sig}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((sig, end), call) in rows.iter().zip(calls) {
        let later = if *sig == "after" { " ended" } else { "" };
        let file = if *sig == "made" { "untouched" } else { "whole" };
        assert_eq!(
            recs[call].out,
            format!("{end} cleaned {file} 0{later}"),
            "{sig}: {}",
            recs[call].err
        );
    }
}

/// A Ctrl-C at any instant of a check leaves none of its temporary files: the
/// mktemp stand-in sends it once mktemp has run. Before, mktemp made the file
/// and the step named it in BOUNDED_TEMPS only after, so one caught in between
/// stayed in TMPDIR. Asked for a name alone (-u), the stand-in makes the file
/// and removes it again after the Ctrl-C, as macOS's mktemp does (mkstemp, then
/// unlink), so one the Ctrl-C ended in between leaves a file nothing named,
/// whatever mktemp the host has. first_line_bounded runs in a command
/// substitution, whose subshell starts with the run's traps reset, so its file
/// stayed when the Ctrl-C landed outside bounded (the read row: in its head).
#[test]
fn a_ctrl_c_in_a_check_leaves_none_of_its_files() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" \"$TMPDIR/app/Contents/MacOS\" && cat >\"$TMPDIR/bin/mktemp\" <<'EOF'
#!/bin/sh
s=0 f=
if [ \"$1\" = -u ]; then shift; f=\"$(\"$REAL_MKTEMP\" \"$@\")\" || exit; else \"$REAL_MKTEMP\" \"$@\" || s=$?; fi
[ \"$ROW\" = read ] || [ -e \"$TMPDIR/sent-$ROW\" ] || { : >\"$TMPDIR/sent-$ROW\"; kill -s INT 0; }
[ -z \"$f\" ] || { rm -f \"$f\"; echo \"$f\"; }
exit $s
EOF
cat >\"$TMPDIR/bin/head\" <<'EOF'
#!/bin/sh
[ \"$ROW\" != read ] || kill -s INT 0
exec \"$REAL_HEAD\" \"$@\"
EOF
printf '#!/bin/sh\\necho aterm 1.0\\n' >\"$TMPDIR/aterm\" && \
chmod +x \"$TMPDIR/bin/mktemp\" \"$TMPDIR/bin/head\" \"$TMPDIR/aterm\" && cat >\"$TMPDIR/make.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
export ROW=$1 REAL_MKTEMP=\"$(command -v mktemp)\" REAL_HEAD=\"$(command -v head)\"
printf '[packages]\\nkeep = 1\\n' >\"$TMPDIR/cfg-$1.toml\"
PATH=\"$TMPDIR/bin:$PATH\"
trap 'echo cleaned >\"$TMPDIR/cleaned-$ROW\"' EXIT
case \"$1\" in
version) linux_candidate_answers_tag \"$TMPDIR/aterm\" v1.0 ;;
line | read) said=\"$(first_line_bounded \"$TMPDIR/aterm\" --version)\" ;;
showed) showed_it_ran \"$TMPDIR/aterm\" ;;
inventory) bundle_showed_it_ran \"$TMPDIR/app\" ;;
config) persist_no_toolchain \"$TMPDIR/cfg-$1.toml\" ;;
esac
echo \"went on ($?)\"
EOF",
    );
    // showed_it_ran asks a program only on macOS.
    let mut tags = vec!["version", "line", "read", "inventory", "config"];
    if cfg!(target_os = "macos") {
        tags.push("showed");
    }
    let calls: Vec<usize> = tags
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "echo \"$({}) $(cat \"$TMPDIR/cleaned-{tag}\" 2>/dev/null)\" \
                     $(ls \"$TMPDIR\" | grep -cE '^aterm-(probe|programs|config)[.]')",
                    group_run(&format!("\"$TMPDIR/make.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (tag, call) in tags.iter().zip(calls) {
        assert_eq!(
            recs[call].out, "signal 2 cleaned 0",
            "{tag}: {}",
            recs[call].err
        );
    }
}

/// A Ctrl-C in a check whose answer goes to a file ends the run with the
/// cleanup's output reaching the run's own: bounded opens the check's files in
/// its own subshell (BOUNDED_OUT, BOUNDED_ERR). Before, the check's call was
/// redirected in the run's shell, so the trap and the EXIT cleanup ran with
/// their output going into that file, and a NOTE saying how to detach a
/// mounted image was never shown. The stand-in program sends the Ctrl-C to
/// the run's group (bounded runs it in a group of its own).
#[test]
fn a_ctrl_c_in_a_check_leaves_the_cleanup_its_output() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "printf '#!/bin/sh\\nkill -s INT -- \"-$G\"\\necho aterm 1.0\\n' >\"$TMPDIR/said\" && chmod +x \"$TMPDIR/said\" && \
         cat >\"$TMPDIR/note.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
export G=$PPID
trap 'echo NOTE; echo ERRNOTE >&2' EXIT
case \"$1\" in
version) linux_candidate_answers_tag \"$TMPDIR/said\" v1.0 ;;
showed) showed_it_ran \"$TMPDIR/said\" ;;
esac
echo \"went on ($?)\"
EOF",
    );
    // showed_it_ran asks a program only on macOS.
    let mut tags = vec!["version"];
    if cfg!(target_os = "macos") {
        tags.push("showed");
    }
    let calls: Vec<usize> = tags
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "echo $({} 2>&1)",
                    group_run(&format!("\"$TMPDIR/note.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (tag, call) in tags.iter().zip(calls) {
        assert_eq!(
            recs[call].out, "NOTE ERRNOTE signal 2",
            "{tag}: {}",
            recs[call].err
        );
    }
}

/// A binary asked what it is answers with the first line it printed, and what
/// it writes to stderr goes nowhere: bounded opens both of first_line_bounded's
/// files in its own subshell (BOUNDED_OUT, BOUNDED_ERR). Off macOS this is the
/// one check of BOUNDED_ERR: showed_it_ran, the other caller that names an
/// error file, asks a program only there.
#[test]
fn a_binary_asked_what_it_is_answers_its_first_line_alone() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let asked = lib.call(
        "first_line_bounded /bin/sh -c 'echo said; echo noise >&2; echo later'",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[asked].rc, 0, "{}", recs[asked].err);
    assert_eq!(recs[asked].out, "said");
    assert_eq!(recs[asked].err, "");
}

/// One digest, two spellings: equality is case-blind, and an empty operand never matches.
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

/// The authenticated Linux candidate is the release it was elected as when the FIRST line of its `--version` says so: `aterm --version` prints the origin line, the build, which copy runs and the trust roots under the identity, and the lane compared that whole report, refusing every release.
#[test]
fn the_linux_candidate_is_judged_by_its_identity_line() {
    let scratch = Scratch::new();
    let report = |identity: &str| {
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{identity}' '{}' 'build: 1792175575' \
             'running: /home/u/.local/lib/aterm/bin/aterm' 'trusts: master=155e'\n",
            aterm_types::identity::ORIGIN_LINE
        )
    };
    let rows: Vec<(String, &str, i32, &str)> = vec![
        (report("aterm 0.105.0"), "v0.105.0", 0, "aterm 0.105.0"),
        (
            report("aterm 0.105.0+g1234abcd"),
            "v0.105.0",
            0,
            "aterm 0.105.0+g1234abcd",
        ),
        (
            "#!/bin/sh\necho 'aterm 0.105.0'\n".into(),
            "v0.105.0",
            0,
            "aterm 0.105.0",
        ),
        (report("aterm 0.104.0"), "v0.105.0", 1, "aterm 0.104.0"),
        (report("aterm 0.105.0"), "v0.10.0", 1, "aterm 0.105.0"),
        (
            report("aterm-gui 0.105.0"),
            "v0.105.0",
            1,
            "aterm-gui 0.105.0",
        ),
        (
            "#!/bin/sh\necho 'aterm 0.104.0'\necho 'aterm 0.105.0'\n".into(),
            "v0.105.0",
            1,
            "aterm 0.104.0",
        ),
        (
            "#!/bin/sh\necho 'aterm 0.105.0'\nexit 1\n".into(),
            "v0.105.0",
            2,
            "",
        ),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (body, tag, _, _))| {
            let exe = scratch.exe(&format!("candidate/{n}/aterm-linux"), body);
            lib.call(
                format!("linux_candidate_answers_tag {} {tag}", q(s(&exe))),
                &["LINUX_CANDIDATE_SAID"],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((body, tag, rc, said), i) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[i].rc, recs[i].var("LINUX_CANDIDATE_SAID")),
            (*rc, *said),
            "{tag}: {body}"
        );
    }
}

const SHA_DMG: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SHA_ZIP: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// The container election: the lean zip whenever the release ships one (the toolset deferred to first launch, or excluded), else the bare DMG a pre-lean release was cut for; half a pair or a malformed one refuses.
#[test]
fn the_container_election_matrix() {
    let scratch = Scratch::new();
    let (dmg, zip) = ("aterm-0.48.0.dmg", "aterm-0.48.0-mac.zip");
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

/// The lean default's deferral is a function of who owns this Mac's console
/// (docs/DESIGN-init-system-2026-10-20.md §4.2, "The SSH install"): the toolset waits for the
/// window only when the installing user is at the screen, and is seeded here for any other owner
/// (another login, loginwindow's root) or one that cannot be read; a root run never seeds
/// (`TOOLSET_AS_ROOT`).
#[test]
fn the_toolsets_deferral_is_a_function_of_the_console_owner() {
    let scratch = Scratch::new();
    let (dmg, zip) = ("aterm-0.48.0.dmg", "aterm-0.48.0-mac.zip");
    let vars = ["TOOLCHAIN_DEFERRED", "TOOLSET_NO_WINDOW", "TOOLSET_AS_ROOT"];
    /// (toolchain, zip, console owner, euid, SUDO_UID) -> (deferred, no window, as root)
    type Row = (
        (u8, &'static str, &'static str, &'static str, &'static str),
        (&'static str, &'static str, &'static str),
    );
    let rows: &[Row] = &[
        ((1, zip, "501", "501", ""), ("1", "0", "0")),
        ((1, zip, "502", "501", ""), ("0", "1", "0")),
        ((1, zip, "0", "501", ""), ("0", "1", "0")),
        ((1, zip, "", "501", ""), ("0", "1", "0")),
        ((1, zip, "502", "501", "502"), ("0", "1", "0")),
        ((1, zip, "501", "0", "501"), ("1", "0", "0")),
        ((1, zip, "502", "0", "501"), ("1", "0", "1")),
        ((1, zip, "", "0", "501"), ("1", "0", "1")),
        ((1, zip, "501", "0", ""), ("1", "0", "1")),
        ((1, zip, "0", "0", ""), ("1", "0", "0")),
        ((0, zip, "502", "501", ""), ("0", "0", "0")),
        ((1, "", "502", "501", ""), ("0", "0", "0")),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|((toolchain, zip, console, euid, sudo), _)| {
            let zip_sha = if zip.is_empty() { "" } else { SHA_ZIP };
            lib.call(
                format!(
                    "elect_container {toolchain} 0.48.0 {dmg} {SHA_DMG} '{zip}' '{zip_sha}' && \
                     toolset_deferral '{console}' '{euid}' '{sudo}'"
                ),
                &vars,
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((input, want), i) in rows.iter().zip(calls) {
        let rec = &recs[i];
        assert_eq!(rec.rc, 0, "{input:?}: {}", rec.err);
        assert_eq!(
            (rec.var(vars[0]), rec.var(vars[1]), rec.var(vars[2])),
            *want,
            "{input:?}"
        );
    }
}

/// The one step the plan and the install both run (`elect_install_lane`) decides over the console's real owner, or `ATERM_INSTALL_CONSOLE_UID`'s stand-in: the election, then the deferral, in that order (the election resets the deferral).
#[test]
fn the_install_lane_defers_by_the_console_owner_after_the_election() {
    let scratch = Scratch::new();
    let (dmg, zip) = ("aterm-0.48.0.dmg", "aterm-0.48.0-mac.zip");
    let vars = ["TOOLCHAIN_DEFERRED", "TOOLSET_NO_WINDOW"];
    let mut lib = Lib::new();
    let me = lib.call("id -u", &[]);
    let recs = lib.run(&scratch);
    let me = recs[me].out.trim().to_string();
    assert_ne!(me, "0", "the suite does not run as root");
    let other = (me.parse::<u32>().unwrap() + 1).to_string();
    let rows = [(me.clone(), ("1", "0")), (other, ("0", "1"))];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(console, _)| {
            lib.call(
                format!(
                    "ATERM_INSTALL_CONSOLE_UID={console} elect_install_lane 1 0.48.0 {dmg} \
                     {SHA_DMG} '{zip}' '{SHA_ZIP}'"
                ),
                &vars,
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((console, want), call) in rows.iter().zip(calls) {
        let rec = &recs[call];
        assert_eq!(rec.rc, 0, "{}", rec.err);
        assert_eq!(
            (rec.var(vars[0]), rec.var(vars[1])),
            *want,
            "console {console}, installer {me}"
        );
    }
}

/// The owner the deferral reads is `/dev/console`'s, as the kernel reports it.
#[cfg(target_os = "macos")]
#[test]
fn the_console_owner_is_dev_consoles_uid() {
    use std::os::unix::fs::MetadataExt as _;
    let scratch = Scratch::new();
    let owner = std::fs::metadata("/dev/console")
        .expect("macOS has a /dev/console")
        .uid();
    let mut lib = Lib::new();
    let call = lib.call("console_owner_uid", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
    assert_eq!(recs[call].out.trim(), owner.to_string());
}

/// The manifest parse reads the current keys; the retired Intel pair's keys are neither required nor read, so they can never masquerade as a zip.
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

/// A run from a checkout installs from the channel its manifest declares — the shipped binary's compiled-in channel — never from `repository` alone.
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

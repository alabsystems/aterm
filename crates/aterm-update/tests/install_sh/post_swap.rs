// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interruptible post-swap commands retain status and end their whole group.

use crate::support::{Lib, Scratch, bash, group_run, install_sh, q, s};

#[test]
fn post_swap_command_preserves_input_status_and_shell_state() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call(
        r#"trap ':' TERM; before="$(trap -p TERM)"; flags="$-";
           interruptible /bin/sh -c 'read line; echo "$line"; exit 23' <<<'input' >"$TMPDIR/out"; rc=$?;
           [ "$(cat "$TMPDIR/out")" = input ] && [ "$rc" = 23 ] &&
           [ "$(trap -p TERM)" = "$before" ] && [ "$flags" = "$-" ] && echo preserved"#,
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].out, "preserved", "{}", recs[call].err);
}

#[test]
fn post_swap_signal_ends_the_child_group_before_exit_cleanup() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    let child = scratch.stand_in("post-swap-child", r#"#!/bin/sh
[ -z "$TARGET" ] || [ "$2" = "$TARGET" ] || exit 0
# A descendant waits for cleanup, then proves it survived if its group was not killed.
/bin/sh -c 'trap "" HUP; : >"$CUT_DONE.ready"; while [ ! -f "$CUT_DONE" ]; do /bin/sleep 0.01; done; echo survived >"$CUT_DONE.survived"' &
while [ ! -f "$CUT_DONE.ready" ]; do /bin/sleep 0.01; done
kill -s "$SIG" "$PPID"
wait
"#);
    lib.step(format!(
        "ln -s {} \"$TMPDIR/child.sh\" && ln -s {} \"$TMPDIR/driver\"",
        q(s(&child)),
        q(s(&child))
    ));
    lib.step(
        r#"cat >"$TMPDIR/run.sh" <<'SH'
ATERM_INSTALL_LIBRARY_ONLY=1
. "$INSTALL_SH"
trap ': >"$CUT_DONE"; echo cleaned' EXIT
if [ "$MUTANT" = leader ]; then
    interruptible_cut() {
        trap '' INT TERM HUP QUIT
        kill -KILL "$interrupt_pid" 2>/dev/null || true
        wait "$interrupt_pid" 2>/dev/null || true
        run_cut "$1" "$2"
    }
fi
case "$TARGET" in
seed|update)
    toolset_driver() { echo "run $TMPDIR/driver"; }
    toolset_withheld() { :; }
    ATPKG_CONTENDED_EXIT=75 ATPKG_OFFLINE_EXIT=69
    install_toolchain
    ;;
*) interruptible /bin/sh "$TMPDIR/child.sh" ;;
esac
echo continued
SH"#,
    );
    let mut calls = Vec::new();
    let shells = [bash(), aterm_uds::host::need_tool("bash-5.3")];
    for (shell_index, shell) in shells.iter().enumerate() {
        for (signal, number) in [("TERM", "15"), ("HUP", "1")] {
            for (mutant, target) in [
                ("none", ""),
                ("leader", ""),
                ("none", "seed"),
                ("none", "update"),
            ] {
                let command = group_run("\"$TMPDIR/run.sh\"").replace(&q(s(&bash())), &q(s(shell)));
                let call = lib.row(
                    format!(
                        "export SIG={signal} MUTANT={mutant} TARGET='{target}' \
                         CUT_DONE=\"$TMPDIR/{shell_index}-{signal}-{mutant}-{target}\"; {command}; \
                         if [ \"$MUTANT\" = leader ]; then \
                         while [ ! -f \"$CUT_DONE.survived\" ]; do sleep 0.01; done; \
                         cat \"$CUT_DONE.survived\"; fi"
                    ),
                    &[],
                );
                calls.push((call, number, mutant));
            }
        }
    }
    let recs = lib.run(&scratch);
    for (call, signal, mutant) in calls {
        let out = &recs[call].out;
        assert!(out.contains("cleaned"), "{out}: {}", recs[call].err);
        assert!(out.contains(&format!("signal {signal}")), "{out}");
        assert!(!out.contains("continued"), "{out}");
        assert_eq!(out.contains("survived"), mutant == "leader", "{out}");
    }
}

#[test]
fn post_swap_phase_returns_quoted_state_after_normal_completion() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call(
        r#"STATE=before; phase() { STATE=$'a quote \' and\na newline'; echo output; };
           post_swap_step STATE phase;
           [ "$STATE" = $'a quote \' and\na newline' ] && echo returned"#,
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].out, "output\nreturned", "{}", recs[call].err);
}

#[test]
fn post_swap_phase_cancels_bounded_groups_inside_command_substitutions() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    let child = scratch.stand_in(
        "post-swap-nested-child",
        r#"#!/bin/sh
# Its enclosing bounded shell must kill it; forwarding TERM alone is insufficient.
trap '' TERM HUP
printf '%s\n' "$$" >"$CUT_DONE.pid"
kill -s "$SIG" "$CUT_PARENT"
while [ ! -f "$CUT_DONE" ]; do /bin/sleep 0.01; done
: >"$CUT_DONE.survived"
"#,
    );
    lib.step(format!("ln -s {} \"$TMPDIR/nested-child\"", q(s(&child))));
    lib.step(
        r#"cat >"$TMPDIR/nested-run" <<'SH'
ATERM_INSTALL_LIBRARY_ONLY=1
. "$INSTALL_SH"
export CUT_PARENT=$$
PHASE_STATE=before
trap ': >"$CUT_DONE"; echo cleaned' EXIT
if [ "$MUTANT" = kill ]; then
    interruptible_cut() {
        trap '' INT TERM HUP QUIT
        kill -KILL -- "-$interrupt_pid" 2>/dev/null || true
        wait "$interrupt_pid" 2>/dev/null || true
        run_cut "$1" "$2"
    }
fi
phase() {
    case "$ROUTE" in
    record) record_placed_command "$TMPDIR/nested-child" ;;
    substitution)
        account_processes() { bounded "$VERIFY_WAIT_SECS" "$TMPDIR/nested-child"; }
        answer="$(running_app_window 1)"
        ;;
    esac
    PHASE_STATE=after
}
post_swap_step PHASE_STATE phase
echo continued
SH"#,
    );
    let mut calls = Vec::new();
    let shells = [bash(), aterm_uds::host::need_tool("bash-5.3")];
    for (shell_index, shell) in shells.iter().enumerate() {
        for (signal, number) in [("TERM", "15"), ("HUP", "1")] {
            for route in ["record", "substitution"] {
                for mutant in ["none", "kill"] {
                    let command =
                        group_run("\"$TMPDIR/nested-run\"").replace(&q(s(&bash())), &q(s(shell)));
                    let call = lib.row(
                        format!(
                            "export SIG={signal} ROUTE={route} MUTANT={mutant} \
                             CUT_DONE=\"$TMPDIR/nested-{shell_index}-{signal}-{route}-{mutant}\"; \
                             {command}; read child <\"$CUT_DONE.pid\"; \
                             if [ \"$MUTANT\" = kill ]; then \
                               while [ ! -f \"$CUT_DONE.survived\" ]; do sleep 0.01; done; \
                               kill -KILL -- \"-$child\" 2>/dev/null || true; echo survived; \
                             else \
                               while kill -0 -- \"-$child\" 2>/dev/null; do \
                                 [ ! -f \"$CUT_DONE.survived\" ] || {{ echo survived; \
                                   kill -KILL -- \"-$child\" 2>/dev/null || true; break; }}; \
                                 sleep 0.01; \
                               done; \
                             fi"
                        ),
                        &[],
                    );
                    calls.push((call, number, mutant));
                }
            }
        }
    }
    let recs = lib.run(&scratch);
    for (call, signal, mutant) in calls {
        let out = &recs[call].out;
        assert!(out.contains("cleaned"), "{out}: {}", recs[call].err);
        assert!(out.contains(&format!("signal {signal}")), "{out}");
        assert!(!out.contains("continued"), "{out}");
        assert_eq!(out.contains("survived"), mutant == "kill", "{out}");
    }
}

#[test]
fn post_swap_phase_waits_for_a_signal_protected_write() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        r#"cat >"$TMPDIR/protected-write" <<'SH'
kill -s "$SIG" "$CUT_PARENT"
# This models the refill that persist_no_toolchain explicitly protects.
printf '%s\n' whole >"$CUT_DONE.whole"
SH
cat >"$TMPDIR/protected-run" <<'SH'
ATERM_INSTALL_LIBRARY_ONLY=1
. "$INSTALL_SH"
export CUT_PARENT=$$
trap 'echo cleaned' EXIT
phase() { (trap '' TERM HUP; exec /bin/sh "$TMPDIR/protected-write"); }
post_swap_step '' phase
echo continued
SH"#,
    );
    let mut calls = Vec::new();
    for (index, shell) in [bash(), aterm_uds::host::need_tool("bash-5.3")]
        .iter()
        .enumerate()
    {
        for (signal, number) in [("TERM", "15"), ("HUP", "1")] {
            let command =
                group_run("\"$TMPDIR/protected-run\"").replace(&q(s(&bash())), &q(s(shell)));
            let call = lib.row(
                format!(
                    "export SIG={signal} CUT_DONE=\"$TMPDIR/protected-{index}-{signal}\"; \
                     {command}; cat \"$CUT_DONE.whole\""
                ),
                &[],
            );
            calls.push((call, number));
        }
    }
    let recs = lib.run(&scratch);
    for (call, signal) in calls {
        let out = &recs[call].out;
        assert_eq!(
            out,
            &format!("cleaned\nsignal {signal}whole"),
            "{}",
            recs[call].err
        );
    }
}

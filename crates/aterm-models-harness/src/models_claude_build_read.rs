// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// How the harness learns which Claude Code build a target is and which build a session runs,
/// with nothing executed.
///
/// `case` is what lies at the managed twin: 0 no twin, 1 a twin naming a store build whose
/// record names its version, 2 the same with no record read (the build directory names it), 3 a
/// twin that names no store build, 4 a store build whose record names another version than its
/// directory, 5 a legacy index build whose version atpkg's stamp names, 6 a legacy index build
/// nothing names. `nat` is what `~/.local/bin/claude` resolves to: 0 nothing, 1 a file named by
/// its version, 2 a file of another name. `answer` is what a look read of the managed twin: 0
/// unread, 1 no managed copy, 2 a version. `word` is what a visit says of a session no read
/// target is ahead of: 1 `current`, 2 `wait:no-version`. `rec` is whether the session's record
/// names a readable version and `exe` whether its process's executable lies at a build's path;
/// `running` is whether the look knows the running build.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_build_read_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeBuildRead {
            const Buggy = 0;
            var case = 0; var nat = 0; var rec = 1; var exe = 0; var phase = 0;
            var answer = 0; var word = 0; var execs = 0; var running = 0; var kept = 0;
            action NextCase when (phase == 0 && 6 > case && kept == 0) { case = case + 1; }
            action NextNative when (phase == 0 && 2 > nat) { nat = nat + 1; }
            action DropRecord when (phase == 0 && rec == 1) { rec = 0; }
            action BuildPath when (phase == 0 && exe == 0) { exe = 1; }
            action Read when (phase == 0) {
                phase = 1;
                answer = if kept == 1 {answer} else {
                    if case == 0 {if Buggy == 4 {0} else {1}} else {
                        if case == 1 {2} else {
                            if case == 2 {if Buggy == 3 {0} else {2}} else {
                                if case == 5 {if Buggy == 10 {0} else {2}} else {
                                    if Buggy == 2 {1} else {0}
                                }
                            }
                        }
                    }
                };
                word = if Buggy == 8 {1} else {
                    if case == 3 || case == 4 || case == 6 || (Buggy == 9 && nat == 2) {2} else {1}
                };
                execs = if Buggy == 1 && case > 0 {1} else {0};
                running = if rec == 1 {1} else {
                    if exe == 1 {if Buggy == 5 {0} else {1}} else {if Buggy == 6 {1} else {0}}
                };
            }
            action Mend when (phase == 1 && (case == 3 || case == 4 || case == 6)) {
                case = 1; phase = 0;
                kept = if Buggy == 7 {1} else {0};
            }
            invariant NoExec: execs == 0;
            invariant FailedReadUnanswered: phase == 0 || 3 > case || case == 5 || answer == 0;
            invariant StoreNamesTheBuild: phase == 0 || case == 0 || case == 3 || case == 4 || case == 6 || answer == 2;
            invariant AbsentIsNoTarget: phase == 0 || case > 0 || answer == 1;
            invariant UnreadIsNotCurrent: phase == 0 || answer > 0 || word == 2;
            invariant ReadIsNoWait: phase == 0 || answer == 0 || word == 1;
            invariant RunningFromPath: phase == 0 || rec == 1 || exe == 0 || running == 1;
            invariant RunningNeverInvented: phase == 0 || rec == 1 || exe == 1 || running == 0;
        }
    }
}

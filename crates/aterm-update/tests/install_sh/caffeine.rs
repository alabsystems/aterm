// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm Caffeine's login job over an install of an aterm with no Caffeine.

use crate::support::{Lib, Scratch, read, tree};

#[test]
fn placing_an_aterm_without_caffeine_turns_its_login_job_off() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let agents = scratch.join("home/Library/LaunchAgents");
    let mut lib = Lib::new();
    // The placed aterm answers `help caffeine` with the status in $TMPDIR/verdict,
    // or dies by SIGKILL for `kill`; launchctl only says what it was asked.
    lib.step(
        "exe=\"$TMPDIR/apps/aterm.app/Contents/MacOS/aterm\"; mkdir -p \"${exe%/*}\" && \
         printf '#!/bin/sh\\necho \"$*\" >>\"$TMPDIR/probed\"\\nv=\"$(cat \"$TMPDIR/verdict\")\"\\n\
         [ \"$v\" != kill ] || kill -KILL $$\\nexit \"$v\"\\n' \
         >\"$exe\" && chmod 755 \"$exe\" && echo 2 >\"$TMPDIR/verdict\"\n\
         launchctl() { echo \"$*\" >>\"$TMPDIR/launchctl\"; }\n\
         job() { local label=\"$1\" a; shift; { printf '<?xml version=\"1.0\" encoding=\"UTF-8\"?>\\n<plist \
         version=\"1.0\"><dict><key>Label</key><string>%s</string><key>ProgramArguments</key><array>' \"$label\"; \
         for a in \"$@\"; do printf '<string>%s</string>' \"$a\"; done; printf '</array></dict></plist>\\n'; } \
         >\"$HOME/Library/LaunchAgents/$label.plist\"; }",
    );
    let uid = lib.call("id -u", &[]);
    let no_jobs = lib.call("retire_caffeine_startup \"$exe\"", &[]);
    lib.step(
        "mkdir -p \"$HOME/Library/LaunchAgents\" && \
         job com.example.custom \"$exe\" caffeine serve && \
         mv \"$HOME/Library/LaunchAgents/com.example.custom.plist\" \
         \"$HOME/Library/LaunchAgents/com.aterm.aterm.caffeine.dev.76543210.plist\"",
    );
    lib.step(
        "cp \"$HOME/Library/LaunchAgents/com.aterm.aterm.caffeine.dev.76543210.plist\" \
         \"$TMPDIR/mismatched.before\"",
    );
    let no_owned_jobs = lib.call("retire_caffeine_startup \"$exe\"", &[]);
    lib.step(
        "mkdir -p \"$HOME/Library/LaunchAgents\" && \
         job com.aterm.aterm.caffeine \"$exe\" caffeine serve && \
         job com.aterm.aterm.caffeine.dev.0123abcd \"$exe\" caffeine serve && \
         job com.aterm.aterm.caffeine.dev.xyz \"$exe\" caffeine serve && \
         job com.aterm.aterm.caffeine.dev.89abcdef /Applications/aterm.app/Contents/MacOS/aterm caffeine serve && \
         job com.aterm.aterm.caffeine.dev.fedcba98 \"$exe\" caffeine serve --x && \
         job com.aterm.aterm.caffeine.dev.01234567 \"$exe\" caffeine",
    );
    let root = "id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }";
    // Root over this account's home, with an aterm it would run, and over its
    // own home, with one it would not.
    let as_root = lib.call(
        format!(
            "( {root}; home_owner_uid() {{ command id -u; }}; \
             ls_entry() {{ command ls \"$@\" | awk '{{ $3 = 0; print }}'; }}; retire_caffeine_startup \"$exe\" )"
        ),
        &[],
    );
    let as_root_at_home = lib.call(
        format!("( {root}; home_owner_uid() {{ echo 0; }}; retire_caffeine_startup \"$exe\" )"),
        &[],
    );
    let probed_by_none = lib.call("cat \"$TMPDIR/probed\" 2>/dev/null || true", &[]);
    let serves = lib.call(
        "echo 0 >\"$TMPDIR/verdict\" && retire_caffeine_startup \"$exe\"",
        &[],
    );
    // bounded's cut-off; a run that could not start (126, 127); one a signal
    // ended, bounded's own Ctrl-C (130) and a SIGKILL (137) among them.
    let no_answers: Vec<usize> = ["124", "126", "127", "130", "kill"]
        .into_iter()
        .map(|verdict| {
            lib.call(
                format!("echo {verdict} >\"$TMPDIR/verdict\" && retire_caffeine_startup \"$exe\""),
                &[],
            )
        })
        .collect();
    let lacks = lib.call(
        "echo 2 >\"$TMPDIR/verdict\" && retire_caffeine_startup \"$exe\"",
        &[],
    );
    let recs = lib.run(&scratch);
    let kept = [no_jobs, no_owned_jobs, as_root, as_root_at_home, serves];
    for &i in kept.iter().chain(&no_answers).chain([&lacks]) {
        assert_eq!(recs[i].rc, 0, "call {i}: {}", recs[i].err);
    }
    for &i in kept.iter().chain(&no_answers) {
        assert_eq!(recs[i].out, "", "call {i} turned a job off");
    }
    assert_eq!(
        recs[probed_by_none].out, "",
        "no job, or root over a home not root's or an aterm it may not run: nothing is asked"
    );
    assert_eq!(
        read(&tmp.join("probed")),
        "help caffeine\n".repeat(2 + no_answers.len()),
        "asked once each, the same question serves_caffeine asks"
    );
    let lines: Vec<&str> = recs[lacks].out.lines().collect();
    let off = [
        "com.aterm.aterm.caffeine.dev.0123abcd",
        "com.aterm.aterm.caffeine",
    ];
    assert_eq!(lines.len(), off.len(), "one line per job: {lines:?}");
    for (line, label) in lines.iter().zip(off) {
        let plist = agents.join(format!("{label}.plist"));
        assert!(
            line.contains(&format!("({})", plist.display())),
            "{line:?} names {label}'s plist"
        );
    }
    let uid = recs[uid].out.trim();
    assert_eq!(
        read(&tmp.join("launchctl")),
        format!(
            "bootout gui/{uid}/com.aterm.aterm.caffeine.dev.0123abcd\n\
             bootout gui/{uid}/com.aterm.aterm.caffeine\n"
        )
    );
    assert_eq!(
        tree(&agents),
        [
            "com.aterm.aterm.caffeine.dev.01234567.plist",
            "com.aterm.aterm.caffeine.dev.76543210.plist",
            "com.aterm.aterm.caffeine.dev.89abcdef.plist",
            "com.aterm.aterm.caffeine.dev.fedcba98.plist",
            "com.aterm.aterm.caffeine.dev.xyz.plist",
        ],
        "a job of another shape, label or program stays"
    );
    assert_eq!(
        std::fs::read(agents.join("com.aterm.aterm.caffeine.dev.76543210.plist")).unwrap(),
        std::fs::read(tmp.join("mismatched.before")).unwrap(),
        "a recognized filename cannot authorize deleting a different job's plist"
    );
}

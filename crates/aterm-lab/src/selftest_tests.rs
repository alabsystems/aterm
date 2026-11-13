// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A member listing that was not read says why on its one line; an empty jail prints nothing.
#[test]
fn a_member_table_that_was_not_read_says_why() {
    let why = "jailpids: the process list could not be read: refused".to_string();
    assert_eq!(
        members_table(&Err(why.clone())),
        format!("  not read: {why}\n")
    );
    assert_eq!(members_table(&Ok(Vec::new())), "");
}

/// A file of the owner's home that the lab never handed the agent is a
/// stray: Muse Code 1.4.10's `session-names.db`, opened with HOME pointed
/// at the scratch.
#[test]
fn a_file_of_the_owners_home_outside_the_lab_is_a_stray() {
    let home = &["/Users//o".to_string()];
    let run = Run {
        run: "/Users//o/Developer/lab/runs/t".into(),
        ..Run::default()
    };
    let bin =
        "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";
    let allowed = lab_roots(&run, bin, home);
    assert_eq!(
        allowed,
        [
            "/Users//o/Developer/lab/runs/t",
            "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002",
        ]
    );
    let db = "/Users//o/Library/Application Support/Muse/session-name-authority/session-names.db";
    let lsof = format!(
        "p7\nfcwd\nn/private/tmp/at.x/proj\nftxt\nn{bin}\nf3\nn/Users//o/Developer/lab/runs/t/scratch/home/.config/muse/x\n\
         f4\nn{db}\nf5\nn/Users//ox/notes\nf6\nn/private/tmp/tbh-501-rt/muse/ms-1.sock.lease\nf7\nn{db}\n\
         f8\nn/Users//o/Developer/lab/runs/tt/x\n"
    );
    assert_eq!(
        owner_strays(&lsof, home, &allowed),
        ["/Users//o/Developer/lab/runs/tt/x", db]
    );
    let clean = format!("p7\nftxt\nn{bin}\nf3\nn/Users//o/Developer/lab/runs/t/scratch/x\n");
    assert!(
        owner_strays(&clean, home, &allowed).is_empty(),
        "the control"
    );
    let outside = lab_roots(&run, "/opt/lab/agy/bin/agy", home);
    assert_eq!(
        outside,
        ["/Users//o/Developer/lab/runs/t", "/opt/lab/agy/bin/agy"]
    );
    assert_eq!(
        owner_strays("p7\nfcwd\nn/Users//o\n", home, &outside),
        ["/Users//o"]
    );
    for own in [
        "/Users//o/bin/muse",
        "/Users//o/.local/bin/muse",
        "/Users//o/Library/Application Support/aterm/pkg/bin/muse",
    ] {
        let roots = lab_roots(&run, own, home);
        assert_eq!(roots, ["/Users//o/Developer/lab/runs/t", own], "{own}");
        let lsof = format!("p7\nftxt\nn{own}\nf4\nn{db}\nf5\nn/Users//o/bin/other\n");
        assert_eq!(
            owner_strays(&lsof, home, &roots),
            [db, "/Users//o/bin/other"],
            "{own}"
        );
    }
    let at_top = "/Users//o/Library/Application Support/aterm/pkg/store/bin/x";
    assert_eq!(
        lab_roots(&run, at_top, home),
        ["/Users//o/Developer/lab/runs/t", at_top]
    );
}

/// UNDER A SCRATCH `HOME` THE ACCOUNT'S HOME IS WATCHED TOO: a file of the
/// account's own home (the account database's) an agent holds is a stray
/// whatever `HOME` said, as is one of the scratch home's, and an agent in
/// the account's package store keeps its own build.
#[test]
fn under_a_scratch_home_a_file_of_the_account_home_is_a_stray() {
    let account = "/Users//o".to_string();
    let scratch = "/private/tmp/s/home".to_string();
    let homes = crate::paths::guarded(&account, &scratch);
    assert_eq!(homes, [account.clone(), scratch.clone()]);
    let run = Run {
        run: "/Users//o/Developer/lab/runs/t".into(),
        ..Run::default()
    };
    let bin = "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude";
    let roots = lab_roots(&run, bin, &homes);
    assert_eq!(
        roots,
        [
            "/Users//o/Developer/lab/runs/t",
            "/Users//o/Library/Application Support/aterm/pkg/store/claude/1",
        ]
    );
    let lsof = format!(
        "p7\nftxt\nn{bin}\nf3\nn/Users//o/.gemini/settings.json\nf4\nn/private/tmp/s/home/.zsh_history\n\
         f5\nn/Users//o/Developer/lab/runs/t/scratch/home/x\n"
    );
    assert_eq!(
        owner_strays(&lsof, &homes, &roots),
        [
            "/Users//o/.gemini/settings.json",
            "/private/tmp/s/home/.zsh_history"
        ]
    );
    let env_only = crate::paths::guarded("", &scratch);
    assert_eq!(
        owner_strays(&lsof, &env_only, &lab_roots(&run, bin, &env_only)),
        ["/private/tmp/s/home/.zsh_history"]
    );
    assert_eq!(
        lab_roots(&run, bin, &env_only),
        ["/Users//o/Developer/lab/runs/t", bin]
    );
}

/// EVERY WITNESS WATCHES EVERY HOME THE LAB GUARDS: [`Watched::of`] the
/// lab's facts under a scratch `HOME` (a fake account home handed in
/// through [`crate::paths::Paths::resolve_with`]) is the account's home
/// and the scratch one, so the Codex and agy witnesses look for
/// `.codex` and `.gemini` in both, the Muse Code witness looks for its
/// directories in both, and a file of either an agent holds is a stray.
#[test]
fn the_witnesses_watch_every_home_the_lab_guards() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-watched-")
        .tempdir()
        .unwrap();
    let real = std::fs::canonicalize(top.path()).unwrap();
    let (account, scratch) = (real.join("account"), real.join("scratch"));
    for d in [&account, &scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (a, s) = (account.display().to_string(), scratch.display().to_string());
    let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
    let w = Watched::of(&p);
    assert_eq!(w.homes(), [a.clone(), s.clone()]);
    for rel in [".codex", ".gemini"] {
        assert_eq!(w.dirs(rel), [format!("{a}/{rel}"), format!("{s}/{rel}")]);
    }
    let run = Run {
        run: format!("{a}/lab/runs/t").into(),
        ..Run::default()
    };
    let roots = lab_roots(&run, "", w.homes());
    let lsof = format!(
        "p7\nf3\nn{a}/.gemini/settings.json\nf4\nn{s}/.codex/auth.json\nf5\nn{a}/lab/runs/t/scratch/x\n"
    );
    assert_eq!(
        owner_strays(&lsof, w.homes(), &roots),
        [
            format!("{a}/.gemini/settings.json"),
            format!("{s}/.codex/auth.json")
        ]
    );
    let muse = format!("p7\nf3\nn{a}/.local/share/muse/db\nf4\nn{s}/.config/muse/auth.json\n");
    assert_eq!(
        crate::muse_proof::held_under(&muse, w.homes()),
        [
            format!("{a}/.local/share/muse/db"),
            format!("{s}/.config/muse/auth.json")
        ]
    );
    let own = Watched::of(&crate::paths::Paths::resolve_with(
        Some(account.clone()),
        Some(account.into()),
    ));
    assert_eq!(own.homes(), std::slice::from_ref(&a));
    assert_eq!(own.dirs(".codex"), [format!("{a}/.codex")]);
    assert_eq!(
        owner_strays(&lsof, own.homes(), &roots),
        [format!("{a}/.gemini/settings.json")]
    );
    assert_eq!(
        crate::muse_proof::held_under(&muse, own.homes()),
        [format!("{a}/.local/share/muse/db")]
    );
}

/// THE STRAYS CHECK FAILS ON A STRAY whatever else it saw, holds on a
/// complete report of every agent process with none, and is UNPROVEN with
/// no agent process or an incomplete report.
#[test]
fn the_strays_check_fails_on_a_stray_and_needs_a_complete_report() {
    let stray = vec!["/Users//o/Library/Application Support/Muse/x".to_string()];
    assert_eq!(strays_verdict(&stray, &[7], true), Verdict::Fail);
    assert_eq!(strays_verdict(&stray, &[], false), Verdict::Fail);
    assert_eq!(strays_verdict(&[], &[7], true), Verdict::Ok);
    assert_eq!(strays_verdict(&[], &[], true), Verdict::Unproven);
    assert_eq!(strays_verdict(&[], &[7], false), Verdict::Unproven);
}
/// Only a whole read of an agent's table proves an absence: a read that failed is
/// UNPROVEN and names nothing, a whole one names each path as `lsof -Fn` would, and a
/// newline in a name starts no line of its own.
#[test]
fn an_open_file_absence_requires_a_whole_read() {
    use std::path::PathBuf;
    let home = ["/Users//owner".to_string()];
    let read = |paths: &[&str]| {
        Ok(crate::procs::OpenPaths {
            paths: paths.iter().map(PathBuf::from).collect(),
            complete: true,
        })
    };
    let clean = open_files_report(7, read(&["/private/tmp/scratch/home/file"]));
    assert_eq!(clean.text, "p7\nn/private/tmp/scratch/home/file\n");
    assert_eq!(
        strays_verdict(&owner_strays(&clean.text, &home, &[]), &[7], clean.complete),
        Verdict::Ok
    );
    let held = open_files_report(7, read(&["/Users//owner/private.txt"]));
    assert_eq!(
        strays_verdict(&owner_strays(&held.text, &home, &[]), &[7], held.complete),
        Verdict::Fail
    );
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        let seen = open_files_report(7, Err(kind.into()));
        assert!(!seen.complete && seen.text.is_empty(), "{kind:?}");
        assert_eq!(strays_verdict(&[], &[7], seen.complete), Verdict::Unproven);
    }
    let forged = open_files_report(7, read(&["/private/tmp/x\nn/Users//owner/y"]));
    assert_eq!(forged.text.lines().count(), 2, "{:?}", forged.text);
    assert!(owner_strays(&forged.text, &home, &[]).is_empty());
}

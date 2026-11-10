// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A checkout completed elsewhere can settle an already admitted repository stage.

use super::*;
use aterm_machines::port::{Ran, Stat, System};

#[test]
fn a_parked_repository_hears_a_matching_checkout_completed_elsewhere() {
    for stopped in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let plan = selected(&[Setting::Repositories]);
        let revision = authorize_at(root, &plan).unwrap().unwrap();
        let mut host = native_repositories(true, true);
        let (sys, ctx) = host.native_repositories.as_ref().unwrap();
        let user = &ctx.users[0];
        sys.logged_in(user);
        let home = PathBuf::from(&user.home);
        let uid = user.uid;
        let gid = user.gid;
        let stat = |dir, len| Stat {
            uid,
            gid,
            mode: if dir { 0o700 } else { 0o600 },
            dir,
            symlink: false,
            len,
        };
        let inventory = home.join(".secrets/github-repositories.json");
        let bytes = br#"{"schema":2,"account":"tester","repositories":[{"id":1,"name":"arrived","owner":{"login":"tester"},"full_name":"tester/arrived","private":true,"clone_url":"https://github.com/tester/arrived.git","default_branch":"main"}]}"#;
        sys.file(inventory.to_str().unwrap(), bytes)
            .stat_of(inventory.to_str().unwrap(), stat(false, bytes.len() as u64));
        sys.executable(home.join(".local/bin/aterm").to_str().unwrap(), b"launcher");
        let checkout = home.join("arrived");
        let git_dir = checkout.join(".git");
        sys.stat_of(checkout.to_str().unwrap(), stat(true, 0));
        let personal = checkout.join("personal.txt");
        sys.file(personal.to_str().unwrap(), b"preserved local work");
        let read_checkout = checkout.clone();
        let read_git = git_dir.clone();
        sys.simulate(move |sys, command| {
            let committed_head = command.args.windows(4).any(|args| {
                args[0] == "rev-parse" && args[1] == "--verify"
                    && args[2] == "--quiet" && args[3] == "HEAD^{commit}"
            });
            let command = command.display();
            Some(if command.ends_with(" credential fill") {
                Ran::ok("protocol=https\nhost=github.com\nusername=tester\npassword=inert_repository_tools_token\n")
            } else if command.contains(" config --null --show-origin --get-regexp ") {
                Ran::ok("")
            } else if command.ends_with(" rev-parse --is-inside-work-tree --show-prefix --show-toplevel") {
                if sys.stat(&read_git).is_ok() {
                    Ran::ok(&format!("true\n\n{}\n", read_checkout.display()))
                } else {
                    Ran::exit(128, "not a Git repository")
                }
            } else if command.ends_with(" config --get-all remote.origin.url") {
                Ran::ok("https://github.com/tester/arrived.git\n")
            } else if committed_head {
                assert!(sys.stat(&read_git).is_ok(), "only the completed checkout has a HEAD");
                Ran::ok("1234567890abcdef1234567890abcdef12345678\n")
            } else {
                panic!("unexpected repository effect: {command}")
            })
        });
        let (report, next) =
            execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
                .unwrap()
                .unwrap();
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Park);
        let saved = progress(root).unwrap().unwrap();
        assert_eq!(saved.done & Stage::Repositories.bit(), 0);
        assert!(!saved.completed && !saved.stopped);
        assert_eq!(
            pending_inputs::repositories_revision(root, &observation_at(root).unwrap().records),
            Some(revision)
        );
        let mark = |host: &Fake| {
            let (sys, _) = host.native_repositories.as_ref().unwrap();
            initial_setup_park_mark_with(Some(root), None, 1_800_000_600, || {
                Some("unchanged attached network".into())
            })
            .with_repository_inputs(|| aterm_machines::init_github::repository_input_mark(sys).ok())
        };
        let parked = mark(&host);
        assert_eq!(parked.next_due(), None);
        assert!(!mark(&host).facts_changed_since(&parked));
        if stopped {
            request_stop_at(root, revision).unwrap();
            assert_eq!(
                pending_inputs::repositories_revision(root, &observation_at(root).unwrap().records),
                None
            );
        }
        // Another admitted runner or the person finishes a matching checkout in the same
        // directory. No vault, private inventory, credential, helper, Git or route changes.
        let (sys, _) = host.native_repositories.as_ref().unwrap();
        sys.stat_of(git_dir.to_str().unwrap(), stat(true, 0));
        let current = host.current_repositories(None).unwrap();
        assert!(rows_ready(&current), "{current:?}");
        assert!(
            current
                .iter()
                .any(|row| row.detail.contains("1 matching worktrees present"))
        );
        host.calls.clear();
        let resumed =
            execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
                .unwrap();
        if stopped {
            assert!(resumed.is_none());
            assert!(host.calls.is_empty());
            assert!(!mark(&host).facts_changed_since(&parked));
        } else {
            let (report, next) = resumed.unwrap();
            assert!(rows_ready(&report.rows));
            assert_eq!(next, SetupNext::Done);
            assert!(finished_at(root).unwrap());
            assert_eq!(
                pending_inputs::repositories_revision(root, &observation_at(root).unwrap().records),
                None
            );
            // Restore only the parked run's completion record to test its useful native fact.
            save_progress(root, saved).unwrap();
            assert!(
                mark(&host).facts_changed_since(&parked),
                "the parked repository must hear an externally completed matching checkout"
            );
        }
        let (sys, _) = host.native_repositories.as_ref().unwrap();
        assert_eq!(sys.read_file(&personal).unwrap(), b"preserved local work");
        assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
            && !call.contains(" agh GET ")
            && !call.contains("credential approve")));
    }
}

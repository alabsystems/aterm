// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A clone the setup publishes runs the hook installer it carries, once, and says what came of
//! it; a clone found in place, an existing worktree and a clone with no installer run nothing,
//! and no outcome of an installer fails the clone.

use super::*;

/// What trust's installer prints first when it installs.
const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";

/// The fake clone of github.com/alice/new, carrying an installer when `carries`, and the run
/// of it [`install_hooks`] makes once it is published.
fn clone_carrying(carries: bool) -> (Fake, User, Repo, String, String) {
    let (sys, who, _) = fixture();
    let (repo, staged, clone_key) = script_clone(&sys, &who, false);
    if carries {
        sys.on_stat(&clone_key, &staged.join("scripts"), directory(who.uid));
        sys.on_write(
            &clone_key,
            &staged.join(HOOK_INSTALLER),
            b"#!/usr/bin/env bash\n",
        );
    }
    let published = Path::new(&who.home).join("new");
    let installer = installer_command(&sys, &who, &published).display();
    (sys, who, repo, clone_key, installer)
}

#[test]
fn a_required_dependency_failure_does_not_suppress_the_new_parents_advisory_installer() {
    let (sys, who, mut repo, _, installer) = clone_carrying(true);
    repo.required_submodules = vec!["vendor/library".into()];
    let parent = Path::new(&who.home).join("new");
    script_existing(&sys, &who, &parent, "new");
    crate::init_github::dependencies::tests::declarations(&sys, &who, &parent, &["vendor/library"]);
    crate::init_github::dependencies::tests::download(&sys, &who, &parent, "vendor/library", 1);
    sys.on(&installer, Ran::ok(INSTALLED));
    sys.simulate(|_, command| {
        command
            .display()
            .contains(" clone --no-checkout ")
            .then(|| Ran::exit(128, "synthetic unavailable dependency"))
    });
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
    assert_eq!(cloned.failed.len(), 1, "{:?}", cloned.failed);
    assert!(sys.stat(&parent).is_ok());
    assert!(sys.stat(&parent.join("vendor/library")).is_err());
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| **call == installer)
            .count(),
        1
    );
    assert!(cloned.hooks.iter().any(|line| line.contains(INSTALLED)));
}

/// THE SETUP STEP: `clone_all` publishes the fixture repository and then runs its own
/// `scripts/install-git-hooks.sh` once, in the published clone, as its person, after the
/// publish; what it printed is the clone's outcome line, and a failed or timed-out installer is
/// a line too, never a failed clone. CONTROLS: a guarded pass whose vault snapshot stopped
/// being current after the publish names the installer it did not run; a clone that carries no
/// installer runs none; and one another clone published first (found in place, as it
/// publishes) never runs the installer, though the worktree in its place carries one.
#[test]
fn a_clone_the_setup_published_runs_its_own_hook_installer_once() {
    for (reply, outcome) in [
        (
            Some(Ran::ok(&format!("{INSTALLED}\n  ssh keepalive: set\n"))),
            format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED}"),
        ),
        (
            Some(Ran::exit(
                1,
                "ERROR: core.hooksPath is already set to 'elsewhere'.\n       Refusing\n",
            )),
            format!(
                "~/new: its {HOOK_INSTALLER} failed (exit 1: ERROR: core.hooksPath is already \
                 set to 'elsewhere'.); the clone stands"
            ),
        ),
        (
            None,
            format!(
                "~/new: its {HOOK_INSTALLER} did not run to the end (no answer within 300s); \
                 the clone stands"
            ),
        ),
    ] {
        let (sys, who, repo, _, installer) = clone_carrying(true);
        match reply {
            Some(ran) => {
                sys.on(&installer, ran);
            }
            None => {
                sys.time_out(&installer);
            }
        }
        let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
        assert_eq!(cloned.hooks, std::slice::from_ref(&outcome));
        assert!(cloned.failed.is_empty(), "{:?}", cloned.failed);
        assert_eq!(cloned.cloned.len(), 1, "the clone stands: {outcome}");
        let calls = sys.calls();
        let ran: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| **call == installer)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(ran.len(), 1, "run once: {calls:?}");
        let published = calls
            .iter()
            .position(|call| call.starts_with("rename-noreplace "))
            .expect("published");
        assert!(published < ran[0], "after the publish: {calls:?}");
        assert!(
            installer.starts_with(&format!(
                "cd {}/new && alice$ ",
                Path::new(&who.home).display()
            )) && installer.ends_with(&format!(" {BASH} {HOOK_INSTALLER}")),
            "in the clone, as its person: {installer}"
        );
    }

    // A guarded pass whose vault snapshot stopped being current once the clone was published.
    let (sys, who, repo, _, installer) = clone_carrying(true);
    sys.on(&installer, Ran::ok(INSTALLED));
    let current = || {
        if sys
            .calls()
            .iter()
            .any(|call| call.starts_with("rename-noreplace "))
        {
            Err("the selected secrets vault changed".to_owned())
        } else {
            Ok(())
        }
    };
    let guard = CloneGuard::new(&current);
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), Some(&guard));
    assert_eq!(
        cloned.hooks,
        [format!(
            "~/new: its {HOOK_INSTALLER} not run: the selected secrets vault changed"
        )]
    );
    assert!(!sys.calls().contains(&installer), "{:?}", sys.calls());

    // Its private staging could not be removed once the clone moved out of it: the clone is
    // the setup's all the same, so its installer runs, and the row says what was left behind.
    let (sys, who, repo, _, installer) = clone_carrying(true);
    sys.on(&installer, Ran::ok(INSTALLED));
    sys.scratch_close_fails(
        &Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0),
        Some(io::ErrorKind::PermissionDenied),
    );
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
    assert_eq!(cloned.cloned.len(), 1);
    assert_eq!(
        cloned.failed,
        [
            "~/new: repository published, but its private empty staging directory could not be \
          removed"
        ]
    );
    assert_eq!(
        cloned.hooks,
        [format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED}")]
    );

    let (sys, who, repo, _, installer) = clone_carrying(false);
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
    assert_eq!((cloned.cloned.len(), cloned.hooks.len()), (1, 0));
    assert!(!sys.calls().contains(&installer), "{:?}", sys.calls());

    let (sys, who, repo, clone_key, installer) = clone_carrying(true);
    let destination = Path::new(&who.home).join("new");
    script_existing(&sys, &who, &destination, "new");
    sys.on_stat(&clone_key, &destination, directory(who.uid));
    // The worktree in place carries an installer the setup would run in a clone it made.
    sys.on_stat(&clone_key, &destination.join("scripts"), directory(who.uid));
    sys.on_write(&clone_key, &destination.join(HOOK_INSTALLER), b"racer's\n");
    sys.on(&installer, Ran::ok(INSTALLED));
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
    assert_eq!(cloned.cloned.len(), 1, "found in place: done");
    assert!(cloned.hooks.is_empty(), "{:?}", cloned.hooks);
    assert!(
        !sys.calls().contains(&installer),
        "a clone it did not make ran its installer: {:?}",
        sys.calls()
    );
}

/// A real repository carrying an installer: [`install_hooks`] runs it with real bash and git,
/// in the clone, as this account, and the installer's own `git config --local` lands in the
/// clone; a failing installer is a line that names its exit and first error; a link in the
/// installer's place, and a clone without one, run nothing.
#[test]
fn the_installer_a_real_clone_carries_runs_in_it_and_says_what_it_did() {
    crate::items::github::need_clt_git();
    let sys = Live::new();
    let mut home = Scratch::new(&sys, "github-hook-installer").unwrap();
    let mut who = user("me", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let marker = home.path().join("link-target-ran");
    let repo = |name: &str, installer: Option<&str>| {
        let clone = home.path().join(name);
        std::fs::create_dir_all(clone.join("scripts")).unwrap();
        let ran = sys
            .run(
                &git(&sys, &who, Some(&clone))
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args(["init", "--quiet", "--template="]),
            )
            .unwrap();
        assert!(ran.success(), "git init: {}", ran.describe());
        if let Some(body) = installer {
            std::fs::write(clone.join(HOOK_INSTALLER), body).unwrap();
        }
        clone
    };

    let clone = repo(
        "fixture",
        Some(
            "#!/usr/bin/env bash\nset -euo pipefail\ncd \"$(dirname \"$0\")/..\"\n\
             git config --local core.hooksPath scripts/hooks\n\
             echo \"Fixture git hooks: installed (core.hooksPath = $(git config --local \
             core.hooksPath)) in $(basename \"$(pwd -P)\")\"\necho second line\n",
        ),
    );
    let outcome = install_hooks(&sys, &who, "fixture", None).expect("it carries one");
    assert_eq!(
        outcome,
        format!(
            "~/fixture: ran its {HOOK_INSTALLER}: Fixture git hooks: installed \
             (core.hooksPath = scripts/hooks) in fixture"
        )
    );
    assert_eq!(
        std::fs::read_to_string(clone.join(".git/config"))
            .unwrap()
            .matches("hooksPath = scripts/hooks")
            .count(),
        1,
        "the installer's own config landed in the clone"
    );

    repo(
        "refuses",
        Some("#!/usr/bin/env bash\necho 'ERROR: refused here' >&2\necho more >&2\nexit 3\n"),
    );
    assert_eq!(
        install_hooks(&sys, &who, "refuses", None).as_deref(),
        Some(
            format!(
                "~/refuses: its {HOOK_INSTALLER} failed (exit 3: ERROR: refused here); the \
                 clone stands"
            )
            .as_str()
        )
    );

    let linked = repo("linked", None);
    let target = home.path().join("elsewhere.sh");
    std::fs::write(&target, format!("touch {}\n", marker.display())).unwrap();
    std::os::unix::fs::symlink(&target, linked.join(HOOK_INSTALLER)).unwrap();
    assert_eq!(
        install_hooks(&sys, &who, "linked", None).as_deref(),
        Some(format!("~/linked: its {HOOK_INSTALLER} is reached through a link; not run").as_str())
    );
    // A tracked `scripts` link, which git checks out as a link: the installer path reads as a
    // regular file through it, and bash would run the file outside the clone.
    let shared = home.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("install-git-hooks.sh"),
        format!("touch {}\n", marker.display()),
    )
    .unwrap();
    let through = repo("through", None);
    std::fs::remove_dir(through.join("scripts")).unwrap();
    std::os::unix::fs::symlink("../shared", through.join("scripts")).unwrap();
    assert!(
        std::fs::metadata(through.join(HOOK_INSTALLER)).is_ok_and(|meta| meta.is_file()),
        "the path reads as a regular file through the link"
    );
    assert_eq!(
        install_hooks(&sys, &who, "through", None).as_deref(),
        Some(
            format!("~/through: its {HOOK_INSTALLER} is reached through a link; not run").as_str()
        )
    );
    assert!(!marker.exists(), "a linked installer ran");

    repo("bare", None);
    assert_eq!(install_hooks(&sys, &who, "bare", None), None);
    // A file named `scripts`: nothing is under it, so the clone carries no installer.
    let plain = repo("plain", None);
    std::fs::remove_dir(plain.join("scripts")).unwrap();
    std::fs::write(plain.join("scripts"), b"a file, not a folder\n").unwrap();
    assert_eq!(install_hooks(&sys, &who, "plain", None), None);
    home.close().unwrap();
}

/// `aterm init`'s profile stage says it too: the row its read-back reports ends with what the
/// new clone's installer printed, and a failing installer leaves the stage changed.
#[test]
fn the_profile_stage_row_says_what_a_new_clones_hook_installer_did() {
    for (reply, says) in [
        (
            Ran::ok(&format!("{INSTALLED}\n")),
            format!("; ~/new: ran its {HOOK_INSTALLER}: {INSTALLED}"),
        ),
        (
            Ran::exit(1, "ERROR: refused\n"),
            format!(
                "; ~/new: its {HOOK_INSTALLER} failed (exit 1: ERROR: refused); the clone stands"
            ),
        ),
    ] {
        let (sys, who, repo, _, installer) = clone_carrying(true);
        let config = config();
        sys.on(&installer, reply);
        script_existing(
            &sys,
            &who,
            &Path::new(&who.home).join(&repo.name),
            &repo.name,
        );
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("new", 1, true)], None),
        );
        let row = engine::run(
            &[item(&who, &config, Vec::new())],
            Mode::Apply,
            &sys,
            &ctx(std::slice::from_ref(&who)),
            &mut NoJournal,
        )
        .remove(0);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        let now = row.to.unwrap_or_default();
        assert!(now.ends_with(&says), "{now}");
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| **call == installer)
                .count(),
            1
        );
    }
}

/// The stage's guards meet the installers: a guard that stops the rest once a clone it made ran
/// its installer fails the row with that outcome after why, and a switch turned off before the
/// read-back answers not applicable with it, so the one line about the installer is never lost.
/// NEGATIVE CONTROLS: a guard checked after the clones with `?`, or the switch read before the
/// outcomes are taken, drops the line from that answer.
#[test]
fn a_guard_or_switch_after_the_clones_still_says_what_their_installers_did() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let says = format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED}");
    for stop in [true, false] {
        let (sys, who, repo, _, installer) = clone_carrying(true);
        let config = config();
        sys.on(&installer, Ran::ok(&format!("{INSTALLED}\n")));
        script_existing(
            &sys,
            &who,
            &Path::new(&who.home).join(&repo.name),
            &repo.name,
        );
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("new", 1, true)], None),
        );
        let ran = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&ran);
        sys.simulate(move |_, command| {
            if command.display() == installer {
                seen.store(true, Ordering::SeqCst);
            }
            None
        });
        let item = item(&who, &config, Vec::new());
        let context = ctx(std::slice::from_ref(&who));
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        let authorized = || {
            if stop && ran.load(Ordering::SeqCst) {
                Err("late Stop".to_owned())
            } else {
                Ok(())
            }
        };
        let applied = item.apply_guarded(&sys, &context, &authorized);
        assert!(ran.load(Ordering::SeqCst), "the installer ran");
        if stop {
            let why = applied.unwrap_err();
            assert_eq!(why, format!("late Stop; {says}"));
            continue;
        }
        applied.unwrap();
        sys.write_user_file(
            &who,
            &Path::new(&who.home).join(".config/aterm/init.toml"),
            b"[github]\nenabled = false\n",
            0o600,
        )
        .unwrap();
        match item.probe(&sys, &context) {
            Probe::NotApplicable { why } => assert_eq!(
                why,
                format!("repository setup is disabled in init.toml; {says}")
            ),
            other => panic!("{other:?}"),
        }
    }
}

/// Whatever the read-back answers, its line ends with every installer outcome, so a read-back
/// that cannot answer, waits or blocks still says what the installers did; five or more are
/// all named, never `and N more`.
#[test]
fn every_read_back_answer_carries_every_installer_outcome() {
    let hooks: Vec<String> = (0..6).map(|n| format!("~/r{n}: ran its x")).collect();
    let all = hooks.join("; ");
    let why = |probe: Probe| match probe {
        Probe::Ok { now: line }
        | Probe::Change { from: line, .. }
        | Probe::Person { how: line, .. }
        | Probe::Blocked { why: line }
        | Probe::Waits { why: line, .. }
        | Probe::NotApplicable { why: line }
        | Probe::Deferred { why: line }
        | Probe::Unusable { why: line } => line,
    };
    for probe in [
        Probe::Ok { now: "read".into() },
        Probe::Change {
            from: "read".into(),
            to: "wanted".into(),
        },
        Probe::Person {
            step: "step".into(),
            how: "read".into(),
        },
        Probe::Blocked { why: "read".into() },
        Probe::Waits {
            on: "item".into(),
            why: "read".into(),
        },
        Probe::NotApplicable { why: "read".into() },
        Probe::Deferred { why: "read".into() },
        Probe::Unusable { why: "read".into() },
    ] {
        assert_eq!(
            why(with_hooks(probe.clone(), &hooks)),
            format!("read; {all}")
        );
        assert_eq!(with_hooks(probe.clone(), &[]), probe);
    }
}

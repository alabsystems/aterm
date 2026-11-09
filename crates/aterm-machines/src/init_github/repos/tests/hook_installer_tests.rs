// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A clone the setup publishes runs the hook installer it carries, once, and says what came of
//! it; a clone found in place, an existing worktree and a clone with no installer run nothing,
//! and no outcome of an installer fails the clone.

use super::*;

/// What trust's installer prints first when it installs.
const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";

#[test]
fn the_hook_installer_uses_only_its_execution_accounts_git_path() {
    let sys = Fake::root();
    let alice = user("alice", 501);
    let bob = user("bob", 502);
    sys.logged_in(&alice);
    let prefix = Path::new(&alice.home).join("Library/Application Support/aterm/pkg");
    let store = prefix.join("store/git");
    let exec = Stat {
        uid: alice.uid,
        gid: 20,
        mode: 0o755,
        dir: false,
        symlink: false,
        len: 1,
    };
    sys.link(store.join("current").to_str().unwrap(), "12");
    sys.stat_of(store.join("12.ready").to_str().unwrap(), exec);
    sys.stat_of(store.join("12/bin/git").to_str().unwrap(), exec);
    sys.stat_of(prefix.join("bin/git").to_str().unwrap(), exec);
    sys.stat_of(crate::items::github::CLT_GIT, exec);
    for (who, bin) in [
        (&alice, prefix.join("bin")),
        (
            &bob,
            PathBuf::from("/Library/Developer/CommandLineTools/usr/bin"),
        ),
    ] {
        let command = installer_command(&sys, who, &Path::new(&who.home).join("checkout"));
        assert_eq!(command.user.as_ref(), Some(who));
        assert!(command.env.contains(&(
            "PATH".to_owned(),
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
        )));
    }
    assert!(
        sys.calls().is_empty(),
        "selecting the installer path spawns nothing"
    );
}

/// AN INSTALLER NEVER ASKS A PERSON FOR THE DEVELOPER TOOLS (installer#0 of the hostile review
/// of the hook installer, 2026-11-06): the setup clones before it installs the Command Line
/// Tools, and on a Mac with no developer directory an installer's `/usr/bin` shim (clean's runs
/// `python3`) puts Apple's install dialog on the person's screen. There its run names a
/// `DEVELOPER_DIR` that does not exist, so a shim fails at once; where the chosen developer
/// directory, else Xcode's or the tools' own, holds the tools, and off macOS, it names none.
/// NEGATIVE CONTROL: without the variable, the run on a Mac with no tools leaves every shim free
/// to ask.
#[test]
fn a_hook_installer_on_a_mac_without_developer_tools_cannot_ask_for_them() {
    use crate::items::github::NO_DEVELOPER_DIR;

    let who = user("alice", 501);
    let clone = Path::new(&who.home).join("checkout");
    let developer_dir = |sys: &Fake| {
        installer_command(sys, &who, &clone)
            .env
            .into_iter()
            .find(|(key, _)| key == "DEVELOPER_DIR")
            .map(|(_, value)| value)
    };
    let tools = |sys: &Fake, dir: &str| {
        sys.stat_of(&format!("{dir}/usr/bin"), directory(0));
    };
    const CLT: &str = "/Library/Developer/CommandLineTools";
    const XCODE: &str = "/Applications/Xcode.app/Contents/Developer";
    const BETA: &str = "/Applications/Xcode-beta.app/Contents/Developer";

    let bare = Fake::root();
    assert_eq!(developer_dir(&bare).as_deref(), Some(NO_DEVELOPER_DIR));
    for found in [CLT, XCODE] {
        let sys = Fake::root();
        tools(&sys, found);
        assert_eq!(developer_dir(&sys), None, "{found} holds the tools");
    }
    // A chosen directory is the one the shims use: gone, they ask though the tools are there.
    let chosen = Fake::root();
    chosen.link("/var/db/xcode_select_link", BETA);
    tools(&chosen, CLT);
    assert_eq!(developer_dir(&chosen).as_deref(), Some(NO_DEVELOPER_DIR));
    tools(&chosen, BETA);
    assert_eq!(developer_dir(&chosen), None);

    // Managed Git does not hide healthy system tools from the installer's shims.
    const ROOT: &str = "/Applications/Developer Suite";
    const ALIAS: &str = "/Applications/Alternate.app";
    const CONTENTS: &str = "/Applications/Developer Suite/Contents/Developer";
    let prefix = Path::new(&who.home).join("Library/Application Support/aterm/pkg");
    let store = prefix.join("store/git");
    let exec = Stat {
        uid: who.uid,
        gid: 20,
        mode: 0o755,
        dir: false,
        symlink: false,
        len: 1,
    };
    for selected in [
        CONTENTS,
        ROOT,
        ALIAS,
        "../../Applications/Developer Suite",
        "../../Applications/Alternate.app",
    ] {
        let sys = Fake::root();
        sys.link("/var/db/xcode_select_link", selected);
        sys.link(ALIAS, ROOT);
        sys.stat_of(CONTENTS, directory(0));
        tools(&sys, CONTENTS);
        sys.link(store.join("current").to_str().unwrap(), "12");
        sys.stat_of(store.join("12.ready").to_str().unwrap(), exec);
        sys.stat_of(store.join("12/bin/git").to_str().unwrap(), exec);
        sys.stat_of(prefix.join("bin/git").to_str().unwrap(), exec);
        let command = installer_command(&sys, &who, &clone);
        assert!(command.env.contains(&(
            "PATH".to_owned(),
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                prefix.join("bin").display()
            ),
        )));
        assert!(
            command.env.iter().all(|(key, _)| key != "DEVELOPER_DIR"),
            "selected {selected} already holds the tools"
        );
        assert!(sys.calls().is_empty(), "selection starts no child");
    }
    // A missing, unreadable or looping selection still fences Apple's install dialog,
    // even when a different developer directory happens to be installed.
    for failure in ["missing", "unreadable", "loop"] {
        let sys = Fake::root();
        sys.link("/var/db/xcode_select_link", ALIAS);
        sys.link(ALIAS, if failure == "loop" { ALIAS } else { ROOT });
        if failure == "unreadable" {
            sys.stat_fails(CONTENTS, std::io::ErrorKind::PermissionDenied);
        }
        tools(&sys, CLT);
        assert_eq!(
            developer_dir(&sys).as_deref(),
            Some(NO_DEVELOPER_DIR),
            "{failure} selection must not borrow another directory"
        );
        assert!(sys.calls().is_empty());
    }

    let mut linux = Fake::root();
    linux.linux = true;
    assert_eq!(developer_dir(&linux), None, "no shims off macOS");
    assert!(
        bare.calls().is_empty(),
        "the developer directory is read without a child"
    );
}

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

/// A published clone whose private staging stayed behind and whose required dependency then
/// failed: the row names both, and its installer still runs. NEGATIVE CONTROL: noting the
/// staging only for a clone whose dependencies came through drops that line here.
#[test]
fn a_clone_whose_staging_stayed_and_whose_dependency_failed_says_both() {
    let (sys, who, mut repo, _, installer) = clone_carrying(true);
    repo.required_submodules = vec!["vendor/library".into()];
    let parent = Path::new(&who.home).join("new");
    script_existing(&sys, &who, &parent, "new");
    crate::init_github::dependencies::tests::declarations(&sys, &who, &parent, &["vendor/library"]);
    crate::init_github::dependencies::tests::download(&sys, &who, &parent, "vendor/library", 1);
    sys.on(&installer, Ran::ok(INSTALLED));
    sys.scratch_close_fails(
        &Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0),
        Some(io::ErrorKind::PermissionDenied),
    );
    sys.simulate(|_, command| {
        command
            .display()
            .contains(" clone --no-checkout ")
            .then(|| Ran::exit(128, "synthetic unavailable dependency"))
    });
    let cloned = clone_all(&sys, &who, std::slice::from_ref(&repo), None);
    assert_eq!(cloned.failed.len(), 2, "{:?}", cloned.failed);
    assert!(
        cloned.failed[1].ends_with(
            "repository published, but its private empty staging directory could not be removed"
        ),
        "{:?}",
        cloned.failed
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
            format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED:?}"),
        ),
        (
            Some(Ran::exit(
                1,
                "ERROR: core.hooksPath is already set to 'elsewhere'.\n       Refusing\n",
            )),
            format!(
                "~/new: its {HOOK_INSTALLER} failed (exit 1: \"ERROR: core.hooksPath is \
                 already set to 'elsewhere'.\"); the clone stands"
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
        [format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED:?}")]
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

/// The listed repository github.com/me/`name`, which requires no submodule.
fn named(name: &str) -> Repo {
    Repo {
        owner: "me".into(),
        name: name.into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    }
}

/// A real repository carrying an installer: [`install_hooks`] runs it with real bash and git,
/// in the clone, as this account, and the installer's own `git config --local` lands in the
/// clone; a failing installer is a line that names its exit and first error; a link in the
/// installer's place, and a clone without one, run nothing.
///
/// AN INSTALLER RUNS ONLY FROM THE CLONE'S OWN TREE (installer F1 and F6 of the hostile review
/// of the hook installer, 2026-11-06): a link swapped in for `~/<name>` after its publish, and an
/// installer inside a required dependency's gitlink (another repository's file), run nothing,
/// and their rows say why. NEGATIVE CONTROLS: without the look at `~/<name>` itself, the link's
/// target's installer runs (the walk starts inside it, where every step is a plain folder);
/// without the gitlink rule, the dependency's installer runs.
#[test]
fn the_installer_a_real_clone_carries_runs_in_it_and_says_what_it_did() {
    crate::items::github::need_native_git();
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
    let outcome = install_hooks(&sys, &who, &named("fixture"), None).expect("it carries one");
    assert_eq!(
        outcome,
        format!(
            "~/fixture: ran its {HOOK_INSTALLER}: \"Fixture git hooks: installed \
             (core.hooksPath = scripts/hooks) in fixture\""
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
        install_hooks(&sys, &who, &named("refuses"), None).as_deref(),
        Some(
            format!(
                "~/refuses: its {HOOK_INSTALLER} failed (exit 3: \"ERROR: refused here\"); \
                 the clone stands"
            )
            .as_str()
        )
    );

    let linked = repo("linked", None);
    let target = home.path().join("elsewhere.sh");
    std::fs::write(&target, format!("touch {}\n", marker.display())).unwrap();
    std::os::unix::fs::symlink(&target, linked.join(HOOK_INSTALLER)).unwrap();
    assert_eq!(
        install_hooks(&sys, &who, &named("linked"), None).as_deref(),
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
        install_hooks(&sys, &who, &named("through"), None).as_deref(),
        Some(
            format!("~/through: its {HOOK_INSTALLER} is reached through a link; not run").as_str()
        )
    );
    assert!(!marker.exists(), "a linked installer ran");

    // `~/<name>` swapped for a link once it was published: the folder it names carries an
    // installer, every step to which is a plain folder of this account's.
    let real = repo(
        "real",
        Some(&format!(
            "#!/usr/bin/env bash\ntouch {}\n",
            marker.display()
        )),
    );
    std::os::unix::fs::symlink(&real, home.path().join("swapped")).unwrap();
    assert_eq!(
        install_hooks(&sys, &who, &named("swapped"), None).as_deref(),
        Some(
            format!("~/swapped: its {HOOK_INSTALLER} is reached through a link; not run").as_str()
        )
    );
    assert!(!marker.exists(), "the swapped-in folder's installer ran");

    // A required gitlink at `scripts` (or the installer's own path, in either case): the file
    // there is the dependency's, populated by this pass from its own remote.
    repo(
        "vendored",
        Some(&format!(
            "#!/usr/bin/env bash\ntouch {}\n",
            marker.display()
        )),
    );
    for gitlink in ["scripts", "Scripts/install-git-hooks.sh"] {
        let mut vendored = named("vendored");
        vendored.required_submodules = vec!["vendor/lib".into(), gitlink.into()];
        assert_eq!(
            install_hooks(&sys, &who, &vendored, None).as_deref(),
            Some(
                format!(
                    "~/vendored: its {HOOK_INSTALLER} is inside a required dependency; not run"
                )
                .as_str()
            ),
            "{gitlink}"
        );
    }
    assert!(!marker.exists(), "a dependency's installer ran");
    let mut beside = named("vendored");
    beside.required_submodules = vec!["scripts-lib".into(), "vendor/scripts".into()];
    assert_eq!(
        install_hooks(&sys, &who, &beside, None).as_deref(),
        Some(format!("~/vendored: ran its {HOOK_INSTALLER}").as_str()),
        "a gitlink beside the installer's folder is not around it"
    );
    std::fs::remove_file(&marker).unwrap();

    repo("bare", None);
    assert_eq!(install_hooks(&sys, &who, &named("bare"), None), None);
    // A file named `scripts`: nothing is under it, so the clone carries no installer.
    let plain = repo("plain", None);
    std::fs::remove_dir(plain.join("scripts")).unwrap();
    std::fs::write(plain.join("scripts"), b"a file, not a folder\n").unwrap();
    assert_eq!(install_hooks(&sys, &who, &named("plain"), None), None);
    home.close().unwrap();
}

/// `aterm init`'s profile stage says it too: the row its read-back reports ends with what the
/// new clone's installer printed, and a failing installer leaves the stage changed.
#[test]
fn the_profile_stage_row_says_what_a_new_clones_hook_installer_did() {
    for (reply, says) in [
        (
            Ran::ok(&format!("{INSTALLED}\n")),
            format!("; ~/new: ran its {HOOK_INSTALLER}: {INSTALLED:?}"),
        ),
        (
            Ran::exit(1, "ERROR: refused\n"),
            format!(
                "; ~/new: its {HOOK_INSTALLER} failed (exit 1: \"ERROR: refused\"); the clone \
                 stands"
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
    let says = format!("~/new: ran its {HOOK_INSTALLER}: {INSTALLED:?}");
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

/// A pass its guard stopped once the clones were done says why, then every failure its clones
/// met besides the stop (a clone the guard stopped repeats only the stop, left out), the room it
/// found short, and what the installers did. NEGATIVE CONTROL: keeping only why and the
/// installers drops the published clone whose staging stayed behind and the room line.
#[test]
fn a_stopped_pass_keeps_every_line_besides_the_stop() {
    let failed = vec![
        "~/a: late Stop".to_owned(),
        "~/b: repository published, but its private empty staging directory could not be removed"
            .to_owned(),
        "~/c: clone failed: late Stop came too".to_owned(),
    ];
    let hooks = [format!("~/b: ran its {HOOK_INSTALLER}: {INSTALLED:?}")];
    assert_eq!(
        stopped_after_clones(
            "late Stop".to_owned(),
            failed,
            Some("1 repository left for want of room".to_owned()),
            &hooks,
        ),
        format!(
            "late Stop; ~/b: repository published, but its private empty staging directory could \
             not be removed; ~/c: clone failed: late Stop came too; 1 repository left for want of \
             room; {}",
            hooks[0]
        )
    );
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

/// WHAT AN INSTALLER PRINTED IS QUOTED, ESCAPED (installer F3 of the hostile review of the hook
/// installer, 2026-11-06): a bidirectional override, a line separator `str::lines` keeps and a
/// `; ` that would start another of the row's notes reach the row inside quotes, each control,
/// bidirectional and separator character as its escape, on success and on failure alike.
/// NEGATIVE CONTROL: the earlier filter, which dropped only `char::is_control`, passes the
/// override and the separator through and leaves the forged note unquoted.
#[test]
fn an_installers_words_reach_the_row_quoted_and_escaped() {
    let forged = "done\u{202e}gnihton\u{2028}; ~/other: ran its x\u{1b}[2J\n";
    assert_eq!(
        hook_outcome(&Ok(Ran::ok(forged))),
        format!(
            "ran its {HOOK_INSTALLER}: \"done\\u{{202e}}gnihton\\u{{2028}}; ~/other: ran its \
             x\\u{{1b}}[2J\""
        )
    );
    assert_eq!(
        hook_outcome(&Ok(Ran::exit(2, "  \n\u{200f}refused \"here\"\n"))),
        format!(
            "its {HOOK_INSTALLER} failed (exit 2: \"\\u{{200f}}refused \\\"here\\\"\"); the \
             clone stands"
        )
    );
    assert_eq!(
        hook_outcome(&Ok(Ran::exit(2, ""))),
        format!("its {HOOK_INSTALLER} failed (exit 2); the clone stands")
    );
    // The escaped words are what is cut, never inside an escape (third and fourth reviews,
    // 2026-11-06): a line of controls, six characters each escaped, shows the whole escapes that
    // fit in HOOK_LINE characters, `…` after the quote; a backslash at the cut is left out whole,
    // never kept as a lone `\` that escapes the closing quote; a line HOOK_LINE characters long
    // shows whole.
    let controls = "\u{1b}".repeat(HOOK_LINE);
    let escapes = "\\u{1b}".repeat(HOOK_LINE / 6);
    assert_eq!(
        hook_outcome(&Ok(Ran::ok(&controls))),
        format!("ran its {HOOK_INSTALLER}: \"{escapes}\"…")
    );
    let pathlike = format!("{}\\x", "p".repeat(HOOK_LINE - 1));
    assert_eq!(
        hook_outcome(&Ok(Ran::ok(&pathlike))),
        format!(
            "ran its {HOOK_INSTALLER}: \"{}\"…",
            "p".repeat(HOOK_LINE - 1)
        )
    );
    let full = "x".repeat(HOOK_LINE);
    assert_eq!(
        hook_outcome(&Ok(Ran::ok(&full))),
        format!("ran its {HOOK_INSTALLER}: \"{full}\"")
    );
    assert_eq!(
        hook_outcome(&Ok(Ran::ok(&format!("{full}y")))),
        format!("ran its {HOOK_INSTALLER}: \"{full}\"…")
    );
}

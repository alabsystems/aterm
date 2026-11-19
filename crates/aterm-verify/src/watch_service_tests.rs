// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn persistent_follower_is_independent_of_the_deploying_terminal() {
    let text = manifest(
        Path::new("/state/controllers/123/aterm-verify"),
        Path::new("/state/check & judge"),
        false,
        Path::new("/Users//a/aterm"),
        Path::new("/Users//a"),
        "/bin:/custom",
        17,
    );
    // Negative controls: omitting restart or substituting the worker's cwd/executable
    // would lose the follower when that worker exits and removes its worktree.
    assert!(text.contains("<key>KeepAlive</key><true/>"));
    assert!(text.contains("<key>RunAtLoad</key><true/>"));
    assert!(text.contains(
        "<array><string>/state/controllers/123/aterm-verify</string><string>watch</string>"
    ));
    assert!(text.contains("<key>WorkingDirectory</key><string>/state/check &amp; judge</string>"));
    assert!(text.contains("<string>--repair-cwd</string><string>/Users//a/aterm</string>"));
    assert!(text.contains("<string>--repair</string><string>off</string>"));
    assert!(text.contains("<key>ProcessType</key><string>Interactive</string>"));
    assert!(text.contains("<key>ExitTimeOut</key><integer>60</integer>"));
    assert!(text.contains("<string>17</string>"));
    assert!(text.contains("<key>PATH</key><string>/bin:/custom</string>"));
    assert_eq!(xml("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
}

/// A home with the follower's property list and another agent's beside it.
fn home_with(
    list: &str,
) -> (
    aterm_tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let home = aterm_tempfile::tempdir().unwrap();
    let agents = home.path().join("Library/LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    let own = crate::launch_agent::plist(home.path(), LABEL);
    fs::write(&own, list).unwrap();
    let other = agents.join("systems.alab.aterm-release-pump.plist");
    fs::write(&other, "another agent's").unwrap();
    (home, own, other)
}

/// `--uninstall` REMOVES THE FOLLOWER'S AGENT AND NOTHING ELSE, AND SAYS WHAT IT LEFT. It asks
/// [`crate::launch_agent::remove`] for its own label with a wait longer than the follower's
/// `ExitTimeOut`; the property list goes, another agent's stays, and each end is said: removed
/// and unloaded, nothing installed, or removed while the loaded follower was still ending.
/// `--install` prints the command. launchd itself is not touched here: the unload is the
/// test's. NEGATIVE CONTROLS: the other agent's list is still there; a second uninstall finds
/// nothing and says so instead of claiming a removal.
#[test]
fn uninstall_removes_only_the_followers_agent_and_install_says_how() {
    let (home, own, other) = home_with("the follower's");
    let remove = |unload: Result<(), String>| {
        move |home: &Path, label: &str, within: Duration| {
            assert_eq!(label, "systems.alab.aterm-verify-watch");
            assert!(within > Duration::from_secs(EXIT_TIMEOUT));
            crate::launch_agent::remove_list(&crate::launch_agent::plist(home, label), || unload)
        }
    };
    let said = uninstall_in(home.path(), remove(Ok(()))).unwrap();
    assert!(said.starts_with("verify watch uninstalled: "), "{said}");
    assert!(said.contains("removed and systems.alab.aterm-verify-watch unloaded"));
    assert!(!own.exists());
    assert_eq!(fs::read_to_string(&other).unwrap(), "another agent's");
    let said = uninstall_in(home.path(), remove(Ok(()))).unwrap();
    assert!(
        said.starts_with("verify watch: no follower is installed here"),
        "{said}"
    );

    fs::write(&own, "the follower's").unwrap();
    let said = uninstall_in(home.path(), remove(Err("still loaded (75 s)".into()))).unwrap();
    assert!(said.contains("nothing starts the follower again"), "{said}");
    assert!(said.contains("still loaded (75 s)") && !own.exists());

    let line = installed("gui/501/x", Path::new("/w"), false, Path::new("/r"));
    assert!(
        line.ends_with("\nuninstall=targo --unverified verify watch --uninstall"),
        "{line}"
    );
    assert!(super::super::HELP.contains("verify watch --uninstall removes that LaunchAgent"));
}

/// REPAIRS ARE OFF UNLESS ASKED, AND THE LAUNCHAGENT'S OWN ARGUMENTS SAY WHICH. A follower
/// installed with no word about repairs carries `--repair off`, and those arguments parse back
/// to a follower that starts no worker; only `--repair worker` turns it on, and the install
/// says which it is. NEGATIVE CONTROLS: the same arguments with `worker` are the other mode; a
/// word that is neither is refused, never read as on.
#[test]
fn the_launch_agent_carries_the_repair_switch_and_it_is_off_unless_asked() {
    let list = |repair| {
        manifest(
            Path::new("/c/aterm-verify"),
            Path::new("/w"),
            repair,
            Path::new("/r"),
            Path::new("/h"),
            "/bin",
            60,
        )
    };
    assert!(list(false).contains("<string>--repair</string><string>off</string>"));
    assert!(list(true).contains("<string>--repair</string><string>worker</string>"));
    let parsed = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        super::super::options(&args, Path::new("/")).map(|opts| opts.repair)
    };
    assert_eq!(
        parsed(&["--workspace", "/w", "--interval", "60"]),
        Ok(false)
    );
    assert_eq!(
        parsed(&["--repair", "off", "--repair-cwd", "/r"]),
        Ok(false)
    );
    assert_eq!(
        parsed(&["--repair", "worker", "--repair-cwd", "/r"]),
        Ok(true)
    );
    assert_eq!(
        parsed(&["--repair", "on"]),
        Err("--repair needs off or worker".into())
    );
    assert_eq!(
        parsed(&["--repair"]),
        Err("--repair needs off or worker".into())
    );
    let off = installed("gui/501/x", Path::new("/w"), false, Path::new("/r"));
    assert!(
        off.contains("\nrepair=off (a red is filed in the journal; no agent session"),
        "{off}"
    );
    let on = installed("gui/501/x", Path::new("/w"), true, Path::new("/r"));
    assert!(
        on.contains("\nrepair=worker (a red starts one repair worker"),
        "{on}"
    );
}

/// The property list a controller wrote before the follower existed: `watch` with its checkout
/// and its interval, and no word about repairs.
const PRE_FOLLOWER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>systems.alab.aterm-verify-watch</string>
<key>ProgramArguments</key><array><string>/Users//a/.local/state/aterm-verify/.ubd/watch/controllers/9f/aterm-verify</string><string>watch</string><string>--workspace</string><string>/Users//a/.local/state/aterm-verify/.ubd/watch/checkout</string><string>--interval</string><string>60</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
</dict></plist>
"#;

/// `--sample-ready` RETIRES THE ONE LAUNCHAGENT OF A CONTROLLER FROM BEFORE THE FOLLOWER, AND
/// NOTHING ELSE. Only such a controller hands the tip's verifier that option, every minute,
/// filing a claim and a could-not-run each time. Where this label's property list is one it
/// wrote, the list and its job are removed through `launch_agent::remove` and the line says so.
/// NEGATIVE CONTROLS: a follower's list, with `--repair-cwd` or with `--repair`, stays and
/// nothing is asked to remove it; with no list nothing is removed; another agent's list beside
/// it stays in every case.
#[test]
fn sample_ready_retires_only_a_pre_follower_controllers_agent() {
    let removes = |home: &Path, label: &str, _: Duration| {
        assert_eq!(label, "systems.alab.aterm-verify-watch");
        crate::launch_agent::remove_list(&crate::launch_agent::plist(home, label), || Ok(()))
    };
    let never = |_: &Path, _: &str, _: Duration| -> Result<Removed, String> {
        panic!("a follower's agent is not retired")
    };
    let mut quiet = |_: &str| {};
    assert!(pre_follower(PRE_FOLLOWER));
    let (home, own, other) = home_with(PRE_FOLLOWER);
    let said = retire_pre_follower_in(home.path(), &mut quiet, removes).unwrap();
    assert!(
        said.contains("retired the controller from before the follower"),
        "{said}"
    );
    assert!(!own.exists());
    assert_eq!(fs::read_to_string(&other).unwrap(), "another agent's");
    let said = retire_pre_follower_in(home.path(), &mut quiet, never).unwrap();
    assert!(said.ends_with("so nothing was retired"), "{said}");

    let list = |repair| {
        manifest(
            Path::new("/c/aterm-verify"),
            Path::new("/w"),
            repair,
            Path::new("/r"),
            Path::new("/h"),
            "/bin",
            60,
        )
    };
    let before_the_switch =
        list(false).replace("<string>--repair</string><string>off</string>", "");
    assert!(before_the_switch.contains("<string>--repair-cwd</string>"));
    for follower in [list(false), list(true), before_the_switch] {
        assert!(!pre_follower(&follower));
        let (home, own, other) = home_with(&follower);
        let said = retire_pre_follower_in(home.path(), &mut quiet, never).unwrap();
        assert!(said.ends_with("is a follower's and stays"), "{said}");
        assert_eq!(fs::read_to_string(&own).unwrap(), follower);
        assert!(other.exists());
    }
    let pump = PRE_FOLLOWER.replace("aterm-verify-watch", "aterm-release-pump");
    assert!(
        !pre_follower(&pump),
        "another label's list is never this one's"
    );
}

/// THE RETIREMENT IS SAID BEFORE THE REMOVAL IS ASKED, BECAUSE THE VERIFIER DOES NOT OUTLIVE IT.
/// On the Mac it acts on, unloading the job makes launchd end the old controller, whose stop
/// ends its pass, which is this verifier: it dies inside `launch_agent::remove` and returns
/// nothing. So the one line that explains a vanished agent is written while the property list
/// is still there, names it, and stands whatever the removal then does. The removal here is the
/// test's, and reads what was said at the moment it is asked. NEGATIVE CONTROLS: a removal that
/// never answers (it fails here) leaves the line already said and no outcome; with a follower's
/// list, or none, nothing is said ahead, since nothing is about to be removed.
#[test]
fn the_retirement_is_said_before_the_removal_that_ends_this_verifier() {
    let said = std::cell::RefCell::new(Vec::<String>::new());
    let mut say = |line: &str| said.borrow_mut().push(line.to_owned());
    let (home, own, _other) = home_with(PRE_FOLLOWER);
    let ended_inside = |home: &Path, label: &str, _: Duration| -> Result<Removed, String> {
        let ahead = said.borrow();
        assert_eq!(ahead.len(), 1, "one line before the removal: {ahead:?}");
        assert!(
            ahead[0].starts_with("verify watch: retiring the controller from before the follower (systems.alab.aterm-verify-watch): removing "),
            "{ahead:?}"
        );
        assert!(ahead[0].contains(&own.display().to_string()), "{ahead:?}");
        assert!(ahead[0].contains("which ends this verifier with it"));
        assert!(ahead[0].contains("targo --unverified verify watch --install"));
        assert!(
            crate::launch_agent::plist(home, label).exists(),
            "said while the list is still there"
        );
        Err("this verifier was ended inside the removal".into())
    };
    let outcome = retire_pre_follower_in(home.path(), &mut say, ended_inside);
    assert_eq!(
        outcome,
        Err("this verifier was ended inside the removal".into())
    );
    assert_eq!(said.borrow().len(), 1, "the line stands with no outcome");

    said.borrow_mut().clear();
    let never = |_: &Path, _: &str, _: Duration| -> Result<Removed, String> {
        panic!("nothing is removed here")
    };
    let follower = manifest(
        Path::new("/c/aterm-verify"),
        Path::new("/w"),
        false,
        Path::new("/r"),
        Path::new("/h"),
        "/bin",
        60,
    );
    let (kept, _, _) = home_with(&follower);
    retire_pre_follower_in(kept.path(), &mut say, never).unwrap();
    let empty = aterm_tempfile::tempdir().unwrap();
    retire_pre_follower_in(empty.path(), &mut say, never).unwrap();
    assert!(said.borrow().is_empty(), "{:?}", said.borrow());
}

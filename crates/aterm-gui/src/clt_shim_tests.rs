// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// `entries` joined as this host's PATH joins them (`:`, `;` on Windows).
fn path(entries: &[&str]) -> String {
    entries.join(if cfg!(windows) { ";" } else { ":" })
}

#[test]
fn the_shim_sits_ahead_of_usr_bin_and_behind_a_person_s_own_tools() {
    // Homebrew's git, earlier on PATH, still wins; the shim only fronts `/usr/bin`.
    let got = on_path(
        &path(&["/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/sbin"]),
        "/S",
    );
    assert_eq!(
        got,
        path(&["/opt/homebrew/bin", "/S", "/usr/bin", "/bin", "/usr/sbin"])
    );
}

#[test]
fn with_no_usr_bin_it_fronts_bin() {
    assert_eq!(
        on_path(&path(&["/usr/local/bin", "/bin"]), "/S"),
        path(&["/usr/local/bin", "/S", "/bin"])
    );
}

#[test]
fn applying_it_twice_changes_nothing_more() {
    let once = on_path(&path(&["/opt/homebrew/bin", "/usr/bin"]), "/S");
    assert_eq!(on_path(&once, "/S"), once);
}

#[test]
fn with_no_system_bin_the_shim_is_not_added() {
    // Nothing to intercept, so the shim stays off PATH (and a stale copy is dropped).
    assert_eq!(
        on_path(&path(&["/opt/homebrew/bin", "/S"]), "/S"),
        "/opt/homebrew/bin"
    );
}

/// A spawn decides the shim's place from whether it is wanted at that moment: a tab opened
/// while it is wanted gets it ahead of `/usr/bin` (behind the person's own earlier entries),
/// whatever PATH the window composed at its launch, and the variable naming it; one opened
/// after nothing wants it gets a PATH without it and no variable.
#[test]
fn tabs_opened_while_it_is_wanted_get_it_and_after_it_is_not_do_not() {
    let var = aterm_types::clt_shim::SHIM_ENV;
    let composed = || {
        vec![
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            (
                "PATH".to_owned(),
                path(&["/opt/homebrew/bin", "/usr/bin", "/bin"]),
            ),
        ]
    };
    let get = |env: &[(String, String)], key: &str| {
        env.iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let nothing = |_: &str| None;
    let mut wanted = composed();
    settle_session_path(&mut wanted, &nothing, Some("/S"), false);
    assert_eq!(
        get(&wanted, "PATH"),
        Some(path(&["/opt/homebrew/bin", "/S", "/usr/bin", "/bin"]))
    );
    assert_eq!(get(&wanted, var).as_deref(), Some("/S"));
    for key in ["PATH", var] {
        assert_eq!(wanted.iter().filter(|(name, _)| name == key).count(), 1);
    }

    // No PATH composed at launch: the inherited one carries it.
    let mut bare = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
    let inherited = |key: &str| (key == "PATH").then(|| path(&["/usr/bin", "/bin"]));
    settle_session_path(&mut bare, &inherited, Some("/S"), false);
    assert_eq!(get(&bare, "PATH"), Some(path(&["/S", "/usr/bin", "/bin"])));
    assert_eq!(get(&bare, var).as_deref(), Some("/S"));

    // Not wanted: a copy left on the PATH is dropped, and a clean PATH is left as it was.
    let shim = aterm_types::clt_shim::shim_dir().unwrap();
    let shim = shim.to_str().unwrap();
    let mut stale = vec![("PATH".to_owned(), path(&[shim, "/usr/bin"]))];
    settle_session_path(&mut stale, &nothing, None, true);
    assert_eq!(get(&stale, "PATH").as_deref(), Some("/usr/bin"));
    assert_eq!(get(&stale, var), None, "no variable where none was");
    let mut clean = composed();
    settle_session_path(&mut clean, &nothing, None, true);
    assert_eq!(clean, composed());
    let mut none = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
    let clean_path = |key: &str| (key == "PATH").then(|| "/usr/bin".to_owned());
    settle_session_path(&mut none, &clean_path, None, true);
    assert_eq!(
        get(&none, "PATH"),
        None,
        "an inherited clean PATH is not restated"
    );

    // A window started from a shell that had the shim: its variable is emptied, and the
    // directory it names is dropped from the PATH.
    let mut heir = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
    let from_a_shim_shell = |key: &str| match key {
        "PATH" => Some(path(&["/opt/homebrew/bin", "/T", "/usr/bin"])),
        key if key == var => Some("/T".to_owned()),
        _ => None,
    };
    settle_session_path(&mut heir, &from_a_shim_shell, None, true);
    assert_eq!(
        get(&heir, "PATH"),
        Some(path(&["/opt/homebrew/bin", "/usr/bin"]))
    );
    assert_eq!(get(&heir, var).as_deref(), Some(""));
}

/// While the tools are missing the shim stays wanted, whatever becomes of an install: only its
/// installing mark, which changes a shim's line, follows the install. Once they exist both
/// marks go at once, and the directory, with its links, stays for the processes that reach
/// it; a next launch keeps it too, its developer directory's `xcrun` linked onto that launch's
/// executable, so a process that inherited `DEVELOPER_DIR` from before still reaches a shim.
/// NEGATIVE CONTROL: on a Mac with the tools a launch lays nothing at all.
#[cfg(unix)]
#[test]
fn the_shim_stays_until_the_tools_exist_and_only_then_comes_off_new_shells() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PRESENT: AtomicBool = AtomicBool::new(false);
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dir = tmp.path().join("clt-shim");
    let installing = || aterm_types::clt_shim::installing_mark(&dir).is_file();
    let shim = Shim::at(dir.clone(), || PRESENT.load(Ordering::SeqCst));
    shim.reconcile();
    assert!(
        shim.laid() && shim.wanted(),
        "from the launch, with no pass"
    );
    assert!(!installing());
    shim.settle(|| true);
    assert!(shim.wanted() && installing());
    shim.settle(|| false);
    assert!(shim.wanted(), "a Stop, or an unfinished install, leaves it");
    assert!(!installing());
    let links = std::fs::read_dir(&dir).unwrap().count();
    PRESENT.store(true, Ordering::SeqCst);
    shim.settle(|| true);
    assert!(!shim.wanted() && !installing(), "the tools exist");
    assert!(shim.laid());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), links);
    let xcrun = aterm_types::clt_shim::developer_dir(&dir)
        .join(aterm_types::clt_shim::DEVELOPER_DIR_BIN)
        .join("xcrun");
    std::fs::remove_file(&xcrun).unwrap();
    shim.reconcile();
    assert!(
        shim.laid() && !shim.wanted(),
        "the next launch keeps it, unwanted"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), links);
    assert_eq!(
        std::fs::read_link(&xcrun).unwrap(),
        std::env::current_exe().unwrap(),
        "its developer directory's xcrun, linked onto this launch's executable"
    );

    let owner = tmp.path().join("owner").join("clt-shim");
    let with_the_tools = Shim::at(owner.clone(), || true);
    with_the_tools.reconcile();
    assert!(!owner.exists(), "the tools are here: nothing is laid");
}

/// Apple's real `/usr/bin/git` with `DEVELOPER_DIR` naming the directory [`lay`] lays runs that
/// directory's `xcrun`, whose target here is a stand-in that notes it ran; the real one is
/// `aterm`, whose shim prints a line and exits. Named before the directory is laid (as a
/// process start names it) the stub fails plainly, with Apple's own error and no dialog.
/// Measured against the real stub, so only on a Mac whose tools answer in place of its dialog.
/// NEGATIVE CONTROL: without the variable the same stub never reaches the stand-in.
#[cfg(target_os = "macos")]
#[test]
fn apples_real_stub_reaches_the_laid_developer_dir_and_never_its_install_route() {
    use std::os::unix::fs::PermissionsExt as _;
    if !aterm_types::clt_shim::developer_git() {
        eprintln!("this Mac has no developer tools: its stub would raise Apple's dialog");
        return;
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let noted = root.join("noted");
    let stand_in = root.join("stand-in");
    std::fs::write(
        &stand_in,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nexit 69\n",
            noted.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shim = root.join("clt-shim");
    let developer = aterm_types::clt_shim::developer_dir(&shim);
    let git = |dir: Option<&Path>| {
        let mut git = aterm_uds::spawnfd::command("/usr/bin/git");
        git.arg("--version")
            .env_remove(aterm_types::clt_shim::DEVELOPER_DIR_ENV);
        if let Some(dir) = dir {
            git.env(aterm_types::clt_shim::DEVELOPER_DIR_ENV, dir);
        }
        git.output().unwrap()
    };
    let unlaid = git(Some(&developer));
    assert_eq!(unlaid.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unlaid.stderr).contains("DEVELOPER_DIR"),
        "{unlaid:?}"
    );
    lay(&shim, &stand_in, &["git"]).unwrap();
    let laid = git(Some(&developer));
    assert_eq!(laid.status.code(), Some(69), "{laid:?}");
    assert_eq!(
        std::fs::read_to_string(&noted).unwrap(),
        "git --version\n",
        "Apple's stub called the laid xcrun with the tool and its words"
    );
    std::fs::remove_file(&noted).unwrap();
    assert!(git(None).status.success(), "the tools' own git");
    assert!(!noted.exists(), "no variable, no stand-in");
}

/// A spawn hands `DEVELOPER_DIR` on while the shim is wanted, a person's own excepted, and
/// empties one this process kept from before the tools arrived, only once they did.
/// NEGATIVE CONTROL: with the tools still missing and the directory not wanted (its laying
/// failed), the inherited one stays, as an empty one would send Apple's stubs to their dialog.
#[test]
fn a_spawn_names_the_developer_dir_only_while_the_shim_is_wanted() {
    let key = aterm_types::clt_shim::DEVELOPER_DIR_ENV;
    let get = |env: &[(String, String)]| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let ours = "/s/clt-shim/developer";
    // What a spawn names: `developer` joined onto the shim as this host joins a path.
    let named = Path::new("/s/clt-shim").join("developer");
    let with = |value: &'static str| move |k: &str| (k == key).then(|| value.to_owned());
    let nothing = |_: &str| None;

    let mut env = Vec::new();
    settle_session_path(&mut env, &nothing, Some("/s/clt-shim"), false);
    assert_eq!(get(&env).as_deref(), named.to_str());

    let mut env = Vec::new();
    settle_session_path(&mut env, &with("/opt/xcode"), Some("/s/clt-shim"), false);
    assert_eq!(get(&env), None, "a person's own stands");

    let mut env = Vec::new();
    settle_session_path(&mut env, &with(ours), None, true);
    assert_eq!(get(&env).as_deref(), Some(""), "the tools arrived");

    let mut env = Vec::new();
    settle_session_path(&mut env, &with(ours), None, false);
    assert_eq!(get(&env), None, "still missing: the inherited one stays");

    for inherited in ["/opt/xcode", ""] {
        for present in [false, true] {
            let mut env = Vec::new();
            settle_session_path(&mut env, &with(inherited), None, present);
            assert_eq!(get(&env), None, "{inherited:?}");
        }
    }
    let mut env = Vec::new();
    settle_session_path(&mut env, &nothing, None, true);
    assert_eq!(get(&env), None, "the owner's Mac gets nothing");
}

/// A launch never takes the shim directory from under a live shell, whether it adopted that
/// shell (a seamless hand-off to an updated aterm, the PTY keeper after a crash) or the shell
/// outlived the window on its own (a tmux server started from a tab): a bash there that hashed
/// `<shim>/git` while the install ran would fail `No such file or directory` (127). The launch
/// takes only the mark. Stand-ins only: the shim's `git` and the next directory's are scripts
/// of this test's own.
#[cfg(unix)]
#[test]
fn a_launch_leaves_a_live_shells_shim_runnable() {
    use std::io::{BufRead as _, Read as _, Write as _};
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let shim = root.join("clt-shim");
    let real = root.join("real");
    let aterm = root.join("aterm");
    std::fs::create_dir_all(&shim).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
    let script = |path: &Path, body: &str| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(&aterm, "echo shim");
    std::os::unix::fs::symlink(&aterm, shim.join("git")).unwrap();
    script(&real.join("git"), "echo real");
    let mut child = aterm_uds::spawnfd::command("/bin/bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "git; read _; git; echo rc=$?",
        ])
        .env_clear()
        .env("PATH", format!("{}:{}", shim.display(), real.display()))
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "shim", "the install under way");
    // The successor's launch with the tools there, its predecessor's shells adopted alive.
    let kept = Shim::at(shim.clone(), || true).onto(aterm.clone());
    kept.reconcile();
    assert!(!kept.wanted(), "new shells no longer get it");
    assert!(kept.laid(), "the adopted shells' directory stays");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"\n").unwrap();
    drop(stdin);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        rest,
        "shim\nrc=0\n",
        "an adopted bash keeps its hashed git: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A bash a person opened while the shim was wanted, which ran `git` then (and so hashed it
/// at the shim's path), keeps working once the tools arrive: the window takes the want away
/// and leaves the directory, so that bash's next `git` reaches the shim, which runs the
/// tools' own `git`. Before, the directory went with the want and that `git` failed `No such
/// file or directory` (exit 127) until `hash -r`. Stand-ins only: the shim's `git` (which
/// runs the real one once the tools are there, as `aterm_cli::clt_shim` does) and the real
/// one are scripts of this test's own.
#[cfg(unix)]
#[test]
fn a_bash_that_ran_git_while_the_shim_was_laid_runs_git_once_the_tools_arrive() {
    use std::io::{BufRead as _, Read as _, Write as _};
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let shim = root.join("clt-shim");
    let real = root.join("real");
    let arrived = root.join("arrived");
    std::fs::create_dir_all(&shim).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
    let script = |path: &Path, body: &str| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(
        &shim.join("git"),
        &format!(
            "[ -e '{}' ] && exec '{}' \"$@\"\necho shim",
            arrived.display(),
            real.join("git").display()
        ),
    );
    script(&real.join("git"), "echo real");
    let mut child = aterm_uds::spawnfd::command("/bin/bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "git; read _; git; echo rc=$?",
        ])
        .env_clear()
        .env("PATH", format!("{}:{}", shim.display(), real.display()))
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "shim", "the tools still installing");
    // The tools arrived: the window settles the shim.
    std::fs::write(&arrived, b"").unwrap();
    let kept = Shim::at(shim.clone(), || true);
    kept.settle(|| true);
    assert!(!kept.wanted(), "new shells no longer get it");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"\n").unwrap();
    drop(stdin);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        rest,
        "real\nrc=0\n",
        "the shell's hashed git, once the tools arrived: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn lay_links_each_tool_onto_aterm_then_teardown_removes_them() {
    let tmp = std::env::temp_dir().join(format!("aterm-clt-shim-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("clt-shim");
    let aterm_exe = tmp.join("aterm");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&aterm_exe, b"#!/bin/sh\n").unwrap();

    lay(&dir, &aterm_exe, &["git", "cc"]).unwrap();
    assert_eq!(std::fs::read_link(dir.join("git")).unwrap(), aterm_exe);
    assert_eq!(std::fs::read_link(dir.join("cc")).unwrap(), aterm_exe);

    // A shrunken set drops the link it no longer owns.
    lay(&dir, &aterm_exe, &["git"]).unwrap();
    assert!(dir.join("git").exists());
    assert!(dir.join("cc").symlink_metadata().is_err());

    // Teardown removes our links, never the binary they point at.
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!dir.exists());
    assert!(aterm_exe.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{bundle_dir, reroute_path_env};
use std::path::Path;

const SEP: char = if cfg!(windows) { ';' } else { ':' };
const REROUTE: &str = "/Users//u/Library/Application Support/aterm/pkg/reroute";
const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS";

fn path(parts: &[&str]) -> String {
    parts.join(&SEP.to_string())
}

const AGENTS: &str = "/Users//u/Library/Application Support/aterm/pkg/agents";

#[test]
fn the_injected_dirs_lead_the_inherited_path_once_each() {
    type Case = (
        &'static str,
        [Option<&'static str>; 3],
        Option<String>,
        Option<String>,
    );
    let cases: [Case; 12] = [
        (
            "agents beside the reroute dir, moved out of the user's PATH",
            [Some(REROUTE), Some(AGENTS), Some(BUNDLE)],
            Some(path(&[
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                AGENTS,
            ])),
            Some(path(&[
                REROUTE,
                AGENTS,
                BUNDLE,
                "/Users//u/.local/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
            ])),
        ),
        (
            "the agents dir alone",
            [None, Some(AGENTS), None],
            Some("/usr/bin".to_string()),
            Some(path(&[AGENTS, "/usr/bin"])),
        ),
        (
            "reroute, bundle, inherited",
            [Some(REROUTE), None, Some(BUNDLE)],
            Some(path(&["/usr/bin", "/bin"])),
            Some(path(&[REROUTE, BUNDLE, "/usr/bin", "/bin"])),
        ),
        (
            "a later reroute dir moves to the front",
            [Some(REROUTE), None, None],
            Some(path(&[
                "/opt/homebrew/bin",
                "/Users//u/.cargo/bin",
                REROUTE,
                "/usr/bin",
                REROUTE,
            ])),
            Some(path(&[
                REROUTE,
                "/opt/homebrew/bin",
                "/Users//u/.cargo/bin",
                "/usr/bin",
            ])),
        ),
        (
            "a reachable bundle dir is not stacked",
            [Some(REROUTE), None, Some(BUNDLE)],
            Some(path(&[BUNDLE, "/usr/bin"])),
            Some(path(&[REROUTE, BUNDLE, "/usr/bin"])),
        ),
        (
            "the bundle dir alone",
            [None, None, Some(BUNDLE)],
            Some("/usr/bin".to_string()),
            Some(path(&[BUNDLE, "/usr/bin"])),
        ),
        (
            "the bundle dir alone, already reachable",
            [None, None, Some(BUNDLE)],
            Some(path(&[BUNDLE, "/usr/bin"])),
            None,
        ),
        (
            "no inherited PATH",
            [Some(REROUTE), None, Some(BUNDLE)],
            None,
            Some(path(&[REROUTE, BUNDLE])),
        ),
        (
            "an empty inherited PATH",
            [Some(REROUTE), None, None],
            Some(String::new()),
            Some(REROUTE.to_string()),
        ),
        (
            "an empty entry survives",
            [Some(REROUTE), None, None],
            Some(path(&["/usr/bin", "", "/bin"])),
            Some(path(&[REROUTE, "/usr/bin", "", "/bin"])),
        ),
        (
            "nothing to inject",
            [None, None, None],
            Some("/usr/bin".to_string()),
            None,
        ),
        ("nothing at all", [None, None, None], None, None),
    ];
    for (what, [reroute, agents, bundle], inherited, want) in cases {
        let got = reroute_path_env(reroute, agents, bundle, inherited.as_deref(), None);
        assert!(
            got.as_ref().is_none_or(|(key, _)| key == "PATH"),
            "{what}: {got:?}"
        );
        let got = got.map(|(_, value)| value);
        assert_eq!(got, want, "{what}");
        if let Some(value) = got {
            assert!(
                reroute_path_env(reroute, agents, bundle, Some(&value), None)
                    .is_none_or(|(_, again)| again == value),
                "{what}: idempotent under nesting"
            );
        }
    }
}

#[test]
fn the_agents_dir_is_ensured_at_launch_so_the_first_tab_is_covered() {
    use super::managed_agents_dir;
    let prefix = aterm_tempfile::tempdir().expect("scratch prefix");
    let layout = atpkg::store::Layout {
        prefix: prefix.path().join("pkg"),
    };
    assert!(
        !layout.agents_dir().is_dir(),
        "a fresh prefix has no agents/"
    );
    let dir = managed_agents_dir(&layout).expect("created, hence on PATH");
    assert_eq!(dir, layout.agents_dir().to_str().unwrap());
    assert!(layout.agents_dir().is_dir());
    assert_eq!(managed_agents_dir(&layout).as_deref(), Some(dir.as_str()));
    #[cfg(unix)]
    for (tag, plant) in [("link", true), ("file", false)] {
        let prefix = aterm_tempfile::tempdir().expect("scratch prefix");
        let layout = atpkg::store::Layout {
            prefix: prefix.path().join("pkg"),
        };
        std::fs::create_dir_all(&layout.prefix).expect("prefix");
        let elsewhere = prefix.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("target");
        if plant {
            std::os::unix::fs::symlink(&elsewhere, layout.agents_dir()).expect("link");
        } else {
            std::fs::write(layout.agents_dir(), b"x").expect("file");
        }
        assert_eq!(
            managed_agents_dir(&layout),
            None,
            "{tag} at agents/ is refused"
        );
        let md = std::fs::symlink_metadata(layout.agents_dir()).expect("left alone");
        assert_eq!(md.file_type().is_symlink(), plant, "{tag} left as it was");
    }
}

#[cfg(unix)]
#[test]
fn the_atpkg_child_path_is_the_login_shells_or_the_fallback() {
    use super::{fallback_child_path, pick_login_path};
    let banner = "Welcome!\nLast login: never\n/Users//u/.local/bin:/Users//u/Library/Application Support/aterm/pkg/bin:/usr/bin\n";
    assert_eq!(
        pick_login_path(banner).as_deref(),
        Some("/Users//u/.local/bin:/Users//u/Library/Application Support/aterm/pkg/bin:/usr/bin")
    );
    assert_eq!(pick_login_path("nothing here\n"), None);
    assert_eq!(pick_login_path(""), None);
    let home = Path::new("/Users//u");
    let brew: &[&str] = if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin"]
    } else {
        &[]
    };
    let with = |head: &[&str], tail: &[&str]| -> String {
        let mut all: Vec<&str> = head.to_vec();
        all.extend_from_slice(brew);
        all.extend_from_slice(tail);
        path(&all)
    };
    assert_eq!(
        fallback_child_path(Some("/usr/bin:/bin:/usr/sbin:/sbin"), Some(home), None),
        with(
            &[
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
                "/Users//u/.local/bin"
            ],
            &["/usr/local/bin"]
        ),
        "launchd's four, then the foreign homes"
    );
    assert_eq!(
        fallback_child_path(Some("/usr/local/bin:/usr/bin"), None, None),
        with(&["/usr/local/bin", "/usr/bin"], &[]),
        "already-present dirs are not repeated; no home, no ~/.local/bin"
    );
    assert_eq!(
        fallback_child_path(None, Some(home), None),
        with(&["/Users//u/.local/bin"], &["/usr/local/bin"])
    );
    let agents = "/Users//u/Library/Application Support/aterm/pkg/agents";
    let bin = "/Users//u/Library/Application Support/aterm/pkg/bin";
    assert_eq!(
        fallback_child_path(
            Some(&format!("/usr/bin:{agents}:/bin:{bin}")),
            Some(home),
            Some((agents, bin))
        ),
        with(
            &[agents, bin, "/usr/bin", "/bin", "/Users//u/.local/bin"],
            &["/usr/local/bin"]
        ),
        "agents/ then bin/ moved to the front ahead of /usr/bin"
    );
}

#[cfg(unix)]
#[test]
fn the_path_probe_asks_the_accounts_shell_or_the_platforms_default() {
    use super::path_probe_shell;
    for shell in ["/bin/bash", "/usr/bin/zsh", "/usr/bin/fish"] {
        assert_eq!(path_probe_shell(shell, true), shell);
        assert_eq!(path_probe_shell(shell, false), shell);
    }
    for shell in ["/usr/bin/dash", "/bin/sh", "/bin/mksh"] {
        assert_eq!(
            path_probe_shell(shell, false),
            shell,
            "the sh family off macOS"
        );
        assert_eq!(path_probe_shell(shell, true), "/bin/zsh");
    }
    for shell in ["/bin/tcsh", "/usr/bin/nu", "", "xonsh"] {
        assert_eq!(path_probe_shell(shell, false), "/bin/sh", "{shell:?}");
        assert_eq!(path_probe_shell(shell, true), "/bin/zsh", "{shell:?}");
    }
}

#[test]
fn a_first_run_update_takes_the_fallback_rather_than_wait_for_the_login_shell() {
    use super::update_path_with;
    let never = || -> &'static str { panic!("a first run waited for the login shell") };
    assert_eq!(
        update_path_with(true, None, never, || "/fallback".to_string()),
        "/fallback"
    );
    assert_eq!(
        update_path_with(false, None, || "/login", || unreachable!()),
        "/login",
        "a routine pass waits for the login shell"
    );
    for first_run in [true, false] {
        assert_eq!(
            update_path_with(first_run, Some("/login"), never, || unreachable!()),
            "/login",
            "the login shell's answer, once it is in"
        );
    }
}

#[test]
#[cfg(windows)]
fn login_shell_path_asks_no_shell_on_windows() {
    if !crate::test_alone::alone() {
        return;
    }
    let asks = || super::LOGIN_SHELL_ASKS.with(std::cell::Cell::get);
    let git_bash = [
        r"C:\Program Files\Git\usr\bin\bash",
        r"C:\Program Files\Git\bin\bash",
    ]
    .into_iter()
    .find(|bash| std::path::Path::new(&format!("{bash}.exe")).is_file());
    let shell = git_bash.unwrap_or("/usr/bin/bash");

    let before = asks();
    let answered = super::login_shell_path_with(
        shell,
        &["-c", "printf '%s\\n' \"$PATH\""],
        std::time::Duration::from_secs(60),
    );
    assert_eq!(asks(), before + 1, "the control: a direct ask is counted");
    if let Some(bash) = git_bash {
        assert!(
            answered.is_some(),
            "the control: {bash} answers a PATH when asked directly"
        );
    }

    let before = asks();
    let path = crate::test_env::scoped("SHELL", shell, super::login_shell_path);
    assert_eq!(
        asks(),
        before,
        "SHELL={shell}: login_shell_path asked a shell"
    );
    assert_eq!(path, None, "SHELL={shell}");
}

#[test]
#[cfg(target_os = "macos")]
fn login_path_shell_runs_below_the_inherited_band() {
    let path = super::login_shell_path_with(
        "/bin/sh",
        &["-c", "printf '/pri:%s\\n' \"$(/bin/ps -o pri= -p $$)\""],
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the probe shell answered");
    let pri: i32 = path
        .trim_start_matches("/pri:")
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("no priority in {path:?}"));
    assert!(
        pri <= 20,
        "the login-shell probe ran at pri {pri}, not utility (20)"
    );
}

#[test]
#[cfg(unix)]
fn login_path_budget_bounds_a_grandchild_holding_stdout() {
    const JOB: std::time::Duration = std::time::Duration::from_secs(60);
    let script = format!("sleep {}; printf '/a:/b\\n'", JOB.as_secs());
    let started = std::time::Instant::now();
    let path = super::login_shell_path_with(
        "/bin/sh",
        &["-c", script.as_str()],
        std::time::Duration::from_millis(300),
    );
    let elapsed = started.elapsed();
    assert_eq!(path, None);
    assert!(elapsed < JOB, "waited for the grandchild: {elapsed:?}");
}

#[test]
#[cfg(unix)]
fn login_path_returns_despite_a_backgrounded_rc_job() {
    const JOB: std::time::Duration = std::time::Duration::from_secs(120);
    const BUDGET: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    let script = format!("sleep {} & printf '/a:/b:%s\\n' \"$!\"", JOB.as_secs());
    let started = std::time::Instant::now();
    let path = super::login_shell_path_with("/bin/sh", &["-c", script.as_str()], BUDGET);
    let elapsed = started.elapsed();
    let job = path
        .as_deref()
        .and_then(|line| line.strip_prefix("/a:/b:"))
        .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        .filter(|&pid| pid > 0);
    if let Some(pid) = job.filter(|_| elapsed < JOB) {
        // SAFETY: `kill` takes no pointers, and `pid` is positive, so it names one process
        // and never a group.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(
        elapsed < JOB,
        "waited for the backgrounded job: {elapsed:?}"
    );
    assert!(job.is_some(), "no PATH carrying the job's pid: {path:?}");
}

#[test]
#[cfg(unix)]
fn login_path_answers_under_a_foreground_controlling_tty() {
    const CHILD: &str = "ATERM_TEST_LOGIN_PATH_UNDER_A_PTY";
    const SHELLS: [(&str, &[&str]); 2] = [
        ("/bin/zsh", &["-f", "-i", "-c", "printf '/a:/b\\n'"]),
        (
            "/bin/bash",
            &["--norc", "--noprofile", "-i", "-c", "printf '/a:/b\\n'"],
        ),
    ];
    let shells = || {
        SHELLS
            .into_iter()
            .filter(|(shell, _)| std::path::Path::new(shell).exists())
    };
    if std::env::var_os(CHILD).is_some() {
        #[cfg(target_vendor = "apple")]
        {
            // SAFETY: the native spawn made this original child its own
            // session leader. Set its own inherited stdin as the ctty.
            assert_eq!(unsafe { libc::ioctl(0, libc::TIOCSCTTY as _, 0) }, 0);
        }
        // SAFETY: both calls take no pointers and only read process state.
        let (foreground, own) = unsafe { (libc::tcgetpgrp(libc::STDIN_FILENO), libc::getpgrp()) };
        assert_eq!(
            foreground, own,
            "the re-exec is not in the foreground of a controlling tty"
        );
        for (shell, args) in shells() {
            let started = std::time::Instant::now();
            let path = super::login_shell_path_with(shell, args, aterm_spec::wait::MUST_HAPPEN);
            let elapsed = started.elapsed();
            assert_eq!(path.as_deref(), Some("/a:/b"), "{shell} after {elapsed:?}");
        }
        return;
    }
    if shells().next().is_none() {
        aterm_spec::host::could_not_run(
            "no /bin/zsh or /bin/bash on this host; install zsh or bash",
        );
    }
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
    // SAFETY: two live descriptors this test owns, each taken by exactly one `OwnedFd`; the
    // stand-in's stdin is the native copy of `slave`.
    let (master, slave) = unsafe {
        use std::os::fd::{FromRawFd as _, OwnedFd};
        (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
    };
    let mut stand_in = crate::test_alone::StandIn::this_test();
    stand_in
        .command()
        .env(CHILD, "1")
        .env("HISTFILE", "/dev/null")
        .stdin(crate::test_alone::Stdio::from(slave));
    #[cfg(target_vendor = "apple")]
    stand_in.command().setsid(true);
    // SAFETY: the hook runs between fork and exec and calls only `setsid` and `ioctl`, both
    // async-signal-safe, on the child's own stdin.
    #[cfg(not(target_vendor = "apple"))]
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(stand_in.command(), || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    stand_in.spawn().passed();
    drop(master);
}

#[test]
fn the_bin_dir_leads_behind_agents_and_ahead_of_the_inherited_path() {
    const BIN: &str = "/Users//u/Library/Application Support/aterm/pkg/bin";
    let inherited = path(&["/opt/homebrew/bin", "/usr/bin"]);
    let (_, value) = reroute_path_env(
        Some(REROUTE),
        Some(AGENTS),
        Some(BUNDLE),
        Some(&inherited),
        Some(BIN),
    )
    .expect("injects");
    assert_eq!(
        value,
        path(&[
            REROUTE,
            AGENTS,
            BIN,
            BUNDLE,
            "/opt/homebrew/bin",
            "/usr/bin"
        ])
    );
    let (_, again) = reroute_path_env(
        Some(REROUTE),
        Some(AGENTS),
        Some(BUNDLE),
        Some(&value),
        Some(BIN),
    )
    .expect("injects");
    assert_eq!(again, value, "idempotent on its own output");
    let trailing_bin = path(&["/opt/homebrew/bin", "/usr/bin", BIN]);
    let (_, moved) = reroute_path_env(
        Some(REROUTE),
        Some(AGENTS),
        Some(BUNDLE),
        Some(&trailing_bin),
        Some(BIN),
    )
    .expect("injects");
    assert_eq!(
        moved, value,
        "an outer shell's trailing bin/ moves ahead of /usr/bin once"
    );
    assert_eq!(
        reroute_path_env(None, None, None, Some("/usr/bin"), Some(BIN)).map(|p| p.1),
        Some(path(&[BIN, "/usr/bin"]))
    );
    assert_eq!(
        reroute_path_env(None, None, None, None, Some(BIN)).map(|p| p.1),
        Some(BIN.to_string()),
        "no inherited PATH: no dangling separator"
    );
    assert_eq!(
        reroute_path_env(Some(REROUTE), Some(AGENTS), None, Some(&inherited), None).map(|p| p.1),
        Some(path(&[REROUTE, AGENTS, "/opt/homebrew/bin", "/usr/bin"])),
        "None hands no bin/"
    );
}

#[cfg(not(windows))]
#[test]
fn bundle_dir_is_only_a_real_app_layout() {
    let app = bundle_dir(Some(Path::new(
        "/Applications/aterm.app/Contents/MacOS/aterm",
    )));
    if cfg!(target_os = "linux") {
        assert_eq!(
            app, None,
            "Linux has no app bundle: its rule is the store's"
        );
    } else {
        assert_eq!(app.as_deref(), Some(BUNDLE));
    }
    assert_eq!(
        bundle_dir(Some(Path::new("/Users//u/aterm/target/release/aterm-gui"))),
        None
    );
    assert_eq!(bundle_dir(None), None);
    let dir = aterm_tempfile::tempdir().expect("a store folder");
    let store = dir.path().canonicalize().expect("the store resolves");
    let exe = store.join("aterm");
    std::fs::write(&exe, b"").expect("the binary");
    assert_eq!(bundle_dir(Some(&exe)), None, "no aterm-ctl alias yet");
    std::os::unix::fs::symlink("aterm", store.join("aterm-ctl")).expect("the alias");
    assert_eq!(
        bundle_dir(Some(&exe)).as_deref(),
        cfg!(target_os = "linux").then(|| store.to_str().expect("a UTF-8 path")),
        "the store is the PATH folder on Linux only; macOS tabs take the bundle's"
    );
}

#[test]
fn linux_store_dir_requires_aterm_ctl_sibling() {
    use super::linux_store_dir;
    use std::path::PathBuf;
    const STORE: &str = "/home/u/.local/lib/aterm/bin";
    let exe = Path::new(STORE).join("aterm");
    let ctl = Path::new(STORE).join("aterm-ctl");
    let installed = |path: &Path| (path == exe || path == ctl).then(|| exe.clone());
    assert_eq!(linux_store_dir(&exe, installed).as_deref(), Some(STORE));
    let alone = |path: &Path| (path == exe).then(|| exe.clone());
    assert_eq!(linux_store_dir(&exe, alone), None);
    let own = |path: &Path| Some(PathBuf::from(path));
    assert_eq!(
        linux_store_dir(Path::new("/home/u/work/target/debug/aterm"), own),
        None,
        "a build tree's aterm-ctl is a binary of its own"
    );
    assert_eq!(
        linux_store_dir(&Path::new(STORE).join("aterm-gui"), installed),
        None,
        "the links name `aterm`, so only a binary of that name is the store's"
    );
    let inherited = path(&["/usr/local/bin", "/usr/bin"]);
    assert_eq!(
        reroute_path_env(Some(REROUTE), None, Some(STORE), Some(&inherited), None).map(|p| p.1),
        Some(path(&[REROUTE, STORE, "/usr/local/bin", "/usr/bin"]))
    );
}

#[test]
fn a_windows_app_folder_with_the_console_image_qualifies() {
    use super::windows_app_dir;
    const APP: &str = r"C:\Users\u\AppData\Local\Programs\aterm";
    let holds = |dir: &'static str| move |path: &Path| path == Path::new(dir).join("aterm.exe");
    for image in ["aterm-gui.exe", "aterm.exe", "aterm-windowed.exe"] {
        let exe = Path::new(APP).join(image);
        assert_eq!(
            windows_app_dir(&exe, holds(APP)).as_deref(),
            Some(APP),
            "{image}"
        );
    }
    assert_eq!(
        windows_app_dir(&Path::new(APP).join("aterm-gui.exe"), |_| false),
        None
    );
    assert_eq!(windows_app_dir(Path::new("aterm.exe"), |_| true), None);
    #[cfg(windows)]
    {
        assert_eq!(bundle_dir(None), None);
        let dir = aterm_tempfile::tempdir().expect("an app folder");
        let gui = dir.path().join("aterm-gui.exe");
        std::fs::write(&gui, b"").expect("the windowed image");
        assert_eq!(bundle_dir(Some(&gui)), None, "no console image yet");
        std::fs::write(dir.path().join("aterm.exe"), b"").expect("the console image");
        assert_eq!(bundle_dir(Some(&gui)).as_deref(), dir.path().to_str());
        let inherited = path(&[r"C:\Windows\system32", r"C:\Windows"]);
        assert_eq!(
            reroute_path_env(None, None, Some(APP), Some(&inherited), None).map(|p| p.1),
            Some(path(&[APP, r"C:\Windows\system32", r"C:\Windows"]))
        );
    }
}

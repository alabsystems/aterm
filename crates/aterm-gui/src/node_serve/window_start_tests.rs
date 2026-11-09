// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{
    Image, Launcher, Look, Start, UNIT_VARIABLES, Verdict, await_exec, command, decide, judge,
    process_name, program_at,
};

const UID: u32 = 1000;
const UNDER_MANAGER: &str =
    "0::/user.slice/user-1000.slice/user@1000.service/app.slice/systems.alab.aterm-node.service\n";

fn systemd_run() -> Option<PathBuf> {
    Some(PathBuf::from("/usr/bin/systemd-run"))
}

fn random() -> Option<String> {
    Some("0123456789abcdef".to_owned())
}

/// A node its user manager runs, with `systemd-run` and a random name at hand, starts the window
/// in its own `app-aterm-<random>.scope`; each missing piece leaves it the node's own child.
/// NEGATIVE CONTROLS: a login session's scope, another uid's manager, a component that only
/// contains the manager's name, an unread cgroup, no `systemd-run` and no random name each decide
/// `Child`; return `Scope` unconditionally from `decide` and they fail.
#[test]
fn the_node_its_user_manager_runs_starts_the_window_in_a_scope_of_its_own() {
    assert_eq!(
        decide(Some(UNDER_MANAGER), UID, systemd_run(), random()),
        Start::Scope {
            systemd_run: PathBuf::from("/usr/bin/systemd-run"),
            unit: "app-aterm-0123456789abcdef.scope".to_owned(),
        }
    );
    let legacy = "12:pids:/user.slice/user-1000.slice/user@1000.service\n\
                  1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/x.service\n";
    assert!(
        matches!(
            decide(Some(legacy), UID, systemd_run(), random()),
            Start::Scope { .. }
        ),
        "a legacy hierarchy's name=systemd line names the manager too"
    );

    for (cgroup, what) in [
        (
            "0::/user.slice/user-1000.slice/session-3.scope\n",
            "a login session's scope",
        ),
        (
            "0::/user.slice/user-1001.slice/user@1001.service/app.slice/x.service\n",
            "another uid's manager",
        ),
        (
            "0::/user.slice/user-1000.slice/app-user@1000.service-x.scope\n",
            "a component that only contains the manager's name",
        ),
        ("0::/\n", "the root cgroup"),
        ("", "an empty cgroup file"),
    ] {
        assert!(
            matches!(
                decide(Some(cgroup), UID, systemd_run(), random()),
                Start::Child(_)
            ),
            "{what} starts the window as the node's own child"
        );
    }
    assert_eq!(
        decide(None, UID, systemd_run(), random()),
        Start::Child("no systemd user manager runs the node")
    );
    assert_eq!(
        decide(Some(UNDER_MANAGER), UID, None, random()),
        Start::Child("no systemd-run where the OS keeps it")
    );
    assert_eq!(
        decide(Some(UNDER_MANAGER), UID, systemd_run(), None),
        Start::Child("no random scope name could be drawn")
    );
}

fn args_of(command: &aterm_uds::spawnfd::Command) -> Vec<&OsStr> {
    command.get_args().collect()
}

fn removed_of(command: &aterm_uds::spawnfd::Command) -> Vec<&OsStr> {
    command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name)
        .collect()
}

/// A scope's command is `systemd-run --user --scope … --unit=<unit> -- <exe> --window`, so
/// `systemd-run` registers the scope and then execs the window in place; the node's own child is
/// `<exe> --window`. Both drop every one of the node unit's variables and nothing the window's
/// session needs, which they inherit untouched.
/// NEGATIVE CONTROLS: the child's command carries no `systemd-run` argument and the scope's names
/// its unit, so a builder that ignores `start` fails one of them; a session variable, the
/// launcher's descriptor limit or `PATH` among the removed ones fails the keep assertion, and a
/// unit variable left out of the removals fails the strip assertion.
#[test]
fn the_window_command_names_its_scope_and_drops_only_the_node_units_variables() {
    let exe = Path::new("/home/someone/.local/lib/aterm/bin/aterm");
    let window = [OsStr::new("--window")];
    let scope = Start::Scope {
        systemd_run: PathBuf::from("/usr/bin/systemd-run"),
        unit: "app-aterm-0123456789abcdef.scope".to_owned(),
    };
    let in_scope = command(&scope, exe, &window);
    assert_eq!(in_scope.get_program(), OsStr::new("/usr/bin/systemd-run"));
    assert_eq!(
        args_of(&in_scope),
        [
            "--user",
            "--scope",
            "--collect",
            "--quiet",
            "--slice=app.slice",
            "--description=aterm",
            "--unit=app-aterm-0123456789abcdef.scope",
            "--",
            "/home/someone/.local/lib/aterm/bin/aterm",
            "--window",
        ]
        .map(OsStr::new)
    );

    let child = command(
        &Start::Child("no systemd user manager runs the node"),
        exe,
        &window,
    );
    assert_eq!(child.get_program(), exe.as_os_str());
    assert_eq!(args_of(&child), [OsStr::new("--window")]);

    for built in [&in_scope, &child] {
        let removed = removed_of(built);
        for name in UNIT_VARIABLES {
            assert!(
                removed.contains(&OsStr::new(name)),
                "{name} is the node unit's, not the window's"
            );
        }
        for kept in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "XDG_SESSION_TYPE",
            "XDG_CURRENT_DESKTOP",
            "ATERM_NODE_LAUNCHER_NOFILE",
            "PATH",
            "HOME",
            "LANG",
            "SHELL",
            "TERM",
        ] {
            assert!(
                !removed.contains(&OsStr::new(kept)),
                "{kept} reaches the window"
            );
        }
    }
}

/// `systemd-run` is found as `execvp` would find a program, but only in absolute directories and
/// only as a regular file with an execute bit.
/// NEGATIVE CONTROL: the same `PATH` without the executable copy finds nothing, though a
/// non-executable file and a directory by that name sit earlier on it.
/// `systemd-run` is the OS's file only where it is a regular file with an execute bit: a
/// file without one, a folder by the name and a missing file are none.
#[test]
fn systemd_run_is_its_os_path_where_that_is_an_executable_file() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let make = |name: &str, mode: Option<u32>| {
        let file = scratch.path().join(name);
        match mode {
            Some(mode) => {
                std::fs::write(&file, b"").expect("file");
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))
                    .expect("mode");
            }
            None => std::fs::create_dir(&file).expect("a directory by the name"),
        }
        file
    };
    let program = make("program", Some(0o755));
    assert_eq!(program_at(&program), Some(program.clone()));
    for (control, file) in [
        ("a file with no execute bit", make("plain", Some(0o644))),
        ("a folder by the name", make("folder", None)),
        ("no file", scratch.path().join("missing")),
    ] {
        assert_eq!(program_at(&file), None, "CONTROL: {control}");
    }
}

/// The kernel's process name is the program's file name cut to 15 bytes.
#[test]
fn a_launchers_process_name_is_its_file_name_cut_to_fifteen_bytes() {
    assert_eq!(
        process_name(Path::new("/usr/bin/systemd-run")),
        "systemd-run"
    );
    assert_eq!(
        process_name(Path::new("/opt/bin/a-very-long-launcher-name")),
        "a-very-long-lau"
    );
}

/// A look at a running launcher is judged by its image, one at an ended launcher by its last
/// name: a running child's name may still be the forking thread's, which the kernel replaces only
/// after the spawn has returned, so a running child is never judged by its name.
/// NEGATIVE CONTROLS: each verdict has a row beside it that differs only in the image or the name
/// it read, so a judge that ignores either, or answers one verdict for a kind of look, fails one.
#[test]
fn a_running_launcher_is_judged_by_its_image_and_an_ended_one_by_its_last_name() {
    let launcher = Launcher {
        name: "systemd-run".to_owned(),
        image: Image { dev: 1, ino: 10 },
    };
    let window = Image { dev: 1, ino: 20 };
    for (look, verdict, what) in [
        (
            Look::Running(Some(window)),
            Verdict::Execed,
            "a running command",
        ),
        (
            Look::Running(Some(Image { dev: 2, ino: 10 })),
            Verdict::Execed,
            "the launcher's inode on another device",
        ),
        (
            Look::Running(Some(launcher.image)),
            Verdict::Launching,
            "the launcher still running",
        ),
        (
            Look::Running(None),
            Verdict::Launching,
            "a running child whose image could not be read",
        ),
        (
            Look::Ended(Some("aterm".to_owned())),
            Verdict::Execed,
            "a command that ran and ended",
        ),
        (
            Look::Ended(Some("systemd-run".to_owned())),
            Verdict::Ended,
            "a launcher that ended without execing",
        ),
        (
            Look::Ended(None),
            Verdict::Ended,
            "an ended child whose name could not be read",
        ),
    ] {
        assert_eq!(judge(&launcher, &look), verdict, "{what}");
    }
}

fn launcher(script: &str) -> aterm_uds::spawnfd::Child {
    let mut sh = aterm_uds::spawnfd::command("/bin/sh");
    sh.args(["-c", script]);
    sh.spawn().expect("a stand-in launcher starts")
}

fn sh() -> Launcher {
    Launcher::of(Path::new("/bin/sh")).expect("the stand-in launcher's file")
}

fn hang_detector() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

fn comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .expect("the child's name")
        .trim_end()
        .to_owned()
}

fn image_of(pid: u32) -> Image {
    Image::of(Path::new(&format!("/proc/{pid}/exe"))).expect("the child's image")
}

/// A launcher that execs its command in place hands the node the command's pid: `sh -c 'exec
/// sleep'` stands in for `systemd-run --scope`. The pid then runs the command's image (its name
/// may follow a moment later: the kernel sets the image first).
/// NEGATIVE CONTROL: the stand-in that never execs (`sleep` judged as its own launcher) is killed
/// at a deadline that has passed, and reaped.
#[test]
fn a_launcher_that_execs_in_place_hands_over_its_pid() {
    let mut child = launcher("exec /bin/sleep 600");
    assert_eq!(await_exec(&mut child, &sh(), hang_detector()), Ok(()));
    assert_eq!(
        image_of(child.id()),
        Image::of(Path::new("/bin/sleep")).expect("sleep's file"),
        "the pid now runs the command"
    );
    child.kill().expect("end the stand-in");
    child.wait().expect("reap the stand-in");

    let mut stuck = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("a launcher that never execs");
    let sleep = Launcher::of(Path::new("/bin/sleep")).expect("sleep's file");
    let why = await_exec(&mut stuck, &sleep, Instant::now()).expect_err("CONTROL: never execed");
    assert!(why.contains("killed"), "{why}");
    assert!(stuck.is_reaped(), "the killed launcher is reaped");
}

/// A launcher that runs under a name not its own (as a child does between its spawn's return and
/// the kernel's setting of its name, still named after the thread that forked it) has not execed
/// its command: it is killed at a deadline that has passed, not handed over as the window.
/// NEGATIVE CONTROL: its name is not the launcher's when it is judged, so the rule this one
/// replaced (any name but the launcher's is the command's) hands it over.
#[test]
fn a_running_launcher_under_another_name_has_not_execed() {
    let mut renamed = aterm_uds::spawnfd::command("/bin/sh");
    renamed
        .args(["-c", "printf renamed > /proc/$$/comm; read _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped());
    let mut renamed = renamed.spawn().expect("a stand-in launcher starts");
    let pid = renamed.id();
    let until = hang_detector();
    while comm(pid) != "renamed" {
        assert!(Instant::now() < until, "the stand-in never renamed itself");
        std::thread::sleep(super::LOOK_EVERY);
    }
    assert_ne!(
        comm(pid),
        sh().name,
        "CONTROL: a judge by name alone hands it over"
    );
    let why = await_exec(&mut renamed, &sh(), Instant::now()).expect_err("it never execed");
    assert!(why.contains("killed"), "{why}");
    assert!(renamed.is_reaped(), "the killed launcher is reaped");
}

/// A launcher that ends without execing (as `systemd-run` does when the user manager refuses or
/// cannot be reached) is reported with its exit, so the node starts the window itself; one that
/// execed a command which then failed at once is not, so the node starts no second window.
/// NEGATIVE CONTROL: the two exit non-zero alike, so a judge by exit status alone confuses them;
/// only the ended process's name tells them apart.
#[test]
fn a_launcher_that_ends_without_execing_is_refused_but_an_execed_command_that_fails_is_not() {
    let mut refused = launcher("exit 3");
    let why = await_exec(&mut refused, &sh(), hang_detector()).expect_err("never execed");
    assert!(why.contains("exit status: 3"), "{why}");
    assert!(refused.is_reaped());

    let mut ran = launcher("exec /bin/false");
    assert_eq!(await_exec(&mut ran, &sh(), hang_detector()), Ok(()));
    let status = ran.wait().expect("reap the command");
    assert_eq!(
        status.code(),
        Some(1),
        "CONTROL: it failed, as the refusal did"
    );
}

/// LIVE PROOF, run by hand inside a throwaway transient user service (never the node's unit):
/// starts `/bin/sleep 600` through [`super::start`], the node's own path, prints where it went,
/// and holds the service up for the hang detector's minute, in which it is stopped. A
/// `systemctl --user stop` of that service then leaves the sleep running in its own
/// `app-aterm-*.scope`.
#[test]
#[ignore = "live proof against this login's systemd user manager; run by hand in a transient service"]
fn live_proof_a_window_outlives_the_unit_that_started_it() {
    use std::io::Write as _;
    let started =
        super::start(Path::new("/bin/sleep"), &[OsStr::new("600")]).expect("the stand-in starts");
    println!(
        "STARTED pid={} home={:?}: {started}",
        started.pid, started.home
    );
    std::io::stdout().flush().expect("flush the proof line");
    std::thread::sleep(aterm_spec::wait::MUST_HAPPEN);
}

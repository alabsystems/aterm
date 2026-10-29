// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm caffeine`, aCaffiene's name before 0.145.0, which no build since has as a verb. Enable
//! at Startup under that name left a login job that runs `<aterm> caffeine serve` at every login
//! and again each time it fails, and the mug left its files under aterm's state directory. The
//! old verb now answers with the new name, and its `serve` lets go of the old mug's update bridge
//! and retires the job that ran it, exiting 0 so launchd does not start it again. [`tidy`], from
//! every aCaffiene autostart, retires the old login jobs whose program is gone or is this aterm,
//! and discards the old mug's copies of aterm. It leaves the mug's other files, a few bytes that
//! an old build writes without its lock, to `aterm pkg self-uninstall`.

use std::ffi::OsString;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::paths::Paths;

/// The old verb.
pub const VERB: &str = "caffeine";
/// The old installed app's label.
pub const INSTALLED_LABEL: &str = "com.aterm.aterm.caffeine";
/// The prefix of any other old build's label.
pub const DEV_LABEL_PREFIX: &str = "com.aterm.aterm.caffeine.dev";
/// What an old job runs after its program.
pub const SERVE_ARGS: [&str; 2] = [VERB, "serve"];

/// What `aterm caffeine` says of every command line but `serve`.
pub const RENAMED: &str =
    "aterm caffeine: `aterm caffeine` is now `aterm acaffiene`; run `aterm acaffiene --help`";

/// What `aterm caffeine serve` says first, on the old job's log; a line follows for each thing
/// that could not be done.
pub const RETIRED: &str = "aterm caffeine: serve: this login job runs `aterm caffeine`, which \
is `aterm acaffiene` since 0.145.0 and starts with every ordinary aterm window; it exits 0, so \
launchd does not start it again, and removes its login item";

/// Whether `text` is a label an old build made ([`crate::launchd::is_label`]'s grammar under the
/// old name).
#[must_use]
pub fn is_label(text: &str) -> bool {
    crate::launchd::label_of(text, INSTALLED_LABEL, DEV_LABEL_PREFIX)
}

/// Whether the words after `caffeine` are `serve`, what an old job runs.
#[must_use]
pub fn is_serve(args: &[OsString]) -> bool {
    matches!(args, [word] if word == "serve")
}

/// The old verb's entry: `args` are the words after `caffeine`. `serve` lets go of the old mug's
/// update bridge and retires the job that ran it (on macOS), and exits 0, as an old mug's
/// refusals did, so launchd's KeepAlive does not start it again; anything else names the new
/// verb and exits 2.
#[must_use]
pub fn main_entry(args: &[OsString]) -> ExitCode {
    if !is_serve(args) {
        eprintln!("{RENAMED}");
        return ExitCode::from(2);
    }
    eprintln!("{RETIRED}");
    #[cfg(target_os = "macos")]
    retire_own_job();
    ExitCode::SUCCESS
}

/// [`serve_retires`] for this login and this aterm, the program of the job this process is.
#[cfg(target_os = "macos")]
fn retire_own_job() {
    let paths = match crate::cli::login_paths() {
        Ok(paths) => paths,
        Err(why) => {
            eprintln!("aterm caffeine: serve: {why}");
            return;
        }
    };
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    for line in serve_retires(&paths, exe, crate::launchd::unload_idle) {
        eprintln!("aterm caffeine: serve: {line}");
    }
}

/// What the old `serve` does under this login's `paths`. It lets go of the old mug's update
/// bridge, then retires the old login jobs for `exe`, this aterm ([`retire_jobs`], unloading
/// each with `unload`). An old mug that ends for an update restart leaves a bridge keeping the
/// Mac awake until its successor lets it go, or for at most 21 minutes, and that successor is
/// now this. It takes no lock: an old build's mug that finds its lock held by a process
/// whose command line is `caffeine serve`, with no label in the file, reads it as a running
/// mug and gives up its start for the whole login. What could not be done, one line each.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn serve_retires(
    paths: &Paths,
    exe: std::io::Result<PathBuf>,
    unload: impl Fn(&str),
) -> Vec<String> {
    let bridge = bridge_file(paths);
    let mut failed = match std::fs::remove_file(&bridge) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            vec![format!("{}: {e}", bridge.display())]
        }
        _ => Vec::new(),
    };
    match exe {
        Ok(exe) => failed.extend(retire_jobs_with(paths, &exe, unload)),
        Err(e) => failed.push(format!("this executable's path cannot be read: {e}")),
    }
    failed
}

/// The old mug's mode, which an old build reads and writes without its lock.
#[must_use]
pub fn state_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("caffeine.state")
}

/// The folder of the old mug's session-only launchd plists.
#[must_use]
pub fn session_dir(paths: &Paths) -> PathBuf {
    paths.state_dir.join("caffeine")
}

/// The old mug's update bridge's pid file: a bridge keeps the Mac awake while it names it.
#[must_use]
pub fn bridge_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("caffeine.bridge")
}

/// The old mug's lock, which stays: an old build's mug may still take it, and one that opened it
/// before a removal would hold a lock nobody else can find.
#[must_use]
pub fn lock_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("caffeine.lock")
}

/// The old mug's copies of aterm.
#[must_use]
pub fn code_dir(paths: &Paths) -> PathBuf {
    paths.state_dir.join("caffeine-code.noindex")
}

/// The old login jobs no aterm will run again: those whose program is gone
/// ([`crate::launchd::program_gone`]) or is `exe`, an aterm without the old verb.
#[cfg(unix)]
#[must_use]
pub fn retired_jobs(paths: &Paths, exe: &Path) -> Vec<(String, PathBuf)> {
    crate::launchd::login_jobs(paths, is_label, &SERVE_ARGS, |program| {
        crate::launchd::program_gone(program) || same_file(program, exe)
    })
}

/// Whether `a` and `b` resolve to one file.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Retire the old login jobs [`retired_jobs`] finds for `exe`, this aterm
/// ([`crate::launchd::retire`]). What could not be removed, one line each.
#[cfg(unix)]
#[must_use]
pub fn retire_jobs(paths: &Paths, exe: &Path) -> Vec<String> {
    retire_jobs_with(paths, exe, crate::launchd::unload_idle)
}

/// [`retire_jobs`], unloading each retired job with `unload`.
#[cfg(unix)]
fn retire_jobs_with(paths: &Paths, exe: &Path, unload: impl Fn(&str)) -> Vec<String> {
    crate::launchd::retire_with(retired_jobs(paths, exe), unload)
}

/// [`retire_jobs`] for `exe`, this aterm, then [`discard_copies`]. What could not be removed, and
/// a copy kept, one line each.
#[cfg(unix)]
#[must_use]
pub fn tidy(paths: &Paths, exe: &Path) -> Vec<String> {
    tidy_with(paths, exe, crate::launchd::unload_idle)
}

/// [`tidy`], unloading each retired job with `unload`.
#[cfg(unix)]
fn tidy_with(paths: &Paths, exe: &Path, unload: impl Fn(&str)) -> Vec<String> {
    let mut failed = retire_jobs_with(paths, exe, unload);
    failed.extend(discard_copies(paths));
    failed
}

/// Remove the old mug's copies of aterm ([`crate::code::discard_copies`], which keeps one macOS
/// may still be assessing) while holding its lock, which an old mug running from a copy, or
/// making one, holds: while it does, they all stay. An old mug between its runs needs none of
/// them, as each run that uses a copy makes its own. The hold writes nothing into the lock, whose
/// text it leaves as it was, so a mug from 0.108.0 on that finds it held reads no running mug and
/// waits up to 2 s for it; one from 0.107.0 tries twice without waiting and gives up its start,
/// as it does against any holder that has not yet written its pid. Nothing is opened when there
/// are no copies. What could not be removed, one line, or each copy kept ([`kept_line`]).
#[cfg(unix)]
#[must_use]
pub fn discard_copies(paths: &Paths) -> Vec<String> {
    let code = code_dir(paths);
    if std::fs::symlink_metadata(&code).is_err() {
        return Vec::new();
    }
    let lock = lock_file(paths);
    let held = match crate::instance::Instance::try_hold_unnamed(&lock) {
        Ok(Some(held)) => held,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![format!("{}: {e}", lock.display())],
    };
    let said = match crate::code::discard_copies(&code) {
        Ok(kept) => kept.0.iter().map(|copy| kept_line(copy)).collect(),
        Err(e) => vec![format!("{}: {e}", code.display())],
    };
    drop(held);
    said
}

/// What [`tidy`] says of an old mug's copy that [`discard_copies`] kept: no `aterm acaffiene`
/// verb reaches the old name's copies, and the tidy of aterm's next ordinary launch does.
#[cfg(unix)]
fn kept_line(copy: &Path) -> String {
    format!(
        "kept {}: no old mug said it ran from it, so macOS may still be assessing its first \
         exec; aterm's next launch removes it once the Mac has been awake {} minutes since",
        copy.display(),
        aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_label_the_old_name_made_is_one() {
        assert!(is_label(INSTALLED_LABEL));
        assert!(is_label("com.aterm.aterm.caffeine.dev.0123abcd"));
        for bad in [
            "",
            crate::launchd::INSTALLED_LABEL,
            "com.aterm.aterm.acaffiene.dev.0123abcd",
            "com.aterm.aterm.caffeine.dev",
            "com.aterm.aterm.caffeine.dev.0123ABCD",
            "com.aterm.aterm.caffeine.dev.0123abc",
            "com.aterm.aterm.caffeine.x",
        ] {
            assert!(!is_label(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_serve_alone_is_the_old_jobs_command() {
        let words = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(is_serve(&words(&["serve"])));
        for other in [
            &[][..],
            &["status"],
            &["serve", "x"],
            &["bridge"],
            &["--help"],
        ] {
            assert!(!is_serve(&words(other)), "{other:?}");
        }
    }

    #[cfg(unix)]
    fn scratch(root: &Path) -> Paths {
        Paths {
            state_dir: root.join("state"),
            home: root.join("home"),
            logs_dir: root.join("logs"),
        }
    }

    /// An old build's login plist for `label`, running `program caffeine serve`.
    #[cfg(unix)]
    fn plant(paths: &Paths, label: &str, program: &Path) -> PathBuf {
        let text = crate::launchd::plist_text(label, program, &paths.log_file(), None)
            .replace("<string>acaffiene</string>", "<string>caffeine</string>");
        assert!(text.contains("<string>caffeine</string>"), "{text}");
        let plist = paths.startup_plist(label);
        std::fs::create_dir_all(paths.launch_agents()).expect("agents");
        std::fs::write(&plist, text).expect("plist");
        plist
    }

    #[test]
    #[cfg(unix)]
    fn an_old_login_job_retires_when_its_program_is_gone_or_this_aterm() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(root.path());
        let exe = root.path().join("aterm");
        std::fs::write(&exe, "this aterm").expect("exe");
        let link = root.path().join("aterm-link");
        std::os::unix::fs::symlink(&exe, &link).expect("symlink");
        let other = root.path().join("other-aterm");
        std::fs::write(&other, "another build").expect("exe");
        let gone = root.path().join("removed-worktree/aterm");
        assert_eq!(retired_jobs(&paths, &exe), [], "no LaunchAgents at all");

        let mine = plant(&paths, INSTALLED_LABEL, &exe);
        let linked = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &link);
        let orphan = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &gone);
        plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
        plant(
            &paths,
            "com.aterm.aterm.caffeine.dev.0000000d",
            Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
        );
        plant(&paths, "com.example.other", &exe);
        let new_name = crate::launchd::plist_text(
            crate::launchd::INSTALLED_LABEL,
            &exe,
            &paths.log_file(),
            None,
        );
        std::fs::write(
            paths.startup_plist(crate::launchd::INSTALLED_LABEL),
            new_name,
        )
        .expect("plist");
        let status = paths.startup_plist("com.aterm.aterm.caffeine.dev.0000000e");
        std::fs::write(
            &status,
            std::fs::read_to_string(&mine)
                .expect("plist")
                .replace("<string>serve</string>", "<string>status</string>"),
        )
        .expect("plist");
        let planted = paths.startup_plist("com.aterm.aterm.caffeine.dev.0000000f");
        std::os::unix::fs::symlink(&mine, &planted).expect("symlink");

        assert_eq!(
            retired_jobs(&paths, &exe),
            [
                (INSTALLED_LABEL.to_string(), mine),
                ("com.aterm.aterm.caffeine.dev.0000000a".to_string(), linked),
                ("com.aterm.aterm.caffeine.dev.0000000b".to_string(), orphan),
            ],
            "another build's job, one on a volume, the new name's, a status job and a link stay"
        );
    }

    /// A copy of aterm in the old code folder: one a mug said it ran from, or one macOS may still
    /// be assessing (new, with no such word).
    #[cfg(unix)]
    fn copy(paths: &Paths, name: &str, ran: bool) -> PathBuf {
        let copy = code_dir(paths).join("l").join(name);
        std::fs::create_dir_all(copy.join("aterm.app")).expect("copy");
        if ran {
            std::fs::write(copy.join(aterm_uds::deadline::RAN), "").expect("ran");
        }
        copy
    }

    #[test]
    #[cfg(unix)]
    fn tidy_retires_the_old_jobs_and_discards_the_old_copies() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(root.path());
        let exe = root.path().join("aterm");
        std::fs::write(&exe, "this aterm").expect("exe");
        let other = root.path().join("other-aterm");
        std::fs::write(&other, "another build").expect("exe");
        let mine = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &exe);
        let orphan = plant(
            &paths,
            "com.aterm.aterm.caffeine.dev.0000000b",
            &root.path().join("gone/aterm"),
        );
        let kept = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
        let ran = copy(&paths, "1-1", true);
        let state = state_file(&paths);
        std::fs::write(&state, "mode=on\n").expect("state");

        let unloaded = std::cell::RefCell::new(Vec::new());
        let failed = tidy_with(&paths, &exe, |label| {
            unloaded.borrow_mut().push(label.to_string());
        });
        assert_eq!(failed, Vec::<String>::new());
        assert!(!mine.exists() && !orphan.exists(), "both retired");
        assert!(kept.is_file(), "another build's job stays");
        assert_eq!(
            unloaded.into_inner(),
            [
                "com.aterm.aterm.caffeine.dev.0000000a",
                "com.aterm.aterm.caffeine.dev.0000000b"
            ],
            "each retired job, and no other, is unloaded"
        );
        assert!(!ran.exists() && !code_dir(&paths).exists(), "the copies go");
        assert!(state.is_file(), "an old build writes it without the lock");
        assert_eq!(std::fs::read(lock_file(&paths)).expect("lock"), b"");
    }

    #[test]
    #[cfg(unix)]
    fn the_old_serve_lets_the_bridge_go_and_retires_its_job_taking_no_lock() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(root.path());
        let exe = root.path().join("aterm");
        std::fs::write(&exe, "this aterm").expect("exe");
        let other = root.path().join("other-aterm");
        std::fs::write(&other, "another build").expect("exe");
        let mine = plant(&paths, INSTALLED_LABEL, &exe);
        let kept = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
        let ran = copy(&paths, "1-1", true);
        let bridge = bridge_file(&paths);
        std::fs::write(&bridge, "4242\n").expect("a bridge left by a restart");

        let unloaded = std::cell::RefCell::new(Vec::new());
        let failed = serve_retires(&paths, Ok(exe.clone()), |label| {
            unloaded.borrow_mut().push(label.to_string());
        });
        assert_eq!(failed, Vec::<String>::new());
        assert!(
            !bridge.exists(),
            "the bridge sees its file gone and lets go"
        );
        assert!(!mine.exists() && kept.is_file(), "its own job retires");
        assert_eq!(unloaded.into_inner(), [INSTALLED_LABEL]);
        assert!(ran.is_dir(), "the copies are the window's to discard");
        assert!(!lock_file(&paths).exists(), "no lock is opened");

        std::fs::write(&bridge, "4242\n").expect("bridge");
        let mine = plant(&paths, INSTALLED_LABEL, &exe);
        let failed = serve_retires(&paths, Err(std::io::Error::other("no path")), |_| {
            panic!("nothing is retired without this aterm's path")
        });
        assert_eq!(failed, ["this executable's path cannot be read: no path"]);
        assert!(!bridge.exists(), "the bridge goes first");
        assert!(mine.is_file());
        assert!(!lock_file(&paths).exists(), "no lock is opened");
    }

    #[test]
    #[cfg(unix)]
    fn the_old_copies_go_only_while_no_old_mug_holds_the_lock() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(root.path());
        let lock = lock_file(&paths);
        assert_eq!(discard_copies(&paths), Vec::<String>::new());
        assert!(!lock.exists(), "no copies opens no lock");

        let ran = copy(&paths, "1-1", true);
        let held = crate::instance::Instance::try_hold(&lock)
            .expect("lock")
            .expect("free");
        assert_eq!(discard_copies(&paths), Vec::<String>::new());
        assert!(ran.is_dir(), "an old mug holds the lock");
        drop(held);

        let ended = format!("4242\n{INSTALLED_LABEL}\n");
        std::fs::write(&lock, &ended).expect("a mug that ended for an update restart");
        let young = copy(&paths, "1-2", false);
        assert_eq!(
            discard_copies(&paths),
            [kept_line(&young)],
            "a copy kept is said, as aterm's next launch removes it"
        );
        assert!(!kept_line(&young).contains("quit"), "no verb removes it");
        assert!(!ran.exists(), "a copy a mug ran from goes");
        assert!(young.is_dir(), "a copy macOS may still be assessing stays");
        assert_eq!(
            std::fs::read_to_string(&lock).expect("lock"),
            ended,
            "the lock stays, and the hold left its text as it was"
        );
    }
}

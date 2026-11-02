// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm caffeine`, aCaffiene's name before 0.145.0, which no build since has as a verb. Enable
//! at Startup under that name left a login job that runs `<aterm> caffeine serve` at every login
//! and again each time it fails, and the mug left its files under aterm's state directory. The
//! old verb now answers with the new name, and its `serve` lets go of the old mug's update bridge
//! and retires the job that ran it, exiting 0 so launchd does not start it again. A job whose
//! program is an aterm from before that answer (0.145.0) exits 2 every time instead, so the old
//! login jobs no aterm will run again ([`retired_jobs`]) are those whose program is gone, is this
//! aterm, or is another aterm that says it has no old verb. [`tidy`], once a process from the
//! first pass of an ordinary window's keep-awake watch that finds this login's paths
//! ([`crate::cli::keep_awake`]), retires those jobs and discards the old mug's copies of aterm;
//! the converge pass's keep-awake ([`crate::cli::keep_installed_awake`]) retires the jobs alone.
//! It leaves the mug's other files, a few bytes that an old build writes without its lock, to
//! `aterm pkg self-uninstall`.

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
    for line in serve_retires(&paths, exe, has_old_verb, crate::launchd::unload_idle) {
        eprintln!("aterm caffeine: serve: {line}");
    }
}

/// What the old `serve` does under this login's `paths`. It lets go of the old mug's update
/// bridge, then retires the old login jobs for `exe`, this aterm ([`retire_jobs`], asking another
/// program whether it has the old verb with `has_verb` and unloading each with `unload`). An old
/// mug that ends for an update restart leaves a bridge keeping the Mac awake until its successor
/// lets it go, or for at most 21 minutes, and that successor is now this. It takes no lock: an
/// old build's mug that finds its lock held by a process whose command line is `caffeine serve`,
/// with no label in the file, reads it as a running mug and gives up its start for the whole
/// login. What could not be done, one line each.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn serve_retires(
    paths: &Paths,
    exe: std::io::Result<PathBuf>,
    has_verb: impl Fn(&Path) -> Option<bool>,
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
        Ok(exe) => failed.extend(retire_jobs_with(paths, &exe, has_verb, unload)),
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
/// ([`crate::launchd::program_gone`]), is `exe`, an aterm without the old verb, or is another
/// program that answers no to `help caffeine` ([`has_old_verb`]). That one has no old verb
/// either: its job exits 2 and is started again about every ten seconds (0.145.0, which knows
/// neither the old verb nor its `serve`), or retires itself when it runs (0.146.0 on), so it goes
/// now either way. A program that still has the old verb (0.144.0 and before) keeps its job,
/// which is its own mug, and so does one that gives no answer.
#[cfg(unix)]
#[must_use]
pub fn retired_jobs(paths: &Paths, exe: &Path) -> Vec<(String, PathBuf)> {
    retired_jobs_with(paths, exe, has_old_verb)
}

/// [`retired_jobs`], asking another program whether it has the old verb with `has_verb`.
#[cfg(unix)]
pub(crate) fn retired_jobs_with(
    paths: &Paths,
    exe: &Path,
    has_verb: impl Fn(&Path) -> Option<bool>,
) -> Vec<(String, PathBuf)> {
    crate::launchd::login_jobs(paths, is_label, &SERVE_ARGS, |program| {
        crate::launchd::program_gone(program)
            || same_file(program, exe)
            || has_verb(program) == Some(false)
    })
}

/// Whether the executable `program` still has the old verb: every release from 0.107.0 to
/// 0.144.0 answers `help caffeine` ([`old_verb_probe_args`]) with 0, and every later one with an
/// error ([`crate::code::answers`]). `None` when it gave no answer.
#[cfg(unix)]
pub(crate) fn has_old_verb(program: &Path) -> Option<bool> {
    crate::code::answers(program, old_verb_probe_args())
}

/// What [`has_old_verb`] runs another program with: `help caffeine`. 0.145.0 has the
/// `acaffiene` topic too, so a probe of that one would keep its job, which can only fail.
#[cfg(unix)]
const fn old_verb_probe_args() -> [&'static str; 2] {
    ["help", VERB]
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
    retire_jobs_with(paths, exe, has_old_verb, crate::launchd::unload_idle)
}

/// [`retire_jobs`], asking another program whether it has the old verb with `has_verb` and
/// unloading each retired job with `unload`.
#[cfg(unix)]
pub(crate) fn retire_jobs_with(
    paths: &Paths,
    exe: &Path,
    has_verb: impl Fn(&Path) -> Option<bool>,
    unload: impl Fn(&str),
) -> Vec<String> {
    crate::launchd::retire_with(retired_jobs_with(paths, exe, has_verb), unload)
}

/// [`retire_jobs`] for `exe`, this aterm, then [`discard_copies`]. What could not be removed, and
/// a copy kept, one line each.
#[cfg(unix)]
#[must_use]
pub fn tidy(paths: &Paths, exe: &Path) -> Vec<String> {
    tidy_with(paths, exe, has_old_verb, crate::launchd::unload_idle)
}

/// [`tidy`], asking another program whether it has the old verb with `has_verb` and unloading
/// each retired job with `unload`.
#[cfg(unix)]
fn tidy_with(
    paths: &Paths,
    exe: &Path,
    has_verb: impl Fn(&Path) -> Option<bool>,
    unload: impl Fn(&str),
) -> Vec<String> {
    let mut failed = retire_jobs_with(paths, exe, has_verb, unload);
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
        let ejected = Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm");
        let asked = std::cell::RefCell::new(Vec::new());
        let has_verb = |program: &Path| {
            asked.borrow_mut().push(program.to_path_buf());
            (program == other).then_some(true)
        };
        assert_eq!(
            retired_jobs_with(&paths, &exe, has_verb),
            [],
            "no LaunchAgents at all"
        );

        let mine = plant(&paths, INSTALLED_LABEL, &exe);
        let linked = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &link);
        let orphan = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &gone);
        plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
        plant(&paths, "com.aterm.aterm.caffeine.dev.0000000d", ejected);
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
            retired_jobs_with(&paths, &exe, has_verb),
            [
                (INSTALLED_LABEL.to_string(), mine),
                ("com.aterm.aterm.caffeine.dev.0000000a".to_string(), linked),
                ("com.aterm.aterm.caffeine.dev.0000000b".to_string(), orphan),
            ],
            "the job of another build with the old verb, one on a volume, the new name's, a \
             status job and a link stay"
        );
        let mut asked = asked.into_inner();
        asked.sort();
        let mut others = vec![ejected.to_path_buf(), other];
        others.sort();
        assert_eq!(
            asked, others,
            "only another program is asked, never this aterm or one that is gone"
        );
    }

    /// An old login job whose program is another aterm that says it has no old verb (0.145.0,
    /// whose job exits 2 and is started again every ten seconds) retires. One whose program still
    /// has the old verb (0.144.0's own mug) stays, and so does one whose program gives no answer.
    #[test]
    #[cfg(unix)]
    fn an_old_login_job_of_another_aterm_without_the_old_verb_retires() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(root.path());
        let exe = root.path().join("aterm");
        std::fs::write(&exe, "this aterm").expect("exe");
        let program = |name: &str| {
            let program = root.path().join(name).join("aterm");
            std::fs::create_dir_all(program.parent().expect("a folder")).expect("mkdir");
            std::fs::write(&program, name).expect("exe");
            program
        };
        let no_verb = program("0.145.0");
        let old_verb = program("0.144.0");
        let silent = program("hung");
        let retired = plant(&paths, INSTALLED_LABEL, &no_verb);
        let mug = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &old_verb);
        let unanswered = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &silent);
        let has_verb = |program: &Path| {
            if program == no_verb {
                Some(false)
            } else if program == old_verb {
                Some(true)
            } else {
                None
            }
        };
        assert_eq!(
            retired_jobs_with(&paths, &exe, has_verb),
            [(INSTALLED_LABEL.to_string(), retired.clone())]
        );

        let unloaded = std::cell::RefCell::new(Vec::new());
        let failed = tidy_with(&paths, &exe, has_verb, |label| {
            unloaded.borrow_mut().push(label.to_string());
        });
        assert_eq!(failed, Vec::<String>::new());
        assert!(!retired.exists(), "the job that can only fail goes");
        assert!(mug.is_file(), "an old build's own mug stays");
        assert!(unanswered.is_file(), "no answer keeps it");
        assert_eq!(unloaded.into_inner(), [INSTALLED_LABEL]);
    }

    /// The live probe asks the old name's help topic: 0.145.0 answers yes to `help acaffiene`,
    /// so a probe of the new name would keep the job that exits 2 every ten seconds.
    #[cfg(unix)]
    #[test]
    fn the_old_verb_probe_asks_help_caffeine() {
        assert_eq!(old_verb_probe_args(), ["help", "caffeine"]);
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
        let has_verb = |program: &Path| {
            assert_eq!(program, other, "only another program is asked");
            Some(true)
        };
        let failed = tidy_with(&paths, &exe, has_verb, |label| {
            unloaded.borrow_mut().push(label.to_string());
        });
        assert_eq!(failed, Vec::<String>::new());
        assert!(!mine.exists() && !orphan.exists(), "both retired");
        assert!(
            kept.is_file(),
            "another build with the old verb keeps its job"
        );
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
        let has_verb = |program: &Path| (program == other).then_some(true);
        let failed = serve_retires(&paths, Ok(exe.clone()), has_verb, |label| {
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
        let failed = serve_retires(
            &paths,
            Err(std::io::Error::other("no path")),
            |_| panic!("nothing is asked without this aterm's path"),
            |_| panic!("nothing is retired without this aterm's path"),
        );
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

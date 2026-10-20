// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mug's launchd job, in the user's GUI domain: its label, its plist, and the
//! `launchctl` calls that start it and read it. Enable at Startup is the plist in
//! `~/Library/LaunchAgents` (launchd loads it at every login); `aterm caffeine` without it
//! bootstraps the same job from [`crate::paths::Paths::session_plist`], for this login only.
//!
//! The job runs `<origin> caffeine serve` with `RunAtLoad` and `KeepAlive` on an
//! unsuccessful exit only: Quit (exit 0) ends it, a crash or the update restart
//! ([`EXIT_RESTART`]) starts it again. A signal is an unsuccessful exit too, so a job is
//! ended from outside with `launchctl bootout` ([`bootout`]), never a bare `kill`.
//!
//! The argv builders and the plist text are pure; the calls run `/bin/launchctl` through
//! `aterm_uds::spawnfd::command`, its stdin `/dev/null` and its output read, under a hang
//! detector. No test runs `launchctl`.

use std::io;
use std::path::Path;

use crate::paths::Paths;

/// The installed app's label.
pub const INSTALLED_LABEL: &str = "com.aterm.aterm.caffeine";
/// The prefix of any other build's label: `<prefix>.<hash of its executable's path>`.
pub const DEV_LABEL_PREFIX: &str = "com.aterm.aterm.caffeine.dev";
/// The installed app's executable.
pub const INSTALLED_EXECUTABLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
/// What the job runs after its program: `caffeine serve`.
pub const SERVE_ARGS: [&str; 2] = ["caffeine", "serve"];
/// The exit status `serve` ends with when the code it was started from changed (an update),
/// so launchd starts it again from the new code.
pub const EXIT_RESTART: u8 = 75;

/// The launchctl binary.
#[cfg(unix)]
const LAUNCHCTL: &str = "/bin/launchctl";

/// How long one `launchctl` call may take before it is called hung, killed and refused.
#[cfg(unix)]
const LAUNCHCTL_HANG: std::time::Duration = std::time::Duration::from_secs(30);

/// The label of the job for executable `exe`: [`INSTALLED_LABEL`] for
/// [`INSTALLED_EXECUTABLE`], else a per-path one, so a dev build never takes the installed
/// app's job.
#[must_use]
pub fn label_for(exe: &Path) -> String {
    if exe == Path::new(INSTALLED_EXECUTABLE) {
        return INSTALLED_LABEL.to_string();
    }
    let hash = aterm_hash::fnv1a_64(exe.as_os_str().as_encoded_bytes());
    format!(
        "{DEV_LABEL_PREFIX}.{:08x}",
        (hash >> 32) as u32 ^ hash as u32
    )
}

/// Whether `text` is a label [`label_for`] makes: [`INSTALLED_LABEL`], or the dev prefix and
/// eight lower-case hex digits.
#[must_use]
pub fn is_label(text: &str) -> bool {
    text == INSTALLED_LABEL
        || text
            .strip_prefix(DEV_LABEL_PREFIX)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|hash| {
                hash.len() == 8
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}

/// `text` as XML character data: the five predefined entities, so any path is a well-formed
/// `<string>`.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The job's plist: `label` running `program caffeine serve`, stderr to `log`, attributed to
/// the app `bundle_id` names in Login Items when known.
///
/// `KeepAlive` is `{ SuccessfulExit = false }`, never `true`: an exit of 0 (Quit) ends the
/// job, anything else (a crash, [`EXIT_RESTART`]) starts it again. `ProcessType Interactive`
/// and `LimitLoadToSessionType Aqua`: a menu-bar app, in a GUI login only. A path that is not
/// UTF-8 is written lossily; the callers that write a plist refuse one first.
#[must_use]
pub fn plist_text(label: &str, program: &Path, log: &Path, bundle_id: Option<&str>) -> String {
    let string = |s: &str| format!("<string>{}</string>", xml_escape(s));
    let mut args = String::new();
    for arg in std::iter::once(program.to_string_lossy().as_ref()).chain(SERVE_ARGS) {
        args.push_str(&format!("\t\t{}\n", string(arg)));
    }
    let associated = bundle_id.map_or_else(String::new, |id| {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            string(id)
        )
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{label}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {args}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Interactive</string>\n\
         \t<key>LimitLoadToSessionType</key>\n\
         \t<string>Aqua</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t{log}\n\
         {associated}\
         </dict>\n\
         </plist>\n",
        label = string(label),
        log = string(&log.to_string_lossy()),
    )
}

/// The bundle identifier of the app `program` is the executable of, for the plist's
/// `AssociatedBundleIdentifiers`.
#[cfg(unix)]
fn bundle_id_of(program: &Path) -> Option<String> {
    crate::code::own_bundle(program).and_then(|b| crate::code::bundle_identifier(&b))
}

/// Whether Enable at Startup is on for `label`: its plist is in `~/Library/LaunchAgents`.
#[must_use]
pub fn startup_enabled(paths: &Paths, label: &str) -> bool {
    paths.startup_plist(label).is_file()
}

/// Turn Enable at Startup on (write the login plist for `label` running `program`) or off
/// (remove it, and [`prune_orphans`]). The running job is left alone either way.
///
/// On, `~/Library/LaunchAgents` is made when missing and the plist written whole (`0644`,
/// a new file renamed over the old), naming the bundle `program` belongs to; the logs
/// directory is made too, for launchd to open the job's stderr in at the next login. Off, a
/// plist already gone is fine.
///
/// # Errors
///
/// The plist cannot be written or removed, or `program` is not a UTF-8 path.
#[cfg(unix)]
pub fn set_startup(paths: &Paths, label: &str, program: &Path, on: bool) -> io::Result<()> {
    let plist = paths.startup_plist(label);
    if !on {
        let removed = match std::fs::remove_file(&plist) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        prune_orphans(paths);
        return removed;
    }
    utf8(program)?;
    let _ = std::fs::create_dir_all(&paths.logs_dir);
    let text = plist_text(
        label,
        program,
        &paths.log_file(),
        bundle_id_of(program).as_deref(),
    );
    aterm_uds::atomic_file::write(&plist, text.as_bytes(), 0o644)
}

/// Enable at Startup: not wired off Unix.
///
/// # Errors
///
/// Always: launchd is macOS.
#[cfg(not(unix))]
pub fn set_startup(paths: &Paths, label: &str, program: &Path, on: bool) -> io::Result<()> {
    let _ = (paths, label, program, on);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launchd is macOS only",
    ))
}

/// `Ok` when `path` is UTF-8 (what a plist can name).
#[cfg(unix)]
fn utf8(path: &Path) -> io::Result<()> {
    if path.to_str().is_some() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a UTF-8 path", path.display()),
        ))
    }
}

/// The login plists of a mug whose program is gone, as `(label, plist)`: each regular file
/// `~/Library/LaunchAgents/<label>.plist` for a label [`is_label`] accepts whose
/// `ProgramArguments` are `<program> caffeine serve` with nothing at `<program>` any more (the
/// app was deleted or moved, the worktree a dev build ran from removed). Only `NotFound`
/// establishes absence; other metadata errors preserve the plist. A program under `/Volumes`
/// is never gone: an unmounted disk is not a deleted app. In name order.
#[must_use]
pub fn orphans(paths: &Paths) -> Vec<(String, std::path::PathBuf)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(paths.launch_agents())
        .into_iter()
        .flatten()
    {
        let Ok(entry) = entry else { continue };
        let plist = entry.path();
        let Some(label) = plist
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".plist"))
            .filter(|label| is_label(label))
            .map(str::to_string)
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(args) = crate::code::read_plist(&plist)
            .and_then(|text| crate::code::plist_strings(&text, "ProgramArguments"))
        else {
            continue;
        };
        let [program, rest @ ..] = args.as_slice() else {
            continue;
        };
        let program = Path::new(program);
        if rest == SERVE_ARGS
            && program.is_absolute()
            && !program.starts_with("/Volumes")
            && std::fs::symlink_metadata(program)
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            found.push((label, plist));
        }
    }
    found.sort();
    found
}

/// Remove every [`orphans`] plist, and unload its job unless it runs a mug now (from its copy,
/// say): launchd would only fail to start the missing program again. A running one is left
/// to run; with its plist gone, the next login does not load it. Best effort: what cannot be
/// removed waits for the next prune.
#[cfg(unix)]
pub fn prune_orphans(paths: &Paths) {
    for (label, plist) in orphans(paths) {
        let _ = std::fs::remove_file(&plist);
        if matches!(job_state(&label), Ok(JobState::Loaded { pid: None })) {
            let _ = bootout(&label);
        }
    }
}

/// Orphaned login plists: none off Unix.
#[cfg(not(unix))]
pub fn prune_orphans(_paths: &Paths) {}

/// The user's GUI domain target for `uid`, `gui/<uid>`.
#[must_use]
pub fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// `launchctl print <domain>/<label>`: the job's state, read-only.
#[must_use]
pub fn print_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["print".to_string(), format!("{domain}/{label}")]
}

/// `launchctl bootstrap <domain> <plist>`: load the job `plist` defines, and with
/// `RunAtLoad`, start it.
#[must_use]
pub fn bootstrap_argv(domain: &str, plist: &Path) -> Vec<String> {
    vec![
        "bootstrap".to_string(),
        domain.to_string(),
        plist.to_string_lossy().into_owned(),
    ]
}

/// `launchctl kickstart <domain>/<label>`: start a loaded job that is not running (no `-k`:
/// a running one is left as it is).
#[must_use]
pub fn kickstart_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["kickstart".to_string(), format!("{domain}/{label}")]
}

/// `launchctl bootout <domain>/<label>`: end the job (launchd sends it SIGTERM) and unload it
/// for this login; a startup plist still loads it at the next.
#[must_use]
pub fn bootout_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["bootout".to_string(), format!("{domain}/{label}")]
}

/// What launchd says of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// launchd knows no such job in the GUI domain.
    Absent,
    /// It is loaded; `pid` when it is running now.
    Loaded {
        /// The job's process, when one runs.
        pid: Option<u32>,
    },
}

/// The running pid `launchctl print`'s text names: the service's own `pid = <n>` line (one
/// tab deep; a nested section's is not it).
#[must_use]
pub fn print_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let top = line.strip_prefix('\t')?;
        top.strip_prefix("pid = ")?
            .trim_end()
            .parse()
            .ok()
            .filter(|pid| *pid > 0)
    })
}

/// How [`ensure_running`] got the mug running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The job already ran.
    AlreadyRunning {
        /// Its process.
        pid: u32,
    },
    /// The job was loaded but not running; it was kickstarted.
    Kicked,
    /// The job was not loaded; it was bootstrapped (and launchd starts it).
    Bootstrapped,
}

/// One `launchctl` call: its stdin `/dev/null`, its stdout and stderr read, under
/// [`LAUNCHCTL_HANG`].
#[cfg(unix)]
fn launchctl(argv: &[String]) -> io::Result<std::process::Output> {
    let mut cmd = aterm_uds::spawnfd::command(LAUNCHCTL);
    cmd.args(argv);
    aterm_uds::deadline::output_until(&mut cmd, std::time::Instant::now() + LAUNCHCTL_HANG)
        .map_err(|e| io::Error::other(format!("launchctl {}: {e}", argv.join(" "))))
}

/// One `launchctl` call that must succeed: a refusal is an error carrying what it said.
#[cfg(unix)]
fn launchctl_ok(argv: &[String]) -> io::Result<()> {
    let out = launchctl(argv)?;
    if out.status.success() {
        return Ok(());
    }
    let said = [&out.stderr, &out.stdout]
        .iter()
        .map(|b| String::from_utf8_lossy(b).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(io::Error::other(format!(
        "launchctl {} ({}): {said}",
        argv.join(" "),
        out.status
    )))
}

/// This user's GUI domain.
#[cfg(unix)]
fn own_domain() -> String {
    gui_domain(aterm_uds::peer::our_uid())
}

/// Read the job `label`'s state with `launchctl print` (read-only): loaded when it answers
/// 0, with the pid its text names; absent otherwise.
///
/// # Errors
///
/// `launchctl` cannot be run, or hangs.
#[cfg(unix)]
pub fn job_state(label: &str) -> io::Result<JobState> {
    let out = launchctl(&print_argv(&own_domain(), label))?;
    if !out.status.success() {
        return Ok(JobState::Absent);
    }
    Ok(JobState::Loaded {
        pid: print_pid(&String::from_utf8_lossy(&out.stdout)),
    })
}

/// Load (and, with `RunAtLoad`, start) the job `plist` defines in this user's GUI domain.
///
/// # Errors
///
/// `launchctl` cannot be run, or refuses.
#[cfg(unix)]
pub fn bootstrap(plist: &Path) -> io::Result<()> {
    launchctl_ok(&bootstrap_argv(&own_domain(), plist))
}

/// Start the loaded job `label` (a running one is left as it is).
///
/// # Errors
///
/// `launchctl` cannot be run, or refuses.
#[cfg(unix)]
pub fn kickstart(label: &str) -> io::Result<()> {
    launchctl_ok(&kickstart_argv(&own_domain(), label))
}

/// End the job `label` and unload it for this login (launchd's SIGTERM; no restart).
///
/// # Errors
///
/// `launchctl` cannot be run, or refuses (a label it does not know).
#[cfg(unix)]
pub fn bootout(label: &str) -> io::Result<()> {
    launchctl_ok(&bootout_argv(&own_domain(), label))
}

/// Make the job `label` run `program`'s mug: nothing when it runs, a kickstart when it is
/// loaded but stopped, else a bootstrap: of the startup plist when Enable at Startup is on,
/// else of a session plist written now ([`Paths::session_plist`], its directory `0700`,
/// naming the bundle `program` belongs to), which launchd loads for this login only. The
/// logs directory is made first, for the job's stderr.
///
/// # Errors
///
/// What failed, as one line: `launchctl`, or a plist that cannot be written.
#[cfg(unix)]
pub fn ensure_running(paths: &Paths, label: &str, program: &Path) -> Result<Started, String> {
    match job_state(label).map_err(|e| e.to_string())? {
        JobState::Loaded { pid: Some(pid) } => return Ok(Started::AlreadyRunning { pid }),
        JobState::Loaded { pid: None } => {
            return kickstart(label)
                .map(|()| Started::Kicked)
                .map_err(|e| e.to_string());
        }
        JobState::Absent => {}
    }
    std::fs::create_dir_all(&paths.logs_dir)
        .map_err(|e| format!("{}: {e}", paths.logs_dir.display()))?;
    let plist = if startup_enabled(paths, label) {
        paths.startup_plist(label)
    } else {
        let plist = paths.session_plist(label);
        write_session_plist(
            paths,
            &plist,
            label,
            program,
            bundle_id_of(program).as_deref(),
        )
        .map_err(|e| format!("{}: {e}", plist.display()))?;
        plist
    };
    match bootstrap(&plist) {
        Ok(()) => Ok(Started::Bootstrapped),
        // A second load of a loaded job is refused: another `start` or `on` bootstrapped it
        // between the read above and this one, and the job is there, as asked.
        Err(refused) => match job_state(label) {
            Ok(JobState::Loaded { pid: Some(pid) }) => Ok(Started::AlreadyRunning { pid }),
            Ok(JobState::Loaded { pid: None }) => kickstart(label)
                .map(|()| Started::Kicked)
                .map_err(|e| e.to_string()),
            Ok(JobState::Absent) | Err(_) => Err(refused.to_string()),
        },
    }
}

/// Write the session plist at `plist` (`0644`, as launchd reads a login plist; its directory
/// made `0700`).
#[cfg(unix)]
fn write_session_plist(
    paths: &Paths,
    plist: &Path,
    label: &str,
    program: &Path,
    bundle_id: Option<&str>,
) -> io::Result<()> {
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    utf8(program)?;
    if let Some(dir) = plist.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // Admit the actual directory handle before chmod: an existing link must not make
        // a session-plist write change an unrelated directory's permissions.
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
            .open(dir)?;
        let meta = directory.metadata()?;
        if !meta.is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not this user's directory", dir.display()),
            ));
        }
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    let text = plist_text(label, program, &paths.log_file(), bundle_id);
    aterm_uds::atomic_file::write(plist, text.as_bytes(), 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_app_and_dev_builds_never_share_a_label() {
        assert_eq!(label_for(Path::new(INSTALLED_EXECUTABLE)), INSTALLED_LABEL);
        let dev = label_for(Path::new("/Users//x/aterm/target/debug/aterm"));
        let short = dev
            .strip_prefix(&format!("{DEV_LABEL_PREFIX}."))
            .expect("the dev prefix");
        assert_eq!(short.len(), 8);
        assert!(
            short
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(
            dev,
            label_for(Path::new("/Users//x/aterm/target/debug/aterm"))
        );
        assert_ne!(
            dev,
            label_for(Path::new("/Users//x/aterm2/target/debug/aterm"))
        );
        // Only the installed path exactly: a ~/Applications install is a dev-style label.
        for other in [
            "/Users//x/Applications/aterm.app/Contents/MacOS/aterm",
            "/Applications/aterm (dev).app/Contents/MacOS/aterm",
        ] {
            assert_ne!(label_for(Path::new(other)), INSTALLED_LABEL, "{other}");
        }
    }

    #[test]
    fn only_a_label_this_crate_makes_is_one() {
        assert!(is_label(INSTALLED_LABEL));
        assert!(is_label(&label_for(Path::new("/w/target/debug/aterm"))));
        assert!(is_label("com.aterm.aterm.caffeine.dev.0123abcd"));
        for bad in [
            "",
            "com.aterm.aterm",
            "com.aterm.aterm.caffeine.dev",
            "com.aterm.aterm.caffeine.dev.",
            "com.aterm.aterm.caffeine.dev.0123ABCD",
            "com.aterm.aterm.caffeine.dev.0123abc",
            "com.aterm.aterm.caffeine.dev.0123abcde",
            "com.aterm.aterm.caffeine.dev.0123abcg",
            "com.aterm.aterm.caffeine.x",
            "com.aterm.aterm.caffeine\n",
        ] {
            assert!(!is_label(bad), "{bad:?}");
        }
    }

    /// A login plist naming a program that is gone is an orphan; one naming a program that
    /// is there (another build's, deliberately enabled), one on another volume, one that is
    /// not the mug's job and one that is not this crate's are not.
    #[test]
    fn an_orphan_is_a_mugs_login_plist_whose_program_is_gone() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
        };
        assert_eq!(orphans(&paths), [], "no LaunchAgents at all");
        let present = root.path().join("aterm");
        std::fs::write(&present, "exe").expect("exe");
        let gone = root.path().join("removed-worktree").join("aterm");
        let agents = paths.launch_agents();
        std::fs::create_dir_all(&agents).expect("agents");
        let plant = |label: &str, program: &Path| {
            let text = plist_text(label, program, &paths.log_file(), None);
            std::fs::write(agents.join(format!("{label}.plist")), text).expect("plist");
        };
        let orphan = "com.aterm.aterm.caffeine.dev.0000000a";
        plant(orphan, &gone);
        plant(INSTALLED_LABEL, &gone);
        plant("com.aterm.aterm.caffeine.dev.0000000b", &present);
        plant(
            "com.aterm.aterm.caffeine.dev.0000000c",
            Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
        );
        plant("com.example.other", &gone);
        // The parent is an ordinary file: this is an unknown path error, not absence.
        let uncertain = present.join("aterm");
        assert!(
            std::fs::symlink_metadata(&uncertain)
                .is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
        );
        plant("com.aterm.aterm.caffeine.dev.0000000e", &uncertain);
        std::fs::write(
            agents.join("com.aterm.aterm.caffeine.dev.0000000d.plist"),
            plist_text("x", &gone, &paths.log_file(), None)
                .replace("<string>serve</string>", "<string>status</string>"),
        )
        .expect("plist");
        assert_eq!(
            orphans(&paths),
            [
                (
                    INSTALLED_LABEL.to_string(),
                    paths.startup_plist(INSTALLED_LABEL)
                ),
                (orphan.to_string(), paths.startup_plist(orphan)),
            ]
        );
    }

    #[test]
    fn the_dev_label_folds_the_hash_as_the_keeper_does() {
        let exe = Path::new("/p/aterm");
        let h = aterm_hash::fnv1a_64(b"/p/aterm");
        assert_eq!(
            label_for(exe),
            format!("{DEV_LABEL_PREFIX}.{:08x}", (h >> 32) as u32 ^ h as u32)
        );
    }

    #[test]
    fn the_plist_names_the_job_and_escapes_every_string() {
        let text = plist_text(
            "com.aterm.aterm.caffeine.dev.0123abcd",
            Path::new("/Users//x/A&B <\"q'>.app/Contents/MacOS/aterm"),
            Path::new("/Users//x/Library/Logs/aterm/caffeine.log"),
            Some("com.aterm.aterm"),
        );
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert!(text.contains(
            "\t<key>Label</key>\n\t<string>com.aterm.aterm.caffeine.dev.0123abcd</string>\n"
        ));
        assert!(text.contains(
            "\t<array>\n\
             \t\t<string>/Users//x/A&amp;B &lt;&quot;q&apos;&gt;.app/Contents/MacOS/aterm</string>\n\
             \t\t<string>caffeine</string>\n\
             \t\t<string>serve</string>\n\
             \t</array>\n"
        ));
        assert!(!text.contains("A&B"), "an unescaped ampersand");
        assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
        assert!(text.contains(
            "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n"
        ));
        assert!(
            !text.contains("<key>KeepAlive</key>\n\t<true/>"),
            "never KeepAlive true: Quit must end the job"
        );
        assert!(text.contains("\t<key>ProcessType</key>\n\t<string>Interactive</string>\n"));
        assert!(text.contains("\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n"));
        assert!(text.contains(
            "\t<key>StandardErrorPath</key>\n\t<string>/Users//x/Library/Logs/aterm/caffeine.log</string>\n"
        ));
        assert!(text.contains(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>com.aterm.aterm</string>\n\t</array>\n"
        ));
        let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
        assert!(!bare.contains("AssociatedBundleIdentifiers"));
    }

    #[test]
    fn xml_escaping_covers_the_five_entities() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("&amp;"), "&amp;amp;");
        assert_eq!(xml_escape("plain/π"), "plain/π");
    }

    /// `plutil -lint` reads the plist as launchd will: well formed, with and without the
    /// bundle row and with every character escaping covers. Negative control: an unescaped
    /// ampersand does not lint.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_is_well_formed_by_plutil() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let lint = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("write");
            // A minute is a hang detector.
            let out = aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command("/usr/bin/plutil")
                    .arg("-lint")
                    .arg(&path),
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
            .expect("plutil");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let full = plist_text(
            "com.aterm.aterm.caffeine",
            Path::new("/A&B <\"q'>/x.app/Contents/MacOS/aterm"),
            Path::new("/l/caffeine & log.log"),
            Some("com.aterm.<aterm>"),
        );
        let (ok, said) = lint("full.plist", &full);
        assert!(ok, "{said}\n{full}");
        let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
        let (ok, said) = lint("bare.plist", &bare);
        assert!(ok, "{said}\n{bare}");
        let broken = full.replace("A&amp;B", "A&B");
        let (ok, _) = lint("broken.plist", &broken);
        assert!(!ok, "the control lints");
    }

    #[test]
    fn the_launchctl_argv_is_plain() {
        let domain = gui_domain(501);
        assert_eq!(domain, "gui/501");
        assert_eq!(print_argv(&domain, "l.x"), ["print", "gui/501/l.x"]);
        assert_eq!(
            bootstrap_argv(&domain, Path::new("/h/Library/LaunchAgents/l.x.plist")),
            ["bootstrap", "gui/501", "/h/Library/LaunchAgents/l.x.plist"]
        );
        assert_eq!(kickstart_argv(&domain, "l.x"), ["kickstart", "gui/501/l.x"]);
        assert_eq!(bootout_argv(&domain, "l.x"), ["bootout", "gui/501/l.x"]);
    }

    #[test]
    fn the_pid_is_the_services_own_line() {
        let running = "gui/501/com.aterm.aterm.caffeine = {\n\
                       \tactive count = 1\n\
                       \tpath = /Users//x/Library/LaunchAgents/com.aterm.aterm.caffeine.plist\n\
                       \tstate = running\n\
                       \n\
                       \tenvironment = {\n\
                       \t\tXPC_SERVICE_NAME => com.aterm.aterm.caffeine\n\
                       \t}\n\
                       \n\
                       \truns = 1\n\
                       \tpid = 1688\n\
                       \timmediate reason = non-ipc demand\n\
                       }\n";
        assert_eq!(print_pid(running), Some(1688));
        let stopped = "gui/501/l = {\n\tstate = not running\n\truns = 1\n\tlast exit code = 0\n}\n";
        assert_eq!(print_pid(stopped), None);
        let nested = "gui/501/l = {\n\tsection = {\n\t\tpid = 9\n\t}\n\tstate = not running\n}\n";
        assert_eq!(
            print_pid(nested),
            None,
            "a nested section's pid is not the job's"
        );
        assert_eq!(print_pid("\tpid = 0\n"), None);
        assert_eq!(print_pid("\tpid = x\n"), None);
        assert_eq!(print_pid("\tpid = 77  \n"), Some(77));
    }

    #[cfg(unix)]
    #[test]
    fn startup_is_the_login_plist_on_disk() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
        };
        let label = "com.aterm.aterm.caffeine.dev.0123abcd";
        let program = std::path::PathBuf::from("/t/target/debug/aterm");
        assert!(!startup_enabled(&paths, label));
        {
            // Off with nothing there is fine.
            set_startup(&paths, label, &program, false).expect("off");
            set_startup(&paths, label, &program, true).expect("on");
            assert!(startup_enabled(&paths, label));
            let plist = paths.startup_plist(label);
            assert_eq!(
                std::fs::read_to_string(&plist).expect("read"),
                plist_text(label, &program, &paths.log_file(), None)
            );
            let mode = std::fs::metadata(&plist)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644);
            assert!(paths.logs_dir.is_dir());
            // On again rewrites it in place; no temporary is left beside it.
            set_startup(&paths, label, &program, true).expect("on");
            let names: Vec<_> = std::fs::read_dir(plist.parent().expect("dir"))
                .expect("list")
                .map(|e| e.expect("entry").file_name())
                .collect();
            assert_eq!(names, [std::ffi::OsString::from(format!("{label}.plist"))]);
            set_startup(&paths, label, &program, false).expect("off");
            assert!(!startup_enabled(&paths, label));
            assert!(!plist.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_startup_plist_names_the_programs_bundle() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = root.path().join("X.app");
        std::fs::create_dir_all(app.join("Contents").join("MacOS")).expect("bundle");
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.x.y</string></dict></plist>",
        )
        .expect("plist");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
        };
        let program = app.join("Contents").join("MacOS").join("aterm");
        set_startup(&paths, "l", &program, true).expect("on");
        let text = std::fs::read_to_string(paths.startup_plist("l")).expect("read");
        assert!(text.contains("<string>com.x.y</string>"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_session_plist_sits_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
        };
        let plist = paths.session_plist("l");
        write_session_plist(&paths, &plist, "l", Path::new("/t/aterm"), None).expect("write");
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(plist.parent().expect("dir")), 0o700);
        assert_eq!(mode(&plist), 0o644);
        assert_eq!(
            std::fs::read_to_string(&plist).expect("read"),
            plist_text("l", Path::new("/t/aterm"), &paths.log_file(), None)
        );

        let aliased = Paths {
            state_dir: root.path().join("alias-state"),
            ..paths.clone()
        };
        std::fs::create_dir_all(&aliased.state_dir).expect("alias state");
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o750))
            .expect("outside mode");
        let marker = outside.join("keep");
        std::fs::write(&marker, b"unrelated bytes").expect("marker");
        let alias = aliased.state_dir.join("caffeine");
        std::os::unix::fs::symlink(&outside, &alias).expect("directory alias");
        assert!(
            write_session_plist(
                &aliased,
                &aliased.session_plist("l"),
                "l",
                Path::new("/t/aterm"),
                None,
            )
            .is_err()
        );
        assert_eq!(mode(&outside), 0o750);
        assert_eq!(std::fs::read(&marker).expect("marker"), b"unrelated bytes");
        assert!(!outside.join("l.plist").exists());
        assert!(
            std::fs::symlink_metadata(&alias)
                .expect("alias retained")
                .file_type()
                .is_symlink()
        );
    }
}

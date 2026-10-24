// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one-time launchd job that removes the folders [`super::install::uninstall`] keeps
//! ([`crate::first_exec::hold_left`]) once macOS can no longer be checking a program started
//! from them, and then removes itself. It outlives the agent, and its plist in
//! `/Library/LaunchDaemons` starts it again at each boot until it has run to its end.

use super::Layout;
use crate::controller::enroll::boot_id_check;
use crate::model::PValue;
use crate::port::{Cmd, System, shell_word};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The job's label, less the `.<run>` of the uninstall that set it up.
const LABEL: &str = "systems.alab.aterm-machines.reclaim";

/// Where launchd finds system jobs, at each boot too.
const DAEMONS: &str = "/Library/LaunchDaemons";

/// The file uninstall writes in each folder it keeps, holding its run id. The job takes only
/// a folder that holds it, so a folder made again at that path since (a later enrollment's
/// stage), or one a program is started from again ([`crate::first_exec::stamp`]) before the
/// job reads its mark, stays.
pub const KEPT: &str = "kept";

/// The commands the job reads the clocks, waits and takes itself out with.
struct Commands<'a> {
    /// Prints this boot's id (`kern.bootsessionuuid`).
    session: &'a str,
    /// Prints the last wake, which changes at each wake.
    wake: &'a str,
    /// Waits for its argument's seconds.
    sleep: &'a str,
    /// `launchctl`.
    launchctl: &'a str,
}

/// This Mac's [`Commands`].
const MAC: Commands<'static> = Commands {
    session: "/usr/sbin/sysctl -n kern.bootsessionuuid",
    wake: "/usr/sbin/sysctl -n kern.waketime",
    sleep: "/bin/sleep",
    launchctl: "/bin/launchctl",
};

/// The job's `/bin/sh` script, run as `sh -c <script> <label> <run> <boot id> <seconds>
/// <folder>...`. Unless the Mac has restarted since uninstall read `<boot id>`, it first
/// waits `<seconds>` with no wake inside them, so they are awake time whether or not `sleep`
/// counts time asleep. Then it empties each folder that still holds [`KEPT`] with `<run>`
/// from inside it, so a folder made again at that path since is never entered, [`KEPT`] last
/// so a run cut short by a shutdown is taken up at the next boot. It removes each folder,
/// `stage/`, `state/` and the agent root that is empty then. Last it removes its plist and
/// itself.
fn script(layout: &Layout, plist: &Path, label: &str, c: &Commands<'_>) -> String {
    let Commands {
        session,
        wake,
        sleep,
        launchctl,
    } = c;
    let q = |p: &Path| shell_word(&p.to_string_lossy());
    let (root, stage, state, plist) = (
        q(&layout.root),
        q(&layout.stage()),
        q(&layout.state()),
        q(plist),
    );
    format!(
        "run=$1 was=$2 secs=$3\n\
         shift 3\n\
         sess=$({session})\n\
         {}known=$ok\n\
         {}if [ -z \"$known\" ] || [ -z \"$ok\" ] || [ \"$sess\" = \"$was\" ]; then\n\
         while :; do\n\
         woke=$({wake})\n\
         {sleep} \"$secs\"\n\
         [ \"$({wake})\" != \"$woke\" ] || break\n\
         done\n\
         fi\n\
         for dir in \"$@\"; do\n\
         [ -L \"$dir\" ] || (cd -P \"$dir\" && [ \"$(/bin/cat {KEPT} 2>/dev/null)\" = \"$run\" ] \
         && /usr/bin/find . -mindepth 1 ! -path ./{KEPT} -delete && /bin/rm {KEPT})\n\
         /bin/rmdir \"$dir\" 2>/dev/null\n\
         done\n\
         /bin/rmdir {stage} {state} {root} 2>/dev/null\n\
         /bin/rm -f {plist}\n\
         {launchctl} bootout system/{label}\n",
        boot_id_check("$was"),
        boot_id_check("$sess"),
    )
}

/// The job's launchd plist: [`script`] with its arguments, started when it is loaded.
fn document(
    label: &str,
    script: String,
    run: &str,
    session: &str,
    secs: u64,
    kept: &[PathBuf],
) -> String {
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script,
        label.to_owned(),
        run.to_owned(),
        session.to_owned(),
        secs.to_string(),
    ];
    argv.extend(kept.iter().map(|dir| dir.to_string_lossy().into_owned()));
    crate::plist::document(&PValue::Dict(vec![
        ("Label".to_owned(), PValue::Str(label.to_owned())),
        (
            "ProgramArguments".to_owned(),
            PValue::Array(argv.into_iter().map(PValue::Str).collect()),
        ),
        ("RunAtLoad".to_owned(), PValue::Bool(true)),
    ]))
}

/// Leave `kept` to a one-time launchd job, which removes each of them once `left` of awake
/// time has passed from now, or after a restart, and then removes itself. `run` names the
/// job and marks each folder ([`KEPT`]).
///
/// # Errors
/// One line saying what could not be set up. Once the plist is written, launchd starts the
/// job at the next boot even when it could not be started now.
pub fn schedule(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    kept: &[PathBuf],
    left: Duration,
) -> Result<(), String> {
    for dir in kept {
        let marker = dir.join(KEPT);
        sys.write_file(&marker, run.as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", marker.display()))?;
    }
    let label = format!("{LABEL}.{run}");
    let plist = Path::new(DAEMONS).join(format!("{label}.plist"));
    let session = crate::first_exec::boot_clocks(sys)
        .map(|c| c.session)
        .unwrap_or_default();
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let doc = document(
        &label,
        script(layout, &plist, &label, &MAC),
        run,
        &session,
        secs,
        kept,
    );
    sys.write_file(&plist, doc.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", plist.display()))?;
    let bootstrap = Cmd::new("/bin/launchctl")
        .args(["bootstrap", "system"])
        .arg(plist.as_os_str());
    match sys.run(&bootstrap) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(format!(
            "{}: {}; it starts at the next restart",
            bootstrap.display(),
            ran.describe()
        )),
        Err(e) => Err(format!(
            "{}: {e}; it starts at the next restart",
            bootstrap.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Live, Ran};

    /// A boot id in `kern.bootsessionuuid`'s form.
    const BOOT_ID: &str = "55C317A3-F793-4F0D-BE04-262D2C6ED332";

    /// A folder under `root` a program was started from, kept by the uninstall `run` when
    /// it is `Some`.
    fn kept(root: &Path, dir: &str, run: Option<&str>) -> PathBuf {
        let dir = root.join(dir);
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        std::fs::write(dir.join("payload/aterm"), b"agent").unwrap();
        std::fs::write(dir.join("exec"), b"").unwrap();
        if let Some(run) = run {
            std::fs::write(dir.join(KEPT), run).unwrap();
        }
        dir
    }

    /// The job for `layout` and `plist`, run with the boot id `was` and `secs`, over `dirs`.
    /// The clocks read the boot id `session` and a wake that changes during the first wait
    /// when `wakes`; each wait and `launchctl` call is a line of `log`.
    fn job(
        layout: &Layout,
        plist: &Path,
        log: &Path,
        (session, wakes): (&str, bool),
        (was, secs): (&str, u64),
        dirs: &[PathBuf],
    ) -> Ran {
        let scratch = log.parent().unwrap();
        let (wake, woken) = (scratch.join("wake"), scratch.join("woken"));
        std::fs::write(&wake, b"first\n").unwrap();
        if wakes {
            std::fs::write(&woken, b"").unwrap();
        }
        let q = |p: &Path| shell_word(&p.to_string_lossy());
        let stubs = format!(
            "boot_id() {{ echo {session}; }}\n\
             last_wake() {{ /bin/cat {wake}; }}\n\
             nap() {{ echo \"sleep $1\" >> {log}; if [ -e {woken} ]; then /bin/rm {woken}; \
             echo later > {wake}; fi; }}\n\
             launchctl_stub() {{ echo \"launchctl $*\" >> {log}; }}\n",
            log = q(log),
            woken = q(&woken),
            wake = q(&wake),
        );
        let commands = Commands {
            session: "boot_id",
            wake: "last_wake",
            sleep: "nap",
            launchctl: "launchctl_stub",
        };
        let label = format!("{LABEL}.1-9");
        let body = script(layout, plist, &label, &commands);
        Live::new()
            .run(
                &Cmd::new("/bin/sh")
                    .arg("-c")
                    .arg(format!("{stubs}{body}"))
                    .args([label.as_str(), "1-9", was, &secs.to_string()])
                    .args(dirs.iter().map(|d| d.as_os_str())),
            )
            .unwrap()
    }

    #[test]
    fn the_job_waits_out_the_hold_then_takes_each_folder_it_was_left_and_itself() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let layout = Layout {
            root: tmp.path().join("aterm"),
        };
        let (plist, log) = (tmp.path().join("job.plist"), tmp.path().join("log"));
        let read = |log: &Path| std::fs::read_to_string(log).unwrap_or_default();
        let root = layout.root.clone();

        // The same boot, a wake during the first wait, and a Mac enrolled again since.
        std::fs::create_dir_all(root.join("agent")).unwrap();
        let ours = [
            kept(&root, ".staging", Some("1-9")),
            kept(&root, "stage/upgrade-1-5", Some("1-9")),
        ];
        let others = [
            kept(&root, "stage/upgrade-1-6", Some("1-8")),
            kept(&root, "stage/upgrade-1-7", None),
        ];
        std::fs::write(&plist, b"plist").unwrap();
        let dirs: Vec<PathBuf> = ours.iter().chain(&others).cloned().collect();
        let ran = job(
            &layout,
            &plist,
            &log,
            (BOOT_ID, true),
            (BOOT_ID, 540),
            &dirs,
        );
        assert!(ran.success(), "{}", ran.err());
        assert_eq!(
            read(&log),
            format!("sleep 540\nsleep 540\nlaunchctl bootout system/{LABEL}.1-9\n"),
            "a wake inside the wait starts it again"
        );
        for dir in &ours {
            assert!(!dir.exists(), "{}", dir.display());
        }
        for dir in &others {
            assert!(
                dir.join("payload/aterm").is_file(),
                "another uninstall's, or made again since: {}",
                dir.display()
            );
        }
        assert!(root.join("agent").is_dir() && !plist.exists());

        // A restart since, and two folders a run a shutdown cut short emptied, one with its
        // mark still in it: no wait, and nothing is left.
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_file(&log).unwrap();
        let (cut, emptied) = (
            root.join("stage/upgrade-1-4"),
            root.join("stage/upgrade-1-3"),
        );
        std::fs::create_dir_all(&cut).unwrap();
        std::fs::create_dir_all(&emptied).unwrap();
        std::fs::write(cut.join(KEPT), "1-9").unwrap();
        let ours = [
            kept(&root, ".staging", Some("1-9")),
            kept(&root, "state/aterm.app-first-run", Some("1-9")),
            kept(&root, "stage/upgrade-1-5", Some("1-9")),
            cut,
            emptied,
        ];
        std::fs::write(&plist, b"plist").unwrap();
        let other = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
        let ran = job(&layout, &plist, &log, (other, false), (BOOT_ID, 540), &ours);
        assert!(ran.success(), "{}", ran.err());
        assert_eq!(
            read(&log),
            format!("launchctl bootout system/{LABEL}.1-9\n")
        );
        assert!(!root.exists() && !plist.exists());

        // A boot id uninstall could not read: the job waits.
        std::fs::remove_file(&log).unwrap();
        let ours = [kept(&root, ".staging", Some("1-9"))];
        let ran = job(&layout, &plist, &log, (other, false), ("", 60), &ours);
        assert!(ran.success(), "{}", ran.err());
        assert!(read(&log).starts_with("sleep 60\n"), "{}", read(&log));
        assert!(!root.exists());
    }

    #[test]
    fn the_job_is_a_plist_launchd_starts_at_load_and_at_each_boot() {
        let layout = Layout::installed();
        let kept = [layout.staging(), layout.stage().join("upgrade-1-5")];
        let label = format!("{LABEL}.1-9");
        let plist = Path::new(DAEMONS).join(format!("{label}.plist"));
        let body = script(&layout, &plist, &label, &MAC);
        let doc = document(&label, body.clone(), "1-9", BOOT_ID, 600, &kept);
        let PValue::Dict(entries) = crate::plist::parse(doc.as_bytes()).unwrap() else {
            panic!("{doc}");
        };
        let get = |key: &str| {
            entries
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{key}: {doc}"))
        };
        assert_eq!(get("Label"), PValue::Str(label.clone()));
        assert_eq!(get("RunAtLoad"), PValue::Bool(true));
        let argv: Vec<PValue> = [
            "/bin/sh",
            "-c",
            &body,
            &label,
            "1-9",
            BOOT_ID,
            "600",
            "/Library/Management/aterm/.staging",
            "/Library/Management/aterm/stage/upgrade-1-5",
        ]
        .into_iter()
        .map(|a| PValue::Str(a.to_owned()))
        .collect();
        assert_eq!(get("ProgramArguments"), PValue::Array(argv));
        assert!(body.ends_with(&format!(
            "/bin/rm -f {}\n/bin/launchctl bootout system/{label}\n",
            plist.display()
        )));
    }
}

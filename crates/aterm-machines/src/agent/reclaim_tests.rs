// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    job_with_gate(
        layout,
        plist,
        log,
        (session, wakes),
        (was, secs),
        dirs,
        None,
    )
}

/// [`job`], optionally stopping after the real hard-link claim until `release` exists.
fn job_with_gate(
    layout: &Layout,
    plist: &Path,
    log: &Path,
    (session, wakes): (&str, bool),
    (was, secs): (&str, u64),
    dirs: &[PathBuf],
    gate: Option<(&Path, &Path)>,
) -> Ran {
    let scratch = log.parent().unwrap();
    let (wake, woken) = (scratch.join("wake"), scratch.join("woken"));
    std::fs::write(&wake, b"first\n").unwrap();
    if wakes {
        std::fs::write(&woken, b"").unwrap();
    }
    let q = |p: &Path| shell_word(&p.to_string_lossy());
    let claim = gate.map_or_else(String::new, |(claimed, release)| {
        format!(
            "claim() {{ /bin/ln \"$@\" || return; echo claimed > {}; \
             while [ ! -e {} ]; do /bin/sleep 0.01; done; }}\n",
            q(claimed),
            q(release),
        )
    });
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
        link: if gate.is_some() { "claim" } else { "/bin/ln" },
    };
    let label = format!("{LABEL}.1-9");
    let body = script(layout, plist, &label, &commands);
    Live::new()
        .run(
            &Cmd::new("/bin/sh")
                .arg("-c")
                .arg(format!("{stubs}{claim}{body}"))
                .args([label.as_str(), "1-9", was, &secs.to_string()])
                .args(dirs.iter().map(|d| d.as_os_str()))
                .budget(Duration::from_secs(60)),
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
fn a_reclaim_claim_and_a_new_start_cannot_both_own_the_same_folder() {
    struct Release<'a>(&'a Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0, b"release");
        }
    }

    let tmp = aterm_tempfile::tempdir().unwrap();
    let live = Live::new();
    let layout = Layout {
        root: tmp.path().join("claimed"),
    };
    let dir = kept(&layout.root, "state/aterm.app-first-run", Some("1-9"));
    let (plist, log) = (tmp.path().join("job.plist"), tmp.path().join("log"));
    let (claimed, release) = (
        tmp.path().join("claim-observed"),
        tmp.path().join("release"),
    );
    let dirs = [dir.clone()];
    std::thread::scope(|scope| {
        let _release_on_panic = Release(&release);
        let child = scope.spawn(|| {
            job_with_gate(
                &layout,
                &plist,
                &log,
                (BOOT_ID, false),
                (BOOT_ID, 0),
                &dirs,
                Some((&claimed, &release)),
            )
        });
        let by = std::time::Instant::now() + Duration::from_secs(60);
        while !claimed.is_file() {
            assert!(
                std::time::Instant::now() < by,
                "the job must claim its mark"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            crate::first_exec::stamp(&live, &dir).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "the actual paused reclaim job already owns this folder"
        );
        assert!(
            dir.join(CLAIM).is_file(),
            "cancellation cannot erase the claim"
        );
        std::fs::write(&release, b"release").unwrap();
        let ran = child.join().unwrap();
        assert!(ran.success(), "{}", ran.err());
    });
    assert!(
        !dir.exists(),
        "the claimed generation finishes before a new start"
    );
    std::fs::create_dir_all(&dir).unwrap();
    crate::first_exec::stamp(&live, &dir).unwrap();
    assert!(crate::first_exec::hold_left(&live, &dir).is_some());

    // Cancellation wins before the job's real link: its mark is gone, so it must not
    // touch the fresh stamp even though it was given that same path.
    let layout = Layout {
        root: tmp.path().join("canceled"),
    };
    let dir = kept(&layout.root, "state/aterm.app-first-run", Some("1-9"));
    crate::first_exec::stamp(&live, &dir).unwrap();
    let stamp = std::fs::read(dir.join(aterm_uds::deadline::EXEC_STAMP)).unwrap();
    let ran = job(
        &layout,
        &plist,
        &log,
        (BOOT_ID, false),
        (BOOT_ID, 0),
        std::slice::from_ref(&dir),
    );
    assert!(ran.success(), "{}", ran.err());
    assert_eq!(
        std::fs::read(dir.join(aterm_uds::deadline::EXEC_STAMP)).unwrap(),
        stamp
    );
    assert!(!dir.join(CLAIM).exists());

    // A shutdown after the claim leaves the run's claim for its job to resume; the finished
    // job removes it.
    let layout = Layout {
        root: tmp.path().join("resumed"),
    };
    let dir = kept(&layout.root, "state/aterm.app-first-run", Some("1-9"));
    std::fs::hard_link(dir.join(KEPT), dir.join(CLAIM)).unwrap();
    std::fs::remove_file(dir.join(KEPT)).unwrap();
    let ran = job(
        &layout,
        &plist,
        &log,
        (BOOT_ID, false),
        (BOOT_ID, 0),
        std::slice::from_ref(&dir),
    );
    assert!(ran.success(), "{}", ran.err());
    assert!(!dir.exists(), "the matching interrupted claim is finished");

    for bad in ["foreign", "malformed", "directory", "symlink"] {
        let layout = Layout {
            root: tmp.path().join(bad),
        };
        let dir = kept(&layout.root, "state/aterm.app-first-run", Some("1-9"));
        let claim = dir.join(CLAIM);
        let outside = tmp.path().join(format!("outside-{bad}"));
        std::fs::write(&outside, b"1-9").unwrap();
        match bad {
            "foreign" => std::fs::write(&claim, b"1-8").unwrap(),
            "malformed" => std::fs::write(&claim, b"not a run").unwrap(),
            "directory" => std::fs::create_dir(&claim).unwrap(),
            "symlink" => std::os::unix::fs::symlink(&outside, &claim).unwrap(),
            _ => unreachable!(),
        }
        let ran = job(
            &layout,
            &plist,
            &log,
            (BOOT_ID, false),
            (BOOT_ID, 0),
            std::slice::from_ref(&dir),
        );
        assert!(ran.success(), "{bad}: {}", ran.err());
        assert!(
            dir.join("payload/aterm").is_file(),
            "{bad}: no admitted claim"
        );
        assert!(
            std::fs::symlink_metadata(&claim).is_ok(),
            "{bad}: preserve the claim"
        );
        assert!(
            !claim.join(KEPT).exists(),
            "ln must not populate a claim directory"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"1-9");
        assert_eq!(
            crate::first_exec::stamp(&live, &dir).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "{bad}: an existing claim never authorizes a new exec"
        );
    }
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

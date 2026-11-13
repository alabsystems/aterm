// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, Mutex};

/// What one scripted mend did: its installs and paid debts.
#[derive(Default)]
struct Script {
    installs: usize,
    paid: usize,
}

/// A mend over a scripted read; an install answers `install`.
fn mend_with(
    configured: Result<Configured, String>,
    install: Result<bool, Unsynced>,
) -> (Option<Settled>, Script) {
    mend_while(true, configured, install)
}

/// [`mend_with`] while System State's plan in force keeps the vault in sync (`wanted`), or
/// does not.
fn mend_while(
    wanted: bool,
    configured: Result<Configured, String>,
    install: Result<bool, Unsynced>,
) -> (Option<Settled>, Script) {
    let script = Arc::new(Mutex::new(Script::default()));
    let reach = MendReach {
        wanted: Box::new(move || wanted),
        while_wanted: Box::new(move |work| {
            if wanted {
                work();
            }
            wanted
        }),
        configured: Box::new(move || configured.clone()),
        install: Box::new({
            let script = Arc::clone(&script);
            move || {
                script.lock().unwrap().installs += 1;
                install.clone()
            }
        }),
        paid: Box::new({
            let script = Arc::clone(&script);
            move || script.lock().unwrap().paid += 1
        }),
    };
    let settled = mend(&reach);
    drop(reach);
    let script = Arc::try_unwrap(script).ok().unwrap().into_inner().unwrap();
    (settled, script)
}

/// A configured Mac's setup: in, its subscriber sound.
fn setup() -> Setup {
    Setup {
        member: true,
        pending: false,
        plist: Plist::Sound,
        owed: false,
        turned_off: false,
    }
}

/// A plist running a copy that is gone or does not last is installed again; a debt is
/// installed and a paid one forgotten; a Mac with no plist and nothing owed is left as it is,
/// and so is one the person turned off, before the read or during the install. A failed
/// install says why; a Mac with no vault configured, or one that cannot be read, is left
/// alone.
#[test]
fn a_stale_or_owed_subscriber_is_installed_at_launch() {
    let stale = Ok(Configured::Some(Setup {
        plist: Plist::Stale("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into()),
        ..setup()
    }));
    let (settled, script) = mend_with(stale.clone(), Ok(true));
    assert_eq!(script.installs, 1);
    assert_eq!(
        settled,
        Some(Settled {
            member: true,
            subscriber: Subscriber::Installed,
            unfinished: false
        })
    );

    let fleeting = Unsynced::Fleeting("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
    let (settled, _) = mend_with(stale, Err(fleeting.clone()));
    let settled = settled.expect("configured");
    assert_eq!(settled.subscriber, Subscriber::Missing(fleeting));
    assert!(settled_line(&settled).contains("goes away when it quits"));

    let owed = Ok(Configured::Some(Setup {
        plist: Plist::Absent,
        owed: true,
        ..setup()
    }));
    let (_, script) = mend_with(owed.clone(), Ok(true));
    assert_eq!((script.installs, script.paid), (1, 0));
    let (settled, script) = mend_with(owed, Ok(false));
    assert_eq!(script.installs, 1);
    assert_eq!(
        settled.map(|s| s.subscriber),
        Some(Subscriber::Off),
        "turned off during the install"
    );

    let paid = Ok(Configured::Some(Setup {
        owed: true,
        ..setup()
    }));
    let (_, script) = mend_with(paid, Ok(true));
    assert_eq!((script.installs, script.paid), (0, 1));

    let off = Ok(Configured::Some(Setup {
        plist: Plist::Absent,
        turned_off: true,
        ..setup()
    }));
    let (settled, script) = mend_with(off, Ok(true));
    assert_eq!((script.installs, script.paid), (0, 0));
    assert_eq!(settled.map(|s| s.subscriber), Some(Subscriber::Off));
    let owed_off = Ok(Configured::Some(Setup {
        plist: Plist::Absent,
        owed: true,
        turned_off: true,
        ..setup()
    }));
    let (_, script) = mend_with(owed_off, Ok(true));
    assert_eq!(
        (script.installs, script.paid),
        (0, 1),
        "the off wins over the debt"
    );

    for owed in [false, true] {
        for gone in [Plist::Absent, Plist::Down] {
            let gone = Ok(Configured::Some(Setup {
                plist: gone,
                owed,
                ..setup()
            }));
            let (settled, script) = mend_with(gone, Ok(true));
            assert_eq!(script.installs, usize::from(owed));
            let subscriber = if owed {
                Subscriber::Installed
            } else {
                Subscriber::Off
            };
            assert_eq!(settled.map(|s| s.subscriber), Some(subscriber));
        }
    }
    let down_off = Ok(Configured::Some(Setup {
        plist: Plist::Down,
        turned_off: true,
        ..setup()
    }));
    let (settled, script) = mend_with(down_off, Ok(true));
    assert_eq!(script.installs, 0);
    assert_eq!(settled.map(|s| s.subscriber), Some(Subscriber::Off));

    let unread = |turned_off| {
        Ok(Configured::Some(Setup {
            plist: Plist::Unread("launchctl print: exit 1".into()),
            owed: true,
            turned_off,
            ..setup()
        }))
    };
    let (settled, script) = mend_with(unread(false), Ok(true));
    assert_eq!((script.installs, script.paid), (0, 0), "the debt is kept");
    let settled = settled.expect("configured");
    assert!(matches!(
        settled.subscriber,
        Subscriber::Missing(Unsynced::Failed(_))
    ));
    assert!(settled_line(&settled).contains("could not say whether"));
    let (settled, script) = mend_with(unread(true), Ok(true));
    assert_eq!((script.installs, script.paid), (0, 1));
    assert_eq!(settled.map(|s| s.subscriber), Some(Subscriber::Off));

    let cut_off = Ok(Configured::Some(Setup {
        member: false,
        pending: true,
        plist: Plist::Absent,
        owed: false,
        turned_off: false,
    }));
    let (settled, _) = mend_with(cut_off, Ok(true));
    let settled = settled.expect("configured");
    assert!(settled.unfinished);
    assert!(settled_line(&settled).contains("never finished"));

    for nothing in [Ok(Configured::None), Err("unreadable".to_string())] {
        let (settled, script) = mend_with(nothing, Ok(true));
        assert_eq!(settled, None);
        assert_eq!((script.installs, script.paid), (0, 0));
    }
}

/// The mend runs only while System State's plan in force keeps the vault in sync (its
/// Secrets vault group at Set up, no person's Stop holding its run): otherwise a stale plist,
/// an owed one that is absent or down, and a paid debt are all left as they are, and nothing
/// is logged or posted.
#[test]
fn the_mend_changes_nothing_while_the_plan_in_force_leaves_the_vault_as_is() {
    let stale = Plist::Stale("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
    for plist in [Plist::Absent, Plist::Down, Plist::Sound, stale] {
        for owed in [false, true] {
            let read = Ok(Configured::Some(Setup {
                plist: plist.clone(),
                owed,
                ..setup()
            }));
            let (settled, script) = mend_while(false, read.clone(), Ok(true));
            assert_eq!(settled, None, "{plist:?}, owed {owed}");
            assert_eq!(
                (script.installs, script.paid),
                (0, 0),
                "{plist:?}, owed {owed}"
            );
            // The same read, with the plan keeping the vault in sync, is mended as before.
            let (_, kept) = mend_while(true, read, Ok(true));
            let mends = plan(&Setup {
                plist: plist.clone(),
                owed,
                ..setup()
            });
            assert_eq!(
                (kept.installs, kept.paid),
                (usize::from(mends.install), usize::from(mends.paid)),
                "{plist:?}, owed {owed}"
            );
        }
    }
}

#[test]
fn the_mend_does_not_act_when_the_choice_changes_during_its_read() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut outcomes = Vec::new();
    for (plist, permitted) in [
        (Plist::Stale("/gone".into()), (1, 0)),
        (Plist::Sound, (0, 1)),
    ] {
        let wanted = Arc::new(AtomicBool::new(true));
        let revoke = Arc::new(AtomicBool::new(true));
        let script = Arc::new(Mutex::new(Script::default()));
        let reach = MendReach {
            wanted: Box::new({
                let wanted = Arc::clone(&wanted);
                move || wanted.load(Ordering::SeqCst)
            }),
            while_wanted: Box::new({
                let wanted = Arc::clone(&wanted);
                move |work| {
                    if !wanted.load(Ordering::SeqCst) {
                        return false;
                    }
                    work();
                    true
                }
            }),
            configured: Box::new({
                let wanted = Arc::clone(&wanted);
                let revoke = Arc::clone(&revoke);
                move || {
                    if revoke.swap(false, Ordering::SeqCst) {
                        assert!(wanted.swap(false, Ordering::SeqCst));
                    }
                    Ok(Configured::Some(Setup {
                        plist: plist.clone(),
                        owed: true,
                        ..setup()
                    }))
                }
            }),
            install: Box::new({
                let script = Arc::clone(&script);
                move || {
                    script.lock().unwrap().installs += 1;
                    Ok(true)
                }
            }),
            paid: Box::new({
                let script = Arc::clone(&script);
                move || script.lock().unwrap().paid += 1
            }),
        };
        let settled = mend(&reach);
        {
            let script = script.lock().unwrap();
            outcomes.push((settled, script.installs, script.paid));
        }
        wanted.store(true, Ordering::SeqCst);
        // Once admitted again, the same stale or owed setup is repaired normally.
        let repaired = mend(&reach).expect("a current choice permits the mend");
        assert_eq!(repaired.subscriber, Subscriber::Installed);
        let script = script.lock().unwrap();
        assert_eq!((script.installs, script.paid), permitted);
    }
    assert_eq!(outcomes, [(None, 0, 0), (None, 0, 0)]);
}

/// Only a stale plist, or an absent or down one that is owed, is installed, and none the
/// person turned off; only an owed line with a sound plist, or one the person turned off, is
/// paid; only a pending Mac with no subscriber and nothing owed is unfinished.
#[test]
fn the_launch_plan_installs_only_what_is_broken_or_owed() {
    let stale = Plist::Stale("/gone".to_string());
    let unread = Plist::Unread("launchctl print: exit 1".to_string());
    for plist in [Plist::Absent, Plist::Sound, Plist::Down, stale, unread] {
        for owed in [false, true] {
            for pending in [false, true] {
                for turned_off in [false, true] {
                    let read = Setup {
                        member: !pending,
                        pending,
                        plist: plist.clone(),
                        owed,
                        turned_off,
                    };
                    let want = Plan {
                        install: !turned_off
                            && (matches!(plist, Plist::Stale(_))
                                || (matches!(plist, Plist::Absent | Plist::Down) && owed)),
                        paid: owed && (plist == Plist::Sound || turned_off),
                        unfinished: pending && plist == Plist::Absent && !owed,
                    };
                    assert_eq!(plan(&read), want, "{read:?}");
                }
            }
        }
    }
}

/// A held job whose mend failed is not tried again this launch, so a move that keeps failing
/// boots the running job out once, not at every [`MEND_EVERY`]; the other job is still
/// mended each pass, and the next launch, whose memory starts empty, tries again.
/// NEGATIVE CONTROL: a [`held_pass`] that forgets the failure tries the failing move at
/// every pass.
#[cfg(unix)]
#[test]
fn a_held_move_that_failed_is_tried_again_only_on_the_next_launch() {
    use aterm_secrets::client::HeldMend;
    use aterm_secrets::service::Job;
    let jobs = [Job::Server, Job::Subscriber];
    let mut tried = Vec::new();
    let mut mend = |job: Job| {
        tried.push(job);
        match job {
            Job::Server => Err("Bootstrap failed: 5: Input/output error".to_string()),
            Job::Subscriber => Ok(HeldMend::NotHeld),
        }
    };
    let mut failed = Vec::new();
    let first = held_pass(&jobs, &mut failed, &mut mend);
    assert_eq!(
        first,
        [
            (
                Job::Server,
                Err("Bootstrap failed: 5: Input/output error".to_string())
            ),
            (Job::Subscriber, Ok(HeldMend::NotHeld)),
        ]
    );
    let second = held_pass(&jobs, &mut failed, &mut mend);
    assert_eq!(second, [(Job::Subscriber, Ok(HeldMend::NotHeld))]);
    let mut next_launch = Vec::new();
    held_pass(&jobs, &mut next_launch, &mut mend);
    assert_eq!(
        tried,
        [
            Job::Server,
            Job::Subscriber,
            Job::Subscriber,
            Job::Server,
            Job::Subscriber
        ]
    );
}

#[test]
fn the_mend_reads_again_unless_aterm_runs_from_a_copy_that_goes_away_or_fails_alike() {
    let settled = |subscriber| Settled {
        member: true,
        subscriber,
        unfinished: false,
    };
    let failed = |why: &str| settled(Subscriber::Missing(Unsynced::Failed(why.into())));
    let running = settled(Subscriber::Installed);
    assert!(mends_again(None, None));
    assert!(mends_again(None, Some(&running)));
    assert!(mends_again(Some(&running), Some(&running)));
    let refused = failed("launchctl bootstrap: exit 5");
    assert!(mends_again(None, Some(&refused)));
    assert!(mends_again(Some(&running), Some(&refused)));
    assert!(mends_again(
        Some(&failed("launchctl kickstart: exit 3")),
        Some(&refused)
    ));
    assert!(
        !mends_again(Some(&refused), Some(&refused)),
        "failed twice alike"
    );
    assert!(!mends_again(
        None,
        Some(&settled(Subscriber::Missing(Unsynced::Fleeting(
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into()
        ))))
    ));
}

/// The plist's program is read back exactly as `aterm secrets` writes it, entities
/// and all; a plist in another shape names nothing (and is left as it is).
#[cfg(unix)]
#[test]
fn the_plists_program_is_read_back_as_service_writes_it() {
    use aterm_secrets::service;
    let program = Path::new("/Applications/a&b <c> \"d\" 'e'.app/Contents/MacOS/aterm");
    let text = service::plist_text(
        service::Job::Subscriber.label(),
        program,
        &["secrets".to_string(), "watch".to_string()],
        Path::new("/Users//x/.config/aterm/secrets/x.log"),
    );
    assert_eq!(plist_program(&text).as_deref(), program.to_str());
    assert_eq!(plist_program("<plist><dict></dict></plist>"), None);
    assert_eq!(
        plist_program("<key>ProgramArguments</key><string>/x</string>"),
        None,
        "not an array"
    );
    assert_eq!(
        plist_program("<key>ProgramArguments</key><array><string>/a&#47;b</string>"),
        None,
        "an entity plist_text never writes"
    );
}

/// A plist on disk: absent; sound when it runs a file at a lasting place; stale when that file
/// is gone or sits in a temporary directory.
#[cfg(unix)]
#[test]
fn a_plist_is_judged_by_the_program_it_runs() {
    use aterm_secrets::service;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let plist = dir.path().join("job.plist");
    assert_eq!(subscriber_plist(&plist), Plist::Absent);
    let write = |program: &Path| {
        let text = service::plist_text("x", program, &[], Path::new("/tmp/x.log"));
        std::fs::write(&plist, text).expect("plist");
    };
    write(Path::new("/bin/sh"));
    assert_eq!(subscriber_plist(&plist), Plist::Sound);
    let gone = Path::new("/Applications/no-such-aterm.app/Contents/MacOS/aterm");
    write(gone);
    assert_eq!(
        subscriber_plist(&plist),
        Plist::Stale(gone.display().to_string())
    );
    let scratch = dir.path().join("aterm");
    std::fs::write(&scratch, b"").expect("program");
    write(&scratch);
    assert_eq!(
        subscriber_plist(&plist),
        Plist::Stale(scratch.display().to_string()),
        "a temporary directory does not last"
    );
    std::fs::write(&plist, b"bplist00").expect("binary");
    assert_eq!(subscriber_plist(&plist), Plist::Sound, "left as it is");
    assert!(!own_plist(&plist), "not aterm's shape");
    write(gone);
    assert!(own_plist(&plist));
    let text = std::fs::read_to_string(&plist).expect("plist");
    let switched_off = text.replacen("<dict>", "<dict>\n\t<key>Disabled</key>\n\t<true/>", 1);
    assert_ne!(switched_off, text);
    std::fs::write(&plist, switched_off).expect("plist");
    assert_eq!(
        subscriber_plist(&plist),
        Plist::Sound,
        "a person's off is left as it is, whatever it runs"
    );
    assert!(!own_plist(&plist));
}

#[test]
fn only_a_lasting_copy_is_named_by_the_subscriber() {
    for fleeting in [
        "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
        "/private/var/folders/ab/xyz/T/AppTranslocation/0A1B/d/aterm.app/Contents/MacOS/aterm",
    ] {
        assert_eq!(
            lasting_program(PathBuf::from(fleeting)),
            Err(Unsynced::Fleeting(fleeting.to_string()))
        );
    }
    let installed = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    assert_eq!(lasting_program(installed.clone()), Ok(installed));
}

#[test]
fn the_owed_record_is_noted_forgotten_and_fails_toward_owing_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let config = dir.path().join("aterm.toml");
    let record = dir.path().join(OWED_RECORD);
    assert!(!owed(Some(&config), Some("MAC-A")));
    note_owed(Some(&config), Some("MAC-A"), true);
    note_owed(Some(&config), Some("MAC-B"), true);
    assert!(owed(Some(&config), Some("MAC-A")) && owed(Some(&config), Some("MAC-B")));
    assert!(!owed(Some(&config), Some("MAC-C")));
    note_owed(Some(&config), Some("MAC-A"), false);
    assert!(!owed(Some(&config), Some("MAC-A")));
    assert!(
        owed(Some(&config), Some("MAC-B")),
        "another Mac's debt stays"
    );
    note_owed(Some(&config), Some("MAC-B"), false);
    assert!(!record.exists(), "a record naming no Mac goes");
    note_owed(Some(&config), Some("MAC-B"), false);

    std::fs::write(&record, "not a record").unwrap();
    assert!(
        !owed(Some(&config), Some("MAC-A")),
        "unreadable: nothing owed"
    );
    assert!(
        crate::host_record::lists(&config, OWED_RECORD, "MAC-A").is_err(),
        "the record's reader names the failure"
    );
    note_owed(Some(&config), Some("MAC-A"), false);
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        "not a record",
        "a record that cannot be read is left as found"
    );
    assert!(!owed(None, Some("MAC-A")) && !owed(Some(&config), None));
}

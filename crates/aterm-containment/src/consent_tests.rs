// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// One process reads a bundle's signature report once, whoever asks (the privacy row, the
/// copy census); a re-signed executable reads it afresh. NEGATIVE CONTROL: another bundle,
/// the same bundle after its executable changed, and after its mode shut codesign out, each
/// read again.
#[cfg(unix)]
#[test]
fn a_bundles_signature_report_is_read_once_until_it_is_re_signed() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("sig-report");
    let bundle = |name: &str| {
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
        std::fs::write(root.join("Contents/Info.plist"), "<plist/>").unwrap();
        std::fs::write(root.join("Contents/MacOS/aterm"), "one").unwrap();
        root
    };
    let one = bundle("One.app");
    let other = bundle("Other.app");
    let reads = std::cell::Cell::new(0);
    let read = |_: &Path| {
        reads.set(reads.get() + 1);
        Some(format!("report {}", reads.get()))
    };
    assert_eq!(kept_report(&one, read).as_deref(), Some("report 1"));
    assert_eq!(kept_report(&one, read).as_deref(), Some("report 1"));
    assert_eq!(reads.get(), 1, "the second reader took the first report");
    assert_eq!(kept_report(&other, read).as_deref(), Some("report 2"));
    std::fs::write(one.join("Contents/MacOS/aterm"), "re-signed").unwrap();
    assert_eq!(kept_report(&one, read).as_deref(), Some("report 3"));
    assert_eq!(reads.get(), 3, "a changed executable reads afresh");
    let exe = one.join("Contents/MacOS/aterm");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(kept_report(&one, read).as_deref(), Some("report 4"));
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        reads.get(),
        4,
        "an executable codesign can no longer read reads afresh"
    );
    assert_eq!(
        kept_report(&dir.path().join("Gone.app"), read).as_deref(),
        Some("report 5")
    );
    assert_eq!(
        kept_report(&dir.path().join("Gone.app"), read).as_deref(),
        Some("report 6")
    );
    assert_eq!(reads.get(), 6, "an unresolvable bundle is read every time");
}

#[cfg(target_os = "macos")]
type ReadersAct = Box<dyn FnOnce(&mut std::time::Instant)>;

#[cfg(target_os = "macos")]
thread_local! {
    static READERS_PENDING: std::cell::RefCell<std::collections::VecDeque<ReadersAct>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(target_os = "macos")]
pub(super) fn readers_pending(deadline: &mut std::time::Instant) {
    if let Some(act) = READERS_PENDING.with(|acts| acts.borrow_mut().pop_front()) {
        act(deadline);
    }
}

#[cfg(unix)]
type GracePollAct = (u32, Box<dyn FnOnce(&mut u32, &dyn Fn() -> bool)>);

#[cfg(unix)]
thread_local! {
    static GRACE_POLL: std::cell::RefCell<Option<GracePollAct>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
pub(super) fn reader_grace_poll(poll: &mut u32, readers_done: &dyn Fn() -> bool) {
    let act = GRACE_POLL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(at, _)| *at == *poll) {
            slot.take().map(|(_, act)| act)
        } else {
            None
        }
    });
    if let Some(act) = act {
        act(poll, readers_done);
    }
}

#[cfg(target_os = "macos")]
const GATED_GRANDCHILD: &str =
    "exec 3<>\"$1\"; (read line <&3; printf 'late%s' \"$line\") & printf early";

#[cfg(target_os = "macos")]
const HANG_DETECTOR_RELEASE: &str = "+released-by-the-hang-detector";

#[cfg(target_os = "macos")]
fn fifo_gate(dir: &Path) -> PathBuf {
    let gate = dir.join("gate");
    let fifo = std::ffi::CString::new(gate.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    gate
}

#[cfg(target_os = "macos")]
fn release_gate(gate: &Path) -> std::io::Result<()> {
    release_gate_saying(gate, "")
}

#[cfg(target_os = "macos")]
fn release_gate_saying(gate: &Path, said: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(gate)
        .and_then(|mut gate| gate.write_all(format!("{said}\n").as_bytes()))
}

#[cfg(target_os = "macos")]
fn gate_hang_detector(gate: &Path) -> std::sync::mpsc::Sender<()> {
    let (stand_down, detector) = std::sync::mpsc::channel::<()>();
    let gate = gate.to_path_buf();
    std::thread::spawn(move || {
        if detector.recv_timeout(HANG_DETECTOR) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            let _ = release_gate_saying(&gate, HANG_DETECTOR_RELEASE);
        }
    });
    stand_down
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn the_bundle_root_is_lexical() {
    let (app, dev, inner) = (
        "/A/x.app",
        "/A/x (dev).app",
        "/A/O.app/Contents/MacOS/I.app",
    );
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        ("/A/x.app/Contents/MacOS/x", Some(app), Some(app)),
        ("/A/x (dev).app/Contents/MacOS/x", Some(dev), Some(dev)),
        ("/A/x.app/Contents/MacOS/helpers/x", Some(app), Some(app)),
        (
            "/A/O.app/Contents/MacOS/I.app/Contents/MacOS/x",
            Some(inner),
            Some(inner),
        ),
        (
            "sub/x.app/Contents/MacOS/x",
            Some("sub/x.app"),
            Some("sub/x.app"),
        ),
        (
            "/A/x.app.rollback/Contents/MacOS/x",
            None,
            Some("/A/x.app.rollback"),
        ),
        ("/A/.app/Contents/MacOS/x", None, Some("/A/.app")),
        ("rel/Contents/MacOS/x", None, Some("rel")),
        ("/Contents/MacOS/x", None, None),
        ("/A/target/debug/deps/aterm_containment-abc123", None, None),
        ("/usr/local/bin/aterm", None, None),
        (app, None, None),
        ("/A/x.app/Contents/MacOS", None, None),
        ("/A/x.app/Contents/Resources/x", None, None),
        ("/A/x.app/Other/MacOS/x", None, None),
        (
            "/A/X.APP/Contents/MacOS/x",
            Some("/A/X.APP"),
            Some("/A/X.APP"),
        ),
    ];
    for &(exe, bundle, layout) in cases {
        let exe_path = Path::new(exe);
        assert_eq!(app_bundle_root(exe_path), bundle.map(p), "{exe}");
        assert_eq!(path_is_in_app_bundle(exe_path), bundle.is_some(), "{exe}");
        assert_eq!(bundle_layout_root(exe_path), layout.map(p), "{exe}");
    }
}

const DEV_ID: &str = "designated => identifier \"x\" and anchor apple generic and \
                      certificate leaf[subject.OU] = A66A9P66Z7";

fn as_spelled(_: &Path) -> Option<Located> {
    None
}

fn ours(dr: &str) -> Candidate {
    Candidate::Ours(BundleIdentity {
        bundle_id: Some("x".to_string()),
        dr_text: dr.to_string(),
        dr: classify_dr(dr),
        signing: if dr.contains("cdhash") {
            "adhoc"
        } else {
            "developer-id"
        },
        team: None,
    })
}

#[test]
fn the_signing_readers_fail_toward_the_weaker_claim() {
    let devid = "Executable=/Applications/aterm.app/Contents/MacOS/aterm\n\
                 TeamIdentifier=A66A9P66Z7\n\
                 designated => identifier \"com.aterm.aterm\" and anchor apple generic\n";
    assert_eq!(classify_signing(devid), "developer-id");
    assert_eq!(team_identifier(devid).as_deref(), Some("A66A9P66Z7"));
    assert_eq!(
        classify_dr(&designated_requirement(devid).unwrap()),
        DrClass::Identity
    );

    let adhoc = "Signature=adhoc\n\
                 TeamIdentifier=not set\n\
                 # designated => cdhash H\"4249c4f5\"\n";
    assert_eq!(classify_signing(adhoc), "adhoc");
    assert_eq!(team_identifier(adhoc), None, "`not set` is not a team id");
    let clause = designated_requirement(adhoc).expect("the commented form reads");
    assert_eq!(clause, "designated => cdhash H\"4249c4f5\"");
    assert_eq!(classify_dr(&clause), DrClass::Cdhash);

    let unsigned = "/x: code object is not signed at all\n";
    assert_eq!(classify_signing(unsigned), "unsigned");
    assert_eq!(designated_requirement(unsigned), None);
    assert_eq!(classify_signing("something else entirely\n"), "unknown");
    assert_eq!(team_identifier("TeamIdentifier=\n"), None, "an empty id");
    for (report, signing) in [
        (
            "/x: code object is not signed at all\nIdentifier=x\n",
            "unknown",
        ),
        ("not signed at all\n", "unknown"),
        (
            "Executable=/A/Signature=adhoc/x\nTeamIdentifier=T\n",
            "developer-id",
        ),
    ] {
        assert_eq!(classify_signing(report), signing, "{report:?}");
    }
}

#[test]
fn the_plist_reader_is_exact_and_xml_only() {
    let xml = "<plist><dict>\
               <key>CFBundleIdentifier</key><string>com.aterm.aterm.dev</string>\
               <key>CFBundleNameSuffix</key><string>nope</string>\
               </dict></plist>";
    assert_eq!(
        plist_string(xml, "CFBundleIdentifier"),
        Some("com.aterm.aterm.dev")
    );
    assert_eq!(plist_string(xml, "CFBundleName"), None);
    assert_eq!(
        plist_string("<key>A</key><key>B</key><string>v</string>", "A"),
        None
    );
    assert_eq!(plist_string("bplist00\u{0}\u{1}", "A"), None);
    assert_eq!(
        plist_string("<key> A </key>\n\t<string> v </string>", "A"),
        Some("v"),
        "whitespace around the key, before its string and inside it"
    );
    #[cfg(unix)]
    assert_eq!(read_plist_bytes(Path::new("/dev/null")), None, "a device");
    let tmp = crate::test_scratch_dir("plist-bytes");
    let sized = |name: &str, len: u64| {
        let path = tmp.path().join(name);
        std::fs::File::create(&path).unwrap().set_len(len).unwrap();
        path
    };
    assert!(read_plist_bytes(&sized("at", INFO_PLIST_MAX_BYTES)).is_some());
    assert_eq!(
        read_plist_bytes(&sized("over", INFO_PLIST_MAX_BYTES + 1)),
        None
    );
}

#[test]
fn an_unreadable_requirement_conflicts_rather_than_matching() {
    let mk = |dr: &str| Claimant {
        path: PathBuf::from("/x/aterm.app"),
        dr: classify_dr(dr),
        dr_text: dr.to_string(),
        signing: "adhoc",
        team: None,
        running: false,
        links: Vec::new(),
    };
    assert!(!mk(DEV_ID).conflicts_with(&mk(DEV_ID)));
    assert!(mk("").conflicts_with(&mk("")));
    assert!(!mk(DEV_ID).conflicts_with(&mk(&format!(" {DEV_ID}\n"))));
    assert!(mk(" ").conflicts_with(&mk(" ")));
    let fresh = |dr, signing, dr_text: &str| BundleIdentity {
        bundle_id: Some("x".to_string()),
        dr_text: dr_text.to_string(),
        dr,
        signing,
        team: None,
    };
    let unsigned = Claimant {
        dr: DrClass::Unsigned,
        signing: "unsigned",
        ..mk("")
    };
    let unread = Claimant {
        dr: DrClass::Unknown,
        signing: "unknown",
        ..mk("")
    };
    for (read, copy, same) in [
        (fresh(DrClass::Unsigned, "unsigned", ""), &unsigned, true),
        (fresh(DrClass::Unknown, "unknown", ""), &unread, false),
        (fresh(DrClass::Cdhash, "unsigned", ""), &unsigned, false),
        (fresh(DrClass::Unsigned, "adhoc", ""), &unsigned, false),
    ] {
        assert_eq!(same_identity(&read, copy), same, "{read:?}");
    }
}

#[test]
fn the_census_finds_every_claimant_including_rollback_and_registered_copies() {
    let apps = PathBuf::from("/Apps");
    let running = apps.join("x.app");
    let far = PathBuf::from("/far/away/x.app");
    let roots = vec![
        (apps.clone(), true),
        (PathBuf::from("/Applications"), false),
    ];
    let list = |dir: &Path| {
        if dir == apps {
            Listing::Entries(vec![
                apps.join("x.app"),
                apps.join("x.app.rollback"),
                apps.join("x-backup.app"),
                apps.join("x-unread.app"),
                apps.join("Other.app"),
                apps.join("notes.txt"),
                apps.join("x-plain"),
            ])
        } else {
            Listing::Missing
        }
    };
    let read = |root: &Path| match root.file_name().and_then(|n| n.to_str()) {
        Some("x.app") if root.starts_with("/Apps") => ours(DEV_ID),
        Some("x.app.rollback") => ours("designated => cdhash H\"aa\""),
        Some("x-backup.app") => ours("designated => cdhash H\"bb\""),
        Some("x-unread.app") => ours(""),
        Some("x.app") => ours("designated => identifier \"x\" and anchor apple generic"),
        Some("x-plain") => ours("designated => cdhash H\"cc\""),
        _ => Candidate::NotOurs,
    };
    let trashed = PathBuf::from("/Users//u/.Trash/x.app");
    let volume_trash = PathBuf::from("/Volumes/V/.Trashes/501/x.app");
    let registered = [
        far.clone(),
        apps.join("x.app"),
        trashed.clone(),
        volume_trash.clone(),
    ];
    let c = classify_claimants(
        Some(&running),
        &roots,
        Some(&registered),
        list,
        as_spelled,
        read,
    );
    for trash in [&trashed, &volume_trash] {
        assert!(
            !c.found.iter().any(|x| x.path == *trash),
            "a copy in the Trash is not a claimant, though LaunchServices still lists it"
        );
    }

    assert_eq!(
        c.enumeration,
        Enumeration::Complete,
        "a missing /Applications is empty"
    );
    assert_eq!(c.found.len(), 5, "{:?}", c.found);
    assert!(c.found[0].running && c.found[0].path == running);
    assert!(
        c.found.iter().any(|x| x.path == far),
        "LaunchServices' copy is found"
    );
    assert_eq!(c.conflicting().len(), 4);
    let retirable: Vec<_> = c.retirable().iter().map(|x| x.path.clone()).collect();
    assert_eq!(
        retirable,
        vec![apps.join("x-backup.app"), apps.join("x.app.rollback")],
        "an unreadable signature is listed as conflicting and never offered"
    );
    assert!(!c.sole_claimant());
}

#[test]
fn every_hole_downgrades_the_census_and_a_complete_lone_install_is_sole() {
    let apps = PathBuf::from("/Apps");
    let running = apps.join("x.app");
    let lone = |dir: &Path| {
        if dir == apps {
            Listing::Entries(vec![apps.join("x.app")])
        } else {
            Listing::Missing
        }
    };
    let roots = vec![
        (apps.clone(), true),
        (PathBuf::from("/Applications"), false),
    ];
    let only = |root: &Path| {
        if root == running.as_path() {
            ours(DEV_ID)
        } else {
            Candidate::NotOurs
        }
    };

    let complete = classify_claimants(Some(&running), &roots, Some(&[]), lone, as_spelled, only);
    assert!(complete.sole_claimant() && complete.conflicting().is_empty());

    let no_ls = classify_claimants(Some(&running), &roots, None, lone, as_spelled, only);
    assert_eq!(
        no_ls.enumeration,
        Enumeration::Partial,
        "LaunchServices unasked"
    );
    assert!(!no_ls.sole_claimant());

    let required_missing = classify_claimants(
        Some(&running),
        &roots,
        Some(&[]),
        |_| Listing::Missing,
        as_spelled,
        only,
    );
    assert_eq!(
        required_missing.enumeration,
        Enumeration::Partial,
        "the running bundle's own parent is required"
    );
    assert_eq!(
        required_missing.found.len(),
        1,
        "the running copy is still counted"
    );

    let unreadable_bundle = classify_claimants(
        Some(&running),
        &roots,
        Some(&[apps.join("y.app")]),
        lone,
        as_spelled,
        |root: &Path| {
            if root == running.as_path() {
                ours(DEV_ID)
            } else {
                Candidate::Hole
            }
        },
    );
    assert_eq!(unreadable_bundle.enumeration, Enumeration::Partial);

    let blind = classify_claimants(None, &[], None, lone, as_spelled, |_| Candidate::NotOurs);
    assert_eq!(blind.enumeration, Enumeration::Unavailable);
    assert!(
        blind.conflicting().is_empty(),
        "no reference identity, no claim"
    );
    let nothing_listed = classify_claimants(
        Some(&running),
        &[(apps.clone(), true)],
        None,
        |_| Listing::Unreadable,
        as_spelled,
        only,
    );
    assert_eq!(
        nothing_listed.enumeration,
        Enumeration::Partial,
        "nothing listed, but the running copy was read"
    );
}

#[test]
fn a_bundle_is_reported_once_under_its_own_name() {
    let (link, own, resolved) = (p("/Links/x.app"), p("/Apps/x.app"), p("/real/x.app"));
    let firmlink = p("/Data/Apps/x.app");
    let locate = |spelled: &Path| {
        let via_link = spelled == Path::new("/Links/x.app");
        let named = spelled == Path::new("/Apps/x.app") || spelled == Path::new("/Data/Apps/x.app");
        (via_link || named).then(|| Located {
            file: (1, 1),
            path: if via_link {
                p("/real/x.app")
            } else {
                spelled.to_path_buf()
            },
            link: via_link,
        })
    };
    let c = classify_claimants(
        Some(&link),
        &[(p("/Links"), true), (p("/Apps"), false)],
        Some(&[link.clone(), firmlink]),
        |dir: &Path| Listing::Entries(vec![dir.join("x.app")]),
        locate,
        |_: &Path| ours(DEV_ID),
    );
    assert_eq!(c.found.len(), 1, "{:?}", c.found);
    let copy = &c.found[0];
    assert_eq!(copy.path, own, "not {resolved:?}");
    assert_eq!(copy.links, [link]);
    assert!(copy.running);
}

#[test]
fn only_an_ad_hoc_or_unsigned_copy_is_retirable() {
    let running = PathBuf::from("/A/x.app");
    let [pinned, adhoc, unsigned, unread] = [
        "x-pinned.app",
        "x-adhoc.app",
        "x-unsigned.app",
        "x-unread.app",
    ]
    .map(|name| PathBuf::from("/A").join(name));
    let entries = vec![
        running.clone(),
        pinned.clone(),
        adhoc.clone(),
        unsigned.clone(),
        unread.clone(),
    ];
    let c = classify_claimants(
        Some(&running),
        &[(PathBuf::from("/A"), true)],
        Some(&[]),
        |_| Listing::Entries(entries.clone()),
        as_spelled,
        |root: &Path| {
            let report = if root == running.as_path() {
                return ours(DEV_ID);
            } else if root == pinned.as_path() {
                return Candidate::Ours(BundleIdentity {
                    bundle_id: Some("x".to_string()),
                    dr_text: "cdhash H\"aa\"".to_string(),
                    dr: DrClass::Cdhash,
                    signing: "developer-id",
                    team: Some("T".to_string()),
                });
            } else if root == adhoc.as_path() {
                return ours("designated => cdhash H\"bb\"");
            } else if root == unsigned.as_path() {
                Some("/A/x-unsigned.app: code object is not signed at all\n")
            } else {
                None
            };
            Candidate::Ours(identity_from_report(report, Some("x".to_string())))
        },
    );
    assert_eq!(c.conflicting().len(), 4);
    let retirable: Vec<&Path> = c.retirable().iter().map(|c| c.path.as_path()).collect();
    assert_eq!(retirable, vec![adhoc.as_path(), unsigned.as_path()]);
    use TrashOffer::{Linked, NoTool, Offered, Requirement, RunningUnstable, Signature};
    let offers = |trash_tool| -> Vec<TrashOffer> {
        c.listed(trash_tool)
            .into_iter()
            .map(|(_, offer)| offer)
            .collect()
    };
    assert_eq!(offers(true), [Offered, Signature, Signature, Offered]);
    assert_eq!(offers(false), [NoTool, Signature, Signature, NoTool]);

    let none: &[&str] = &[];
    for (signing, dr, links, stable, want) in [
        ("adhoc", DrClass::Cdhash, none, true, Offered),
        ("unsigned", DrClass::Unsigned, none, true, Offered),
        ("adhoc", DrClass::Identity, none, true, Requirement),
        ("adhoc", DrClass::Unknown, none, true, Requirement),
        ("developer-id", DrClass::Cdhash, none, true, Signature),
        ("developer-id", DrClass::Unsigned, none, true, Signature),
        ("unsigned", DrClass::Unknown, none, true, Signature),
        ("unknown", DrClass::Cdhash, none, true, Signature),
        (
            "adhoc",
            DrClass::Cdhash,
            &["/Applications/x-dev.app"],
            true,
            Linked,
        ),
        ("adhoc", DrClass::Cdhash, none, false, RunningUnstable),
    ] {
        let copy = Claimant {
            path: PathBuf::from("/A/x-copy.app"),
            dr,
            dr_text: "r".to_string(),
            signing,
            team: None,
            running: false,
            links: links.iter().map(PathBuf::from).collect(),
        };
        assert_eq!(
            TrashOffer::of(&copy, stable),
            want,
            "{signing} {dr:?} {links:?}"
        );
    }

    let claimant = |name: &str, dr_text: &str, running: bool| Claimant {
        path: PathBuf::from("/A").join(name),
        dr: classify_dr(dr_text),
        dr_text: dr_text.to_string(),
        signing: if dr_text.contains("cdhash") {
            "adhoc"
        } else {
            "developer-id"
        },
        team: None,
        running,
        links: Vec::new(),
    };
    let census = |found| Claimants {
        found,
        enumeration: Enumeration::Complete,
    };
    let paths = |copies: Vec<&Claimant>| -> Vec<PathBuf> {
        copies.into_iter().map(|c| c.path.clone()).collect()
    };
    let (aa, bb) = (
        "designated => cdhash H\"aa\"",
        "designated => cdhash H\"bb\"",
    );
    let unread_running = census(vec![
        claimant("x.app", "", true),
        claimant("y.app", DEV_ID, false),
    ]);
    assert_eq!(paths(unread_running.conflicting()), [p("/A/y.app")]);
    let twin = census(vec![
        claimant("x.app", DEV_ID, true),
        claimant("y.app", DEV_ID, false),
    ]);
    assert!(twin.conflicting().is_empty());
    let headless = census(vec![
        claimant("x.app", DEV_ID, false),
        claimant("y.app", aa, false),
    ]);
    assert!(headless.conflicting().is_empty());
    let unstable = census(vec![
        claimant("x.app", aa, true),
        claimant("y.app", DEV_ID, false),
        claimant("z.app", bb, false),
    ]);
    assert!(
        unstable.retirable().is_empty(),
        "a stable copy that is not running does not steady the running one"
    );
}

#[test]
fn only_a_shown_still_offered_idle_copy_is_moved_to_the_trash() {
    let dir = PathBuf::from("/A");
    let running = dir.join("x.app");
    let [rollback, busy, unsure, refused, unshown, resigned] = [
        "x.app.rollback",
        "x-busy.app",
        "x-unsure.app",
        "x-refused.app",
        "x-unshown.app",
        "x-resigned.app",
    ]
    .map(|name| dir.join(name));
    let gone = dir.join("x-gone.app");
    let entries = vec![
        running.clone(),
        rollback.clone(),
        busy.clone(),
        unsure.clone(),
        refused.clone(),
        unshown.clone(),
        resigned.clone(),
    ];
    let fresh = classify_claimants(
        Some(&running),
        &[(dir.clone(), true)],
        Some(&[]),
        |_| Listing::Entries(entries.clone()),
        as_spelled,
        |root: &Path| {
            if root == running.as_path() {
                ours(DEV_ID)
            } else {
                ours("designated => cdhash H\"aa\"")
            }
        },
    );
    let trashed = std::cell::RefCell::new(Vec::new());
    let plan = [
        &rollback, &busy, &unsure, &refused, &resigned, &gone, &running,
    ]
    .map(|p| (*p).clone());
    let out = retire_claimants(
        &plan,
        &fresh,
        |c| c.path != resigned,
        |p| {
            if p == busy {
                Some(true)
            } else if p == unsure {
                None
            } else {
                Some(false)
            }
        },
        |p| {
            trashed.borrow_mut().push(p.to_path_buf());
            if p == refused {
                Err("denied".to_string())
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(
        out,
        vec![
            (rollback.clone(), Retired::Moved),
            (busy, Retired::InUse),
            (unsure, Retired::InUse),
            (refused.clone(), Retired::Failed("denied".to_string())),
            (resigned, Retired::NotOffered),
            (gone, Retired::NotOffered),
            (running, Retired::NotOffered),
        ]
    );
    assert_eq!(*trashed.borrow(), vec![rollback, refused]);
    assert!(
        !trashed.borrow().contains(&unshown),
        "never outside the plan"
    );
}

#[cfg(unix)]
#[test]
fn the_census_does_not_look_inside_a_protected_root() {
    let scratch = crate::test_scratch_dir("census-prot");
    let dir = scratch.path().canonicalize().unwrap();
    let documents = dir.join("Documents");
    let plist = "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>";
    let hidden = documents.join("x.app");
    std::fs::create_dir_all(hidden.join("Contents")).unwrap();
    std::fs::write(hidden.join("Contents/Info.plist"), plist).unwrap();
    let apps = dir.join("Applications");
    std::fs::create_dir_all(&apps).unwrap();
    let protected = vec![documents.clone()];

    assert!(!census_may_read(&hidden, &protected));
    assert_eq!(
        list_unless_protected(&documents, &protected),
        Listing::Unreadable
    );
    assert!(census_may_read(&apps, &protected));
    assert!(matches!(
        list_unless_protected(&apps, &protected),
        Listing::Entries(_)
    ));

    let link = apps.join("x.app");
    std::os::unix::fs::symlink(&hidden, &link).unwrap();
    assert!(is_under_protected_root(&hidden, &protected));
    assert!(
        !is_under_protected_root(&link, &protected),
        "not as spelled"
    );
    assert!(!census_may_read(&link, &protected), "but once resolved");
    assert_eq!(read_candidate(&link, "x", &protected), Candidate::Hole);

    let shell = apps.join("y.app");
    std::fs::create_dir_all(&shell).unwrap();
    std::os::unix::fs::symlink(hidden.join("Contents"), shell.join("Contents")).unwrap();
    assert!(
        census_may_read(&shell, &protected),
        "the bundle itself is outside"
    );
    assert_eq!(read_candidate(&shell, "x", &protected), Candidate::Hole);
    assert_eq!(bundle_identity(&shell, &protected), None);

    let exe_link = apps.join("z.app");
    std::fs::create_dir_all(exe_link.join("Contents/MacOS")).unwrap();
    std::fs::write(
        exe_link.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string>\
         <key>CFBundleExecutable</key><string>z</string></dict></plist>",
    )
    .unwrap();
    std::fs::write(documents.join("z"), b"").unwrap();
    std::os::unix::fs::symlink(documents.join("z"), exe_link.join("Contents/MacOS/z")).unwrap();
    assert_eq!(
        read_candidate(&exe_link, "x", &protected),
        Candidate::Hole,
        "codesign would open the executable under the root"
    );
    assert_eq!(bundle_identity(&exe_link, &protected), None);

    let sig_link = apps.join("s.app");
    std::fs::create_dir_all(sig_link.join("Contents/_CodeSignature")).unwrap();
    std::fs::write(
        sig_link.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )
    .unwrap();
    let resources = sig_link.join("Contents/_CodeSignature/CodeResources");
    std::os::unix::fs::symlink(documents.join("z"), &resources).unwrap();
    assert_eq!(read_candidate(&sig_link, "x", &protected), Candidate::Hole);
    assert_eq!(bundle_identity(&sig_link, &protected), None);
    std::fs::remove_file(&resources).unwrap();
    std::fs::write(&resources, b"").unwrap();
    assert!(
        !bundle_reaches_protected(&sig_link, &protected),
        "the control: the same bundle without the link is vouched for"
    );

    let away = dir.join("away");
    std::fs::create_dir_all(&away).unwrap();
    std::fs::write(
        away.join("Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )
    .unwrap();
    let w = apps.join("w.app");
    std::fs::create_dir_all(&w).unwrap();
    std::os::unix::fs::symlink(&away, w.join("Contents")).unwrap();
    assert!(bundle_reaches_protected(&w, &protected));
    assert_eq!(read_candidate(&w, "x", &protected), Candidate::Hole);
    assert_eq!(bundle_identity(&w, &protected), None);
    let other = apps.join("o.app");
    std::fs::create_dir_all(other.join("Contents")).unwrap();
    std::fs::write(
        documents.join("other.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>other</string></dict></plist>",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        documents.join("other.plist"),
        other.join("Contents/Info.plist"),
    )
    .unwrap();
    assert_eq!(read_candidate(&other, "x", &protected), Candidate::Hole);
    assert_eq!(bundle_id_of(&other, &protected), None);
    let bare = apps.join("b.app");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(documents.join("no-plist")).unwrap();
    std::os::unix::fs::symlink(documents.join("no-plist"), bare.join("Contents")).unwrap();
    assert!(bundle_reaches_protected(&bare, &protected));
    assert_eq!(bundle_identity(&bare, &protected), None);
    let into = apps.join("n.app");
    std::os::unix::fs::symlink(documents.join("no-plist"), &into).unwrap();
    assert_eq!(read_candidate(&into, "x", &protected), Candidate::Hole);
    let q = documents.join("q.app");
    std::fs::create_dir_all(&q).unwrap();
    std::os::unix::fs::symlink(&away, q.join("Contents")).unwrap();
    let q_link = apps.join("q.app");
    std::os::unix::fs::symlink(&q, &q_link).unwrap();
    assert_eq!(bundle_id_of(&q_link, &protected), None);
    assert!(bundle_reaches_protected(&hidden, &protected));
    let cyc = apps.join("c.app");
    std::fs::create_dir_all(cyc.join("Contents/MacOS")).unwrap();
    std::fs::write(cyc.join("Contents/MacOS/aterm"), b"").unwrap();
    std::os::unix::fs::symlink("aterm", cyc.join("Contents/MacOS/atpkg")).unwrap();
    std::os::unix::fs::symlink(cyc.join("Contents"), cyc.join("Contents/loop")).unwrap();
    assert!(!bundle_reaches_protected(&cyc, &protected));
    assert!(
        bundle_identity(&cyc, &protected).is_some(),
        "the control: vouched for, so read"
    );
    let alias = dir.join("apps-alias");
    std::os::unix::fs::symlink(&apps, &alias).unwrap();
    let spelled = alias.join("c.app");
    assert_eq!(
        bundle_identity(&spelled, &[spelled.join("Contents/Info.plist")]),
        None
    );
}

#[cfg(unix)]
#[test]
fn the_walk_stays_inside_the_bundles_own_contents() {
    let scratch = crate::test_scratch_dir("census-reach");
    let dir = scratch.path().canonicalize().unwrap();
    let apps = dir.join("Applications");
    std::fs::create_dir_all(&apps).unwrap();
    let lay = |at: &Path| {
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        std::fs::write(
            at.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        std::fs::write(at.join("Contents/MacOS/x"), b"").unwrap();
    };

    let home = dir.join("home");
    lay(&home);
    let r = apps.join("r.app");
    std::os::unix::fs::symlink(&home, &r).unwrap();
    assert!(bundle_reaches_protected(&r, &[]));
    let named = dir.join("home.app");
    lay(&named);
    let r2 = apps.join("r2.app");
    std::os::unix::fs::symlink(&named, &r2).unwrap();
    assert!(
        !bundle_reaches_protected(&r2, &[]),
        "the control: the same link, to a folder named like a bundle"
    );

    let p = apps.join("p.app");
    lay(&p);
    std::fs::create_dir_all(p.join("Contents/Private")).unwrap();
    std::fs::write(p.join("Contents/Private/secret.txt"), b"").unwrap();
    assert!(bundle_reaches_protected(&p, &[p.join("Contents/Private")]));
    assert!(
        !bundle_reaches_protected(&p, &[]),
        "the control: nothing protected inside it"
    );
    assert!(bundle_reaches_protected(&p, &[p.join("Contents/Absent")]));
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&apps, &alias).unwrap();
    assert!(bundle_reaches_protected(
        &p,
        &[alias.join("p.app/Contents/Private")]
    ));

    let t = apps.join("t.app");
    std::fs::create_dir_all(t.join("Contents")).unwrap();
    std::fs::write(
        t.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )
    .unwrap();
    std::os::unix::fs::symlink("..", t.join("Contents/MacOS")).unwrap();
    assert!(bundle_reaches_protected(&t, &[]));
    let t2 = apps.join("t2.app");
    lay(&t2);
    std::os::unix::fs::symlink("MacOS", t2.join("Contents/Alias")).unwrap();
    assert!(
        !bundle_reaches_protected(&t2, &[]),
        "the control: a link that stays inside Contents"
    );

    let platform = apps.join("i.app");
    lay(&platform);
    std::fs::write(platform.join("Contents/Info-macos.plist"), b"").unwrap();
    assert!(bundle_reaches_protected(&platform, &[]));
    let lower = apps.join("i2.app");
    lay(&lower);
    std::fs::remove_file(lower.join("Contents/Info.plist")).unwrap();
    std::fs::write(
        lower.join("Contents/info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )
    .unwrap();
    assert!(bundle_reaches_protected(&lower, &[]));
    let forged = apps.join("c\nSignature=adhoc\n.app");
    lay(&forged);
    assert!(bundle_reaches_protected(&forged, &[]));
    let top_link = apps.join("l.app");
    lay(&top_link);
    std::os::unix::fs::symlink(dir.join("home/Contents/MacOS/x"), top_link.join("x")).unwrap();
    assert!(bundle_reaches_protected(&top_link, &[]));
    let top_dir = apps.join("f.app");
    lay(&top_dir);
    std::fs::create_dir_all(top_dir.join("Stuff")).unwrap();
    assert!(bundle_reaches_protected(&top_dir, &[]));
    let top_file = apps.join("g.app");
    lay(&top_file);
    std::fs::write(top_file.join("Icon\r"), b"").unwrap();
    assert!(
        !bundle_reaches_protected(&top_file, &[]),
        "the control: a plain file at the top level"
    );
    let carriage = apps.join("r\r.app");
    lay(&carriage);
    assert!(bundle_reaches_protected(&carriage, &[]));
    let next_line = apps.join(format!("e{}.app", char::from(0x85_u8)));
    lay(&next_line);
    assert!(
        bundle_reaches_protected(&next_line, &[]),
        "a control character outside ASCII"
    );
    let plain = dir.join("plain.app");
    lay(&plain);
    let crooked_link = apps.join("n\n.app");
    std::os::unix::fs::symlink(&plain, &crooked_link).unwrap();
    assert!(bundle_reaches_protected(&crooked_link, &[]));
    let plain_link = apps.join("plain-link.app");
    std::os::unix::fs::symlink(&forged, &plain_link).unwrap();
    assert!(bundle_reaches_protected(&plain_link, &[]));
    assert!(bundle_reaches_protected(&apps.join("gone.app"), &[]));
    let hollow = apps.join("hollow.app");
    std::fs::create_dir_all(&hollow).unwrap();
    assert!(bundle_reaches_protected(&hollow, &[]));
    #[cfg(target_os = "macos")]
    {
        let folder_plist = apps.join("fp.app");
        std::fs::create_dir_all(folder_plist.join("Contents/Info.plist")).unwrap();
        assert!(bundle_reaches_protected(&folder_plist, &[]));
    }
    if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&apps).unwrap()) != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        let shut = apps.join("shut.app");
        std::fs::create_dir_all(shut.join("Contents")).unwrap();
        std::fs::write(shut.join("Contents/Info.plist"), b"").unwrap();
        let mode = |bits| {
            let mode = std::fs::Permissions::from_mode(bits);
            std::fs::set_permissions(shut.join("Contents"), mode).unwrap();
        };
        mode(0o444);
        let refused = bundle_reaches_protected(&shut, &[]);
        mode(0o755);
        assert!(
            refused,
            "a listable Contents whose plist cannot be looked at"
        );
        for (name, shut, bits) in [
            ("sealed.app", ".", 0o100),
            ("closed.app", "Contents/Resources", 0o000),
        ] {
            let app = apps.join(name);
            lay(&app);
            std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
            let set = |bits| {
                let mode = std::fs::Permissions::from_mode(bits);
                std::fs::set_permissions(app.join(shut), mode).unwrap();
            };
            set(bits);
            let refused = bundle_reaches_protected(&app, &[]);
            set(0o755);
            assert!(refused, "{name}");
            assert!(!bundle_reaches_protected(&app, &[]), "the control: {name}");
        }
    }
    let nested = apps.join("nested.app");
    lay(&nested);
    std::fs::create_dir_all(nested.join("Contents/Resources")).unwrap();
    std::fs::write(nested.join("Contents/Resources/Info-macos.plist"), b"").unwrap();
    std::fs::write(nested.join("Contents/version.plist"), b"").unwrap();
    std::fs::write(nested.join("Contents/Info.txt"), b"").unwrap();
    assert!(!bundle_reaches_protected(&nested, &[]));
    let deep = apps.join("deep.app");
    lay(&deep);
    let sixteen = (0..16).fold(deep.join("Contents"), |at, _| at.join("d"));
    std::fs::create_dir_all(&sixteen).unwrap();
    assert!(!bundle_reaches_protected(&deep, &[]));
    std::fs::create_dir_all(sixteen.join("d")).unwrap();
    assert!(bundle_reaches_protected(&deep, &[]));
    let wide = apps.join("wide.app");
    std::fs::create_dir_all(wide.join("Contents")).unwrap();
    let full = apps.join("full.app");
    std::fs::create_dir_all(full.join("Contents")).unwrap();
    for name in ["a", "b"] {
        std::fs::write(wide.join(name), b"").unwrap();
        std::fs::write(full.join("Contents").join(name), b"").unwrap();
    }
    assert!(!bundle_reaches_protected_on(&wide, &[], device_of, 3));
    assert!(!bundle_reaches_protected_on(&full, &[], device_of, 3));
    std::fs::write(wide.join("c"), b"").unwrap();
    std::fs::write(full.join("Contents/c"), b"").unwrap();
    assert!(bundle_reaches_protected_on(&wide, &[], device_of, 3));
    assert!(bundle_reaches_protected_on(&full, &[], device_of, 3));
}

#[cfg(target_os = "macos")]
#[test]
fn the_walk_reads_the_plist_as_codesign_does() {
    let scratch = crate::test_scratch_dir("census-plist");
    let dir = scratch.path().canonicalize().unwrap();
    let refused = |app: &str, plist: &str| {
        let at = dir.join(app);
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        std::fs::write(at.join("Contents/MacOS/x"), b"").unwrap();
        std::fs::write(at.join("Contents/Info.plist"), plist).unwrap();
        bundle_reaches_protected(&at, &[])
    };
    let dict = |body: &str| {
        format!("<plist><dict><key>CFBundleIdentifier</key><string>x</string>{body}</dict></plist>")
    };
    let exe = |value: &str| format!("<key>CFBundleExecutable</key><string>{value}</string>");
    let abs = "<string>/Users//u/Documents/z</string>";
    assert!(refused("e1.app", &dict(&exe("/Users//u/Documents/z"))));
    assert!(refused(
        "e6.app",
        &dict(&exe("&#47;Users&#47;u&#47;Documents&#47;z"))
    ));
    assert!(refused(
        "e7.app",
        &dict(&format!("<key><![CDATA[CFBundleExecutable]]></key>{abs}"))
    ));
    assert!(refused(
        "e9.app",
        &dict("<key>CFBundleExecutable</key><array/>")
    ));
    assert!(refused(
        "e11.app",
        &dict(&format!("<key >CFBundleExecutable</key>{abs}"))
    ));
    assert!(refused(
        "e12.app",
        &dict(&format!("<key>CFBundleExecutable</key >{abs}"))
    ));
    assert!(refused(
        "e13.app",
        &dict(&format!(
            "{}<key>CFBundleExecutable-macos</key>{abs}",
            exe("x")
        ))
    ));
    assert!(refused(
        "e15.app",
        &dict(&format!("<key>NSExecutable</key>{abs}"))
    ));
    assert!(refused("e18.app", &dict(&exe(&"a".repeat(2000)))));
    assert!(!refused(
        "e19.app",
        &dict(&format!("<key>{}Executable</key>{abs}", "x".repeat(200)))
    ));
    let utf7 = "+ADw-key+AD4-CFBundleExecutable+ADw-/key+AD4-\
                +ADw-string+AD4-/Users//u/Documents/z+ADw-/string+AD4-";
    for (app, declaration) in [
        ("d1.app", "<?xml version=\"1.0\" encoding=\"UTF-7\"?>"),
        (
            "d3.app",
            "<?xml version=\"1.0\" encoding =\"UTF-8\" encoding=\"UTF-7\"?>",
        ),
    ] {
        assert!(
            refused(app, &format!("{declaration}{}", dict(utf7))),
            "{app}"
        );
    }
    assert!(refused(
        "n.app",
        "{ CFBundleIdentifier = x; CFBundleExecutable = \"/Users//u/Documents/z\"; }"
    ));
    for (app, plist) in [
        (
            "e14.app",
            dict(&format!("<key>CFBundleExec<!-- -->utable</key>{abs}")),
        ),
        (
            "e16.app",
            dict(&format!("<string>CFBundleExecutable</string>{abs}")),
        ),
        (
            "j.app",
            "{\"CFBundleExecutable\": \"/Users//u/Documents/z\"}".to_string(),
        ),
    ] {
        assert!(!refused(app, &plist), "{app}");
    }
    assert!(!refused("e10.app", &dict(&exe("x"))), "a plain name");
    assert!(
        !refused(
            "k.app",
            &dict(&format!("<key>NSHumanReadableCopyright</key>{abs}"))
        ),
        "a key naming no executable may hold a path"
    );
    assert!(
        !refused(
            "e8.app",
            &dict(&format!(
                "<!-- {} -->{}",
                exe("/Users//u/Documents/z"),
                exe("x")
            ))
        ),
        "a key inside a comment is no key"
    );
    assert!(
        !refused(
            "d2.app",
            &format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
                dict(&exe("x"))
            )
        ),
        "UTF-8 declared"
    );
}

#[test]
fn only_plain_file_names_are_plain() {
    assert!(plain_file_name("aterm"));
    for name in [
        "/Users//u/Documents/z",
        "../z",
        "",
        "MacOS/x",
        "..",
        "x\nSignature=adhoc",
    ] {
        assert!(!plain_file_name(name), "{name:?}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_binary_info_plist_answers_whose_bundle_it_is() {
    let scratch = crate::test_scratch_dir("census-bin");
    let dir = scratch.path().canonicalize().unwrap();
    let binary = |app: &str, id: &str| {
        let at = dir.join(app);
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        let plist = at.join("Contents/Info.plist");
        std::fs::write(
            &plist,
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"
            ),
        )
        .unwrap();
        let converted = aterm_uds::spawnfd::command("/usr/bin/plutil")
            .args(["-convert", "binary1"])
            .arg(&plist)
            .status()
            .expect("plutil runs");
        assert!(converted.success());
        assert!(
            read_plist_text(&plist).is_none(),
            "the text reader cannot read it"
        );
        at
    };
    let other = binary("Numbers.app", "com.apple.iWork.Numbers");
    assert_eq!(read_candidate(&other, "x", &[]), Candidate::NotOurs);
    let ours = binary("x.app", "x");
    assert!(matches!(
        read_candidate(&ours, "x", &[]),
        Candidate::Ours(BundleIdentity { bundle_id: Some(ref id), .. }) if id == "x"
    ));
    let variant = |app: &str, plain: &str, macos: &str| {
        let at = dir.join(app);
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        std::fs::write(
            at.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{plain}</string>\
                 <key>CFBundleIdentifier-macos</key><string>{macos}</string></dict></plist>"
            ),
        )
        .unwrap();
        at
    };
    assert!(matches!(
        read_candidate(&variant("v1.app", "other", "x"), "x", &[]),
        Candidate::Ours(_)
    ));
    assert_eq!(
        read_candidate(&variant("v2.app", "x", "other"), "x", &[]),
        Candidate::NotOurs
    );
    let unusable = dir.join("v3.app");
    std::fs::create_dir_all(unusable.join("Contents/MacOS")).unwrap();
    std::fs::write(
        unusable.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string>\
         <key>CFBundleIdentifier-macos</key><integer>1</integer></dict></plist>",
    )
    .unwrap();
    assert_eq!(read_candidate(&unusable, "x", &[]), Candidate::NotOurs);
    let json = dir.join("j.app");
    std::fs::create_dir_all(json.join("Contents/MacOS")).unwrap();
    std::fs::write(
        json.join("Contents/Info.plist"),
        "{\"CFBundleIdentifier\": \"x\", \"CFBundlePackageType\": \"APPL\"}",
    )
    .unwrap();
    assert_eq!(read_candidate(&json, "x", &[]), Candidate::NotOurs);
    let nul = binary("n.app", "x");
    let mut bplist = b"bplist00\xd1\x01\x02\x5f\x10\x12CFBundleIdentifier\x56x\0junk".to_vec();
    bplist.extend_from_slice(&[8, 11, 32, 0, 0, 0, 0, 0, 0, 1, 1]);
    bplist.extend_from_slice(&3u64.to_be_bytes());
    bplist.extend_from_slice(&0u64.to_be_bytes());
    bplist.extend_from_slice(&39u64.to_be_bytes());
    std::fs::write(nul.join("Contents/Info.plist"), &bplist).unwrap();
    assert_eq!(read_candidate(&nul, "x", &[]), Candidate::NotOurs);
}

#[cfg(target_os = "macos")]
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_os = "macos")]
#[test]
fn a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling() {
    let dir = crate::test_scratch_dir("pipe-ceiling");
    let gate = fifo_gate(dir.path());
    let _detector = gate_hang_detector(&gate);
    let armed = gate.clone();
    READERS_PENDING.with(|acts| {
        let mut acts = acts.borrow_mut();
        acts.push_back(Box::new(|deadline| *deadline = std::time::Instant::now()));
        acts.push_back(Box::new(move |_| release_gate(&armed).unwrap()));
    });
    let run = run_bounded_exit(
        "/bin/sh",
        &[
            "-c".as_ref(),
            GATED_GRANDCHILD.as_ref(),
            "sh".as_ref(),
            gate.as_os_str(),
        ],
        HANG_DETECTOR,
        TOOL_OUTPUT_MAX_BYTES,
    );
    let unrun = READERS_PENDING.with(|acts| acts.borrow_mut().drain(..).count());
    let held = release_gate(&gate);
    assert_eq!(run, ToolRun::TimedOut);
    assert_eq!(unrun, 1, "the wait ended at the poll past its ceiling");
    assert!(held.is_ok(), "the grandchild held the pipes: {held:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_fda_probe_descriptor_is_close_on_exec_from_its_open() {
    use std::os::fd::AsRawFd as _;
    let scratch = crate::test_scratch_dir("fda-probe");
    let path = scratch.path().join("fda-probe");
    std::fs::write(&path, b"").unwrap();
    let fd = imp::probe_descriptor(&path).expect("the probe opens a readable file");
    // SAFETY: F_GETFD only reads the flags of the descriptor `fd` owns.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    drop(fd);
    assert_eq!(imp::open_probe(&path), ProbeOutcome::Ok);
    std::fs::remove_file(&path).unwrap();
    assert!(flags >= 0, "F_GETFD failed");
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "the probe's descriptor must not survive a concurrent spawn's exec"
    );
    assert_eq!(
        imp::open_probe(&path),
        ProbeOutcome::Errno(libc::ENOENT),
        "a missing file is its errno"
    );
}

#[test]
fn the_open_probe_child_line_reads_back_as_its_outcome() {
    for outcome in [
        ProbeOutcome::Ok,
        ProbeOutcome::Errno(ERRNO_EPERM),
        ProbeOutcome::Errno(ERRNO_ENOENT),
        ProbeOutcome::Errno(13),
    ] {
        let printed = format!("{}\n", open_probe_line(outcome));
        assert_eq!(
            parse_open_probe_line(&printed),
            Some(outcome),
            "{printed:?}"
        );
    }
    for garbage in [
        "",
        "\n",
        "OK\n",
        "okay\n",
        "errno\n",
        "errno x\n",
        "errno 1 2\n",
        "error 13\n",
        "ok \n",
        "ok\r\n",
    ] {
        assert_eq!(parse_open_probe_line(garbage), None, "{garbage:?}");
    }
    let scratch = crate::test_scratch_dir("open-verb");
    let missing = scratch.path().join("absent-open-verb");
    for args in [
        vec![],
        vec![missing.clone().into_os_string(), missing.into_os_string()],
    ] {
        assert_eq!(
            format!("{:?}", run_open_probe_verb(&args)),
            format!("{:?}", std::process::ExitCode::FAILURE),
            "the verb wants one path"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_child_that_cannot_answer_leaves_the_open_to_this_process() {
    let scratch = crate::test_scratch_dir("open-child");
    let path = scratch.path().join("open-child");
    std::fs::write(&path, b"").unwrap();
    let missing = Path::new("/nonexistent/aterm.app/Contents/MacOS/aterm");
    // A child that runs to its end must be heard out: past `OPEN_PROBE_CEILING` a loaded
    // machine would read as a hung child, so these wait on the hang detector instead.
    for exe in [None, Some(missing), Some(Path::new("/usr/bin/false"))] {
        assert_eq!(
            open_probe_in_child_bounded(exe, &path, HANG_DETECTOR),
            ProbeOutcome::Ok,
            "{exe:?}"
        );
    }
    let stand_in = |name: &str, code: u8| {
        use std::os::unix::fs::PermissionsExt as _;
        let exe = scratch.path().join(name);
        std::fs::write(&exe, format!("#!/bin/sh\necho 'errno 13'\nexit {code}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    };
    let (answers, fails) = (stand_in("answers", 0), stand_in("fails", 1));
    assert_eq!(
        open_probe_in_child_bounded(Some(&answers), &path, HANG_DETECTOR),
        ProbeOutcome::Errno(13)
    );
    assert_eq!(
        open_probe_in_child_bounded(Some(&fails), &path, HANG_DETECTOR),
        ProbeOutcome::Ok
    );
    let hangs = {
        use std::os::unix::fs::PermissionsExt as _;
        let exe = scratch.path().join("hangs");
        std::fs::write(&exe, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    };
    assert_eq!(
        open_probe_in_child_bounded(Some(&hangs), &path, std::time::Duration::from_millis(50),),
        ProbeOutcome::Errno(libc::ETIMEDOUT)
    );
    let folder = scratch.path().to_str().expect("utf-8 scratch");
    assert_eq!(
        run_bounded_exit(folder, &[], HANG_DETECTOR, TOOL_OUTPUT_MAX_BYTES),
        ToolRun::SpawnFailed,
        "a folder is there but runs nothing"
    );
    std::fs::remove_file(&path).expect("remove only this probe fixture");
    assert_eq!(
        open_probe_in_child(Some(missing), &path),
        ProbeOutcome::Errno(libc::ENOENT)
    );
}

#[cfg(unix)]
#[test]
fn the_walk_does_not_cross_into_another_volume() {
    let scratch = crate::test_scratch_dir("walk-vol");
    let bundle = scratch.path().canonicalize().unwrap().join("m.app");
    let mount = bundle.join("Contents/Vol");
    std::fs::create_dir_all(mount.join("inside")).unwrap();
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )
    .unwrap();
    let volume = device_of(&bundle).expect("the bundle's volume");
    let mounted = |path: &Path| {
        device_of(path).map(|dev| {
            if path.starts_with(&mount) {
                dev + 1
            } else {
                dev
            }
        })
    };
    assert!(
        bundle_reaches_protected_on(&bundle, &[], mounted, BUNDLE_WALK_MAX_ENTRIES),
        "a volume mounted inside the bundle"
    );
    assert!(
        !bundle_reaches_protected_on(
            &bundle,
            &[],
            |_: &Path| Some(volume),
            BUNDLE_WALK_MAX_ENTRIES
        ),
        "the control: every folder on the bundle's volume"
    );
    assert!(bundle_reaches_protected_on(
        &bundle,
        &[],
        |_: &Path| None,
        BUNDLE_WALK_MAX_ENTRIES
    ));
}

#[test]
fn the_spelled_protected_arm_refuses_on_its_own() {
    let root = PathBuf::from("/nonexistent-aterm-census/Documents");
    let protected = vec![root.clone()];
    let missing = root.join("not-yet.app");
    assert!(
        std::fs::canonicalize(&missing).is_err(),
        "nothing to resolve"
    );
    assert!(!census_may_read(&missing, &protected));
    assert_eq!(
        list_unless_protected(&missing, &protected),
        Listing::Unreadable
    );
    assert_eq!(
        list_unless_protected(Path::new("/nonexistent-aterm-census/Apps"), &protected),
        Listing::Missing,
        "outside the root, a missing folder is simply missing"
    );
}

#[test]
fn unsigned_is_the_whole_report_not_a_phrase_in_it() {
    let planted = "Executable=/A/x: code object is not signed at all/x\n\
                   Identifier=x\nSignature=adhoc\n\
                   # designated => cdhash H\"aa\"\n";
    assert_eq!(classify_signing(planted), "adhoc");
    assert_ne!(
        identity_from_report(Some(planted), None).dr,
        DrClass::Unsigned
    );
    let quoted = "Identifier=x\nTeamIdentifier=T\n\
                  designated => identifier \"code object is not signed at all\" and \
                  anchor apple generic and certificate leaf[subject.OU] = \"T\"\n";
    assert_eq!(classify_signing(quoted), "developer-id");
    assert_eq!(
        identity_from_report(Some(quoted), None).dr,
        DrClass::Unknown
    );
}

#[test]
fn the_census_lists_through_the_protected_fence() {
    let scratch = crate::test_scratch_dir("census-in");
    let dir = scratch.path().canonicalize().unwrap();
    let desktop = dir.join("Desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    std::fs::write(desktop.join("notes.txt"), b"").unwrap();
    let roots = [(desktop.clone(), true)];
    let fenced = census_in("x", None, &roots, Some(&[]), std::slice::from_ref(&desktop));
    assert_eq!(fenced.enumeration, Enumeration::Partial, "never listed");
    let open = census_in("x", None, &roots, Some(&[]), &[]);
    assert_eq!(
        open.enumeration,
        Enumeration::Complete,
        "the control: listed"
    );
}

#[test]
fn the_census_never_reads_a_copy_inside_another_apps_container() {
    let scratch = crate::test_scratch_dir("census-cont");
    let home = scratch.path().canonicalize().unwrap().join("home");
    let container_app = home.join("Library/Containers/com.other.App/Data/Applications/aterm.app");
    let group_app = home.join("Library/Group Containers/group.other/aterm.app");
    let normal_app = home.join("Applications/aterm.app");
    for bundle in [&container_app, &group_app, &normal_app] {
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>dev.aterm</string></dict></plist>",
        )
        .unwrap();
    }
    let protected = census_fence(Some(&home));
    assert!(
        protected.contains(&home.join("Library/Containers"))
            && protected.contains(&home.join("Library/Group Containers")),
        "census_fence must include both App Data roots: {protected:?}"
    );
    assert_eq!(
        read_candidate(&container_app, "dev.aterm", &protected),
        Candidate::Hole,
        "a bundle under ~/Library/Containers must not be read"
    );
    assert_eq!(
        read_candidate(&group_app, "dev.aterm", &protected),
        Candidate::Hole,
        "a bundle under ~/Library/Group Containers must not be read"
    );
    let absent_container = home.join("Library/Containers/com.apple.Notes/Data/aterm.app");
    assert_eq!(
        read_candidate(&absent_container, "dev.aterm", &protected),
        Candidate::Hole,
        "an absent path under ~/Library/Containers is still fenced as a hole"
    );
    #[cfg(unix)]
    {
        let link_in_apps = home.join("Applications/linked-aterm.app");
        std::os::unix::fs::symlink(&container_app, &link_in_apps).unwrap();
        assert_eq!(
            read_candidate(&link_in_apps, "dev.aterm", &protected),
            Candidate::Hole,
            "a symlink in ~/Applications resolving into ~/Library/Containers must not be read"
        );
        let link_home = scratch.path().canonicalize().unwrap().join("home-link");
        std::os::unix::fs::symlink(&home, &link_home).unwrap();
        let linked_fence = census_fence(Some(&link_home));
        assert_eq!(
            read_candidate(&container_app, "dev.aterm", &linked_fence),
            Candidate::Hole,
            "canonical container path must be fenced even when HOME is a symlink"
        );
    }
    assert!(
        matches!(
            read_candidate(&normal_app, "dev.aterm", &protected),
            Candidate::Ours(_)
        ),
        "control bundle in ~/Applications must still be read"
    );
    assert!(
        Folder::AppData
            .path(&home)
            .starts_with(&app_data_roots(&home)[0]),
        "Folder::AppData probe path must share APP_DATA_ROOTS[0]"
    );
}

#[test]
fn only_listed_copies_are_offered_for_the_trash() {
    let dir = PathBuf::from("/A");
    let running = dir.join("x.app");
    let stable_other = |i: usize| dir.join(format!("x-stable-{i:02}.app"));
    let mut entries = vec![running.clone()];
    entries.extend((0..MAX_CLAIMANT_ROWS).map(stable_other));
    let late = dir.join("z-adhoc.app");
    entries.push(late.clone());
    let c = classify_claimants(
        Some(&running),
        &[(dir.clone(), true)],
        Some(&[]),
        |_| Listing::Entries(entries.clone()),
        as_spelled,
        |root: &Path| {
            if root == running.as_path() {
                ours(DEV_ID)
            } else if root == late.as_path() {
                ours("designated => cdhash H\"aa\"")
            } else {
                ours(
                    "designated => identifier \"x\" and anchor apple generic and \
                      certificate leaf[subject.OU] = \"OTHER\"",
                )
            }
        },
    );
    assert_eq!(c.conflicting().len(), MAX_CLAIMANT_ROWS + 1);
    assert_eq!(c.listed(true).len(), MAX_CLAIMANT_ROWS);
    assert_eq!(c.retirable().len(), 1, "the ad-hoc copy is retirable");
    assert!(
        c.offered_for_trash(true).is_empty(),
        "but it is past the listed rows, so it is not offered"
    );
}

#[test]
fn bundle_names_include_the_rollback_suffix() {
    for yes in [
        "aterm.app",
        "aterm.app.rollback",
        "aterm.APP",
        "Some Name.app.previous",
    ] {
        assert!(looks_like_bundle(Path::new(yes)), "{yes}");
    }
    for no in [
        "notes.txt",
        "aterm",
        ".app",
        ".aterm.app",
        "apple.appointments",
        "aterm.app.",
        "",
    ] {
        assert!(!looks_like_bundle(Path::new(no)), "{no}");
    }
}

#[test]
fn the_search_roots_start_where_this_process_runs_from() {
    let home = PathBuf::from("/Users//a");
    let roots = claimant_search_roots(
        Some(Path::new("/Users//a/aterm/dist/aterm.app")),
        Some(&home),
    );
    assert_eq!(
        roots,
        vec![
            (PathBuf::from("/Users//a/aterm/dist"), true),
            (PathBuf::from("/Applications"), false),
            (home.join("Applications"), false),
        ]
    );
    let roots = claimant_search_roots(Some(Path::new("/Applications/aterm.app")), Some(&home));
    assert_eq!(roots[0], (PathBuf::from("/Applications"), true));
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert_eq!(
        claimant_search_roots(None, None),
        vec![(PathBuf::from("/Applications"), false)]
    );
}

#[test]
fn the_candidate_reader_tells_ours_from_not_a_bundle_from_a_hole() {
    let tmp = crate::test_scratch_dir("cand-reader");
    let lay = |name: &str, id: &str| {
        let contents = tmp.path().join(name).join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"
            ),
        )
        .unwrap();
    };
    lay("mine.app", "x");
    lay("theirs.app", "y");
    std::fs::create_dir_all(tmp.path().join("empty.app")).unwrap();
    std::fs::write(tmp.path().join("archive.app.zip"), b"zip").unwrap();

    assert!(matches!(
        read_candidate(&tmp.path().join("mine.app"), "x", &[]),
        Candidate::Ours(_)
    ));
    assert_eq!(
        read_candidate(&tmp.path().join("theirs.app"), "x", &[]),
        Candidate::NotOurs
    );
    assert_eq!(
        read_candidate(&tmp.path().join("empty.app"), "x", &[]),
        Candidate::NotOurs
    );
    assert_eq!(
        read_candidate(&tmp.path().join("archive.app.zip"), "x", &[]),
        Candidate::NotOurs
    );
    assert_eq!(
        read_candidate(&tmp.path().join("gone.app"), "x", &[]),
        Candidate::NotOurs
    );
    assert_eq!(
        read_candidate(
            &tmp.path().join("mine.app"),
            "x",
            &[tmp.path().to_path_buf()]
        ),
        Candidate::Hole,
        "a bundle under a protected root is not read"
    );
    assert_eq!(list_root(&tmp.path().join("nowhere")), Listing::Missing);
    assert_eq!(
        list_root(&tmp.path().join("archive.app.zip")),
        Listing::Unreadable,
        "a file is not a missing folder"
    );
    let at = |name: &str| tmp.path().join(name);
    std::fs::create_dir_all(at("flat.app")).unwrap();
    std::fs::write(at("flat.app/Contents"), b"").unwrap();
    std::fs::create_dir_all(at("huge.app/Contents")).unwrap();
    std::fs::File::create(at("huge.app/Contents/Info.plist"))
        .unwrap()
        .set_len(INFO_PLIST_MAX_BYTES + 1)
        .unwrap();
    std::fs::create_dir_all(at("folder.app/Contents/Info.plist")).unwrap();
    lay("upper.app", "X");
    // Unix reads through a file to ENOTDIR, a hole; Windows reads through one to
    // ERROR_PATH_NOT_FOUND, the answer for a bundle with no `Info.plist`.
    let through_a_file = if cfg!(windows) {
        Candidate::NotOurs
    } else {
        Candidate::Hole
    };
    for (name, want) in [
        ("flat.app", through_a_file),
        ("huge.app", Candidate::Hole),
        ("folder.app", Candidate::NotOurs),
        ("upper.app", Candidate::NotOurs),
    ] {
        assert_eq!(read_candidate(&at(name), "x", &[]), want, "{name}");
    }
    #[cfg(unix)]
    {
        std::fs::create_dir_all(at("plain-folder")).unwrap();
        std::os::unix::fs::symlink(at("plain-folder"), at("named.app")).unwrap();
        assert_eq!(locate_candidate(&at("named.app")), None);
        std::os::unix::fs::symlink(at("mine.app"), at("mine-link.app")).unwrap();
        assert!(locate_candidate(&at("mine-link.app")).is_some_and(|found| found.link));
    }
}

#[test]
fn the_fence_judges_the_bundled_image_behind_a_launch_symlink() {
    let exec = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    let rolled = PathBuf::from("/Applications/aterm.app.rollback/Contents/MacOS/aterm");
    let link = PathBuf::from("/Users//a/.local/bin/aterm");
    let test_bin = PathBuf::from("/Users//a/aterm/target/debug/deps/aterm_cli-abc");
    let at = |p: &PathBuf| SelfImage::At(p.clone());

    assert_eq!(
        fence_exe_from(Some(link.clone()), &at(&exec)),
        Some(exec.clone())
    );
    assert_eq!(
        fence_exe_from(Some(exec.clone()), &at(&rolled)),
        Some(exec.clone())
    );
    assert_eq!(fence_exe_from(None, &at(&exec)), Some(exec.clone()));
    let other = PathBuf::from("/Applications/aterm-new.app/Contents/MacOS/aterm");
    assert_eq!(
        fence_exe_from(Some(exec.clone()), &at(&other)),
        Some(exec.clone()),
        "both bundled: the exec path still wins"
    );
    let never = || -> Option<PathBuf> { panic!("the kernel answered") };
    let (app, rollback) = (
        p("/Applications/aterm.app"),
        p("/Applications/aterm.app.rollback"),
    );
    assert_eq!(running_bundle_from(at(&rolled), never), Some(rollback));
    assert_eq!(running_bundle_from(at(&exec), never), Some(app.clone()));
    assert_eq!(running_bundle_from(at(&test_bin), never), None);
    assert_eq!(running_bundle_from(SelfImage::NoPath, never), None);
    assert_eq!(
        running_bundle_from(SelfImage::Unreadable, || Some(exec.clone())),
        Some(app)
    );
    assert_eq!(running_bundle_from(SelfImage::Unreadable, || None), None);
    for image in [at(&test_bin), SelfImage::NoPath, SelfImage::Unreadable] {
        assert_eq!(
            fence_exe_from(Some(test_bin.clone()), &image),
            Some(test_bin.clone())
        );
        assert_eq!(
            fence_exe_from(Some(link.clone()), &image),
            Some(link.clone())
        );
    }
    assert_eq!(fence_exe_from(None, &SelfImage::Unreadable), None);
}

#[test]
fn the_image_anchor_names_every_way_the_code_can_go_missing() {
    let exec = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let rolled = Path::new("/Applications/aterm.app.rollback/Contents/MacOS/aterm");
    let unbundled = Path::new("/Users//a/aterm/target/debug/deps/aterm_gui-abc");
    let at = |p: &Path| SelfImage::At(p.to_path_buf());

    assert_eq!(
        classify_image_anchor(Some(exec), &at(exec)),
        ImageAnchor::Live
    );
    assert_eq!(
        classify_image_anchor(Some(exec), &at(rolled)),
        ImageAnchor::Displaced
    );
    let other = Path::new("/Applications/aterm-new.app/Contents/MacOS/aterm");
    assert_eq!(
        classify_image_anchor(Some(exec), &at(other)),
        ImageAnchor::Live
    );
    assert_eq!(
        classify_image_anchor(Some(unbundled), &at(unbundled)),
        ImageAnchor::NotBundled
    );

    assert_eq!(
        classify_image_anchor(Some(exec), &SelfImage::NoPath),
        ImageAnchor::Deleted
    );
    assert_eq!(
        classify_image_anchor(Some(unbundled), &SelfImage::NoPath),
        ImageAnchor::NotBundled
    );
    assert_eq!(
        classify_image_anchor(None, &SelfImage::NoPath),
        ImageAnchor::Unknown
    );
    assert_eq!(
        classify_image_anchor(Some(exec), &SelfImage::Unreadable),
        ImageAnchor::Unknown
    );

    assert!(ImageAnchor::Displaced.grant_unverifiable());
    assert!(ImageAnchor::Deleted.grant_unverifiable());
    for ok in [
        ImageAnchor::Live,
        ImageAnchor::NotBundled,
        ImageAnchor::Unknown,
    ] {
        assert!(!ok.grant_unverifiable(), "{ok:?}");
    }
}

#[test]
fn probe_refuses_out_of_bundle_without_touching_the_os() {
    let deps = p("/Users//a/aterm/target/debug/deps/aterm_containment-abc123");
    let home = p("/Users//a");
    let got = probe_fda_with(ProbeGate::on(), Some(&deps), Some(&home), |_| {
        panic!("the FDA probe performed a syscall from outside a .app bundle");
    });
    assert_eq!(got.state, FdaState::Unknown);
    assert_eq!(got.label, ProbeLabel::RefusedOutOfBundle);
    assert!(got.label.refused());
}

#[test]
fn probe_refuses_when_disabled_or_unresolvable_without_touching_the_os() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let home = p("/Users//a");
    let boom = |_: &Path| -> ProbeOutcome { panic!("probe performed a syscall") };

    let off = ProbeGate {
        enabled: false,
        check: true,
    };
    assert_eq!(
        probe_fda_with(off, Some(&bundled), Some(&home), boom).label,
        ProbeLabel::RefusedDisabled
    );
    let no_check = ProbeGate {
        enabled: true,
        check: false,
    };
    assert_eq!(
        probe_fda_with(no_check, Some(&bundled), Some(&home), boom).label,
        ProbeLabel::RefusedDisabled
    );
    assert_eq!(
        probe_fda_with(ProbeGate::on(), None, Some(&home), boom).label,
        ProbeLabel::RefusedNoExe
    );
    assert_eq!(
        probe_fda_with(ProbeGate::on(), Some(&bundled), None, boom).label,
        ProbeLabel::RefusedNoHome
    );
    assert_eq!(
        probe_fda_with(
            ProbeGate::on(),
            Some(&bundled),
            Some(&p("/Users//a\0b")),
            boom
        )
        .label,
        ProbeLabel::RefusedBadPath,
        "a home no C API can take"
    );
    for gate in [off, no_check] {
        assert_eq!(
            probe_fda_with(gate, Some(&bundled), Some(&home), boom).state,
            FdaState::Unknown
        );
    }
}

#[test]
fn a_host_uuid_is_spelled_8_4_4_4_12() {
    let id = [
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    assert_eq!(format_uuid(&id), "12345678-9ABC-DEF0-0123-456789ABCDEF");
}

#[test]
fn probe_opens_only_the_user_store_when_it_answers() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let home = p("/Users//a");
    let user = p("/Users//a/Library/Application Support/com.apple.TCC/TCC.db");
    for (outcome, state) in [
        (ProbeOutcome::Ok, FdaState::Granted),
        (ProbeOutcome::Errno(ERRNO_EPERM), FdaState::Denied),
        (ProbeOutcome::Errno(13), FdaState::Unknown),
    ] {
        let mut seen = Vec::new();
        let got = probe_fda_with(ProbeGate::on(), Some(&bundled), Some(&home), |db| {
            seen.push(db.to_path_buf());
            outcome
        });
        assert_eq!(got.state, state, "{outcome:?}");
        assert_eq!(seen, vec![user.clone()], "{outcome:?}");
    }
}

#[test]
fn probe_falls_back_to_the_system_store_when_the_user_store_is_absent() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let home = p("/Users//a");
    let user = p("/Users//a/Library/Application Support/com.apple.TCC/TCC.db");
    for (system, state, label) in [
        (ProbeOutcome::Ok, FdaState::Granted, ProbeLabel::OpenOk),
        (
            ProbeOutcome::Errno(ERRNO_EPERM),
            FdaState::Denied,
            ProbeLabel::OpenEperm,
        ),
        (
            ProbeOutcome::Errno(ERRNO_ENOENT),
            FdaState::Unknown,
            ProbeLabel::OpenErrno(ERRNO_ENOENT),
        ),
    ] {
        let mut seen = Vec::new();
        let got = probe_fda_with(ProbeGate::on(), Some(&bundled), Some(&home), |db| {
            seen.push(db.to_path_buf());
            if db == user {
                ProbeOutcome::Errno(ERRNO_ENOENT)
            } else {
                system
            }
        });
        assert_eq!((got.state, got.label), (state, label), "{system:?}");
        assert_eq!(seen, vec![user.clone(), p(SYSTEM_TCC_DB)], "{system:?}");
    }
}

#[test]
fn probe_classification_is_a_table() {
    let cases = [
        (ProbeOutcome::Ok, FdaState::Granted, ProbeLabel::OpenOk),
        (
            ProbeOutcome::Errno(ERRNO_EPERM),
            FdaState::Denied,
            ProbeLabel::OpenEperm,
        ),
        (
            ProbeOutcome::Errno(13),
            FdaState::Unknown,
            ProbeLabel::OpenErrno(13),
        ),
    ];
    for (outcome, state, label) in cases {
        let got = classify_probe(outcome);
        assert_eq!(got.state, state, "state for {outcome:?}");
        assert_eq!(got.label, label, "label for {outcome:?}");
    }
}

#[test]
fn responsible_pids_detailed_resolves_once() {
    let resolutions = std::cell::Cell::new(0_usize);
    let asked = std::cell::RefCell::new(Vec::new());
    let resolve = || {
        resolutions.set(resolutions.get() + 1);
        Some(|pid: i32| {
            asked.borrow_mut().push(pid);
            if pid == 3 {
                Err(ResponsibleError::Gone)
            } else {
                Ok(pid * 10)
            }
        })
    };
    assert_eq!(
        ask_each(&[1, 2, 3], resolve),
        [Ok(10), Ok(20), Err(ResponsibleError::Gone)]
    );
    assert_eq!(resolutions.get(), 1, "the SPI is resolved once per slice");
    assert_eq!(
        *asked.borrow(),
        [1, 2, 3],
        "and asked once per pid, in order"
    );

    let absent = || {
        resolutions.set(resolutions.get() + 1);
        None::<fn(i32) -> Result<i32, ResponsibleError>>
    };
    assert_eq!(
        ask_each(&[1, 2, 3], absent),
        [Err(ResponsibleError::SymbolUnavailable); 3]
    );
    assert_eq!(resolutions.get(), 2);

    assert!(ask_each(&[], resolve).is_empty());
    assert_eq!(resolutions.get(), 2, "an empty slice resolves nothing");
    assert_eq!(asked.borrow().len(), 3, "the absent symbol asked nothing");
}

#[test]
fn responsible_answers_are_a_table() {
    let cases: [(i32, i32, Result<i32, ResponsibleError>); 5] = [
        (4711, 0, Ok(4711)),
        (0, 0, Ok(0)),
        (-1, ERRNO_EPERM, Err(ResponsibleError::NotOurs)),
        (-1, ERRNO_ESRCH, Err(ResponsibleError::Gone)),
        (-1, 22, Err(ResponsibleError::Errno(22))),
    ];
    for (ret, err, want) in cases {
        assert_eq!(responsible_answer(ret, err), want, "ret={ret} errno={err}");
    }
}

#[test]
fn minus_one_never_becomes_self() {
    let me = 4711;
    for err in [
        ResponsibleError::NotOurs,
        ResponsibleError::SymbolUnavailable,
        ResponsibleError::Errno(22),
        ResponsibleError::Unsupported,
    ] {
        assert_eq!(
            classify_responsible(me, Err(err)),
            Responsible::Unknown,
            "{err:?} must be Unknown, never SelfProcess"
        );
    }
    assert_eq!(
        classify_responsible(me, Err(ResponsibleError::Gone)),
        Responsible::Exited
    );
    assert_eq!(classify_responsible(me, Ok(me)), Responsible::SelfProcess);
    assert_eq!(classify_responsible(me, Ok(4402)), Responsible::Other(4402));
    assert_eq!(classify_responsible(me, Ok(0)), Responsible::Unknown);
    assert_eq!(classify_responsible(me, Ok(-1)), Responsible::Unknown);
    assert_eq!(classify_responsible(-1, Ok(-1)), Responsible::Unknown);
    assert_eq!(classify_responsible(0, Ok(0)), Responsible::Unknown);
}

#[test]
fn info_plist_under_a_protected_root_is_refused_and_falls_back_to_the_basename() {
    let home = p("/Users//a");
    let protected = protected_roots_in(Some(&home), &[]);
    assert!(
        protected.contains(&p("/Users//a/Documents")),
        "PRIVATE_SUBDIRS must supply ~/Documents"
    );

    let evil = p("/Users//a/Documents/Sketchy.app/Contents/MacOS/Sketchy");
    assert_eq!(readable_info_plist(&evil, &protected), None);
    assert_eq!(display_name(&evil, None), "Sketchy");

    let ok = p("/Applications/iTerm.app/Contents/MacOS/iTerm2");
    assert_eq!(
        readable_info_plist(&ok, &protected),
        Some(p("/Applications/iTerm.app/Contents/Info.plist"))
    );

    assert_eq!(readable_info_plist(Path::new("/bin/zsh"), &protected), None);
    assert_eq!(display_name(Path::new("/bin/zsh"), None), "zsh");
}

#[test]
fn display_name_prefers_display_then_name_then_basename() {
    let exe = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let both = "<plist><dict>\
        <key>CFBundleName</key><string>aterm-short</string>\
        <key>CFBundleDisplayName</key><string>aterm (dev)</string>\
        </dict></plist>";
    assert_eq!(display_name(&exe, Some(both)), "aterm (dev)");

    let name_only = "<plist><dict><key>CFBundleName</key><string>aterm</string></dict></plist>";
    assert_eq!(display_name(&exe, Some(name_only)), "aterm");

    let neither =
        "<plist><dict><key>CFBundleIdentifier</key><string>com.aterm.aterm</string></dict></plist>";
    assert_eq!(display_name(&exe, Some(neither)), "aterm");

    let empty = "<plist><dict><key>CFBundleDisplayName</key><string></string></dict></plist>";
    assert_eq!(display_name(&exe, Some(empty)), "aterm");
    let wrong = "<plist><dict><key>CFBundleDisplayName</key><true/><key>CFBundleName</key><string>ok</string></dict></plist>";
    assert_eq!(display_name(&exe, Some(wrong)), "ok");
}

#[test]
fn dr_classification_is_a_table() {
    let cases = [
        (
            "designated => identifier \"com.aterm.aterm\" and anchor apple generic and \
             certificate 1[field.1.2.840.113635.100.6.2.6] and \
             certificate leaf[field.1.2.840.113635.100.6.1.13] and \
             certificate leaf[subject.OU] = \"A66A9P66Z7\"",
            DrClass::Identity,
        ),
        (
            "identifier \"com.aterm.aterm.dev\" and certificate leaf H\"deadbeef\"",
            DrClass::Identity,
        ),
        (
            "designated => cdhash H\"0123456789abcdef\"",
            DrClass::Cdhash,
        ),
        ("cdhash H\"abc\" or cdhash H\"def\"", DrClass::Cdhash),
        (
            "identifier \"com.aterm.aterm\" and cdhash H\"abc\"",
            DrClass::Cdhash,
        ),
        ("code object is not signed at all", DrClass::Unsigned),
        ("", DrClass::Unknown),
        ("anchor apple", DrClass::Unknown),
        ("identifier \"com.aterm.aterm\"", DrClass::Unknown),
        (
            "identifier \"com.cdhash.x\" and anchor apple generic and \
             certificate leaf[subject.OU] = \"CDHASH\"",
            DrClass::Identity,
        ),
        (
            "identifier \"x cdhash H\" and anchor apple generic",
            DrClass::Identity,
        ),
        ("identifier \"anchor apple\"", DrClass::Unknown),
        (
            "designated => identifier mycdhash and anchor apple generic and \
             certificate leaf[subject.OU] = A66A9P66Z7",
            DrClass::Identity,
        ),
        ("identifier cdhash", DrClass::Unknown),
        ("(cdhash H\"abc\")", DrClass::Cdhash),
        (
            "identifier \"a\\\" and anchor apple generic\"",
            DrClass::Unknown,
        ),
        (
            "Executable=/A/we\"ird\ndesignated => cdhash H\"aa\"",
            DrClass::Cdhash,
        ),
        ("x: not signed at all", DrClass::Unsigned),
        ("code object is not signed", DrClass::Unsigned),
        (
            "identifier\"x\" and anchor apple generic",
            DrClass::Identity,
        ),
        (
            "identifier \"x\" and certificate root = H\"ab\"",
            DrClass::Identity,
        ),
        (
            "identifier \"x\" and certificate 1[subject.OU] = \"T\"",
            DrClass::Identity,
        ),
        ("identifier xcdhash H\"ab\"", DrClass::Unknown),
        ("identifier my_cdhash H\"ab\"", DrClass::Unknown),
    ];
    for (input, want) in cases {
        assert_eq!(classify_dr(input), want, "classifying {input:?}");
    }
    for (class, stable) in [
        (DrClass::Identity, true),
        (DrClass::Cdhash, false),
        (DrClass::Unsigned, false),
        (DrClass::Unknown, false),
    ] {
        assert_eq!(class.grant_stable(), stable, "{class:?}");
    }
}

#[test]
fn empty_overrides_resolve_to_private_subdirs() {
    let home = p("/Users//a");
    let roots = protected_roots_in(Some(&home), &[]);
    assert_eq!(
        roots,
        crate::sbpl::PRIVATE_SUBDIRS
            .iter()
            .map(|s| home.join(s))
            .collect::<Vec<_>>()
    );
}

#[test]
fn overrides_replace_the_default_and_are_expanded() {
    let home = p("/Users//a");
    let volumes = if cfg!(windows) {
        "C:/Volumes"
    } else {
        "/Volumes"
    };
    let overrides = [
        String::from("~/Documents"),
        String::from(volumes),
        String::from("~"),
        String::from("  ~/Library/CloudStorage  "),
        String::from("relative/path"),
        String::new(),
    ];
    let roots = protected_roots_in(Some(&home), &overrides);
    assert_eq!(
        roots,
        vec![
            p("/Users//a/Documents"),
            p(volumes),
            p("/Users//a"),
            p("/Users//a/Library/CloudStorage"),
        ]
    );
    assert!(protected_roots_in(None, &[]).is_empty());
    assert_eq!(protected_roots_in(None, &overrides), vec![p(volumes)]);
}

#[test]
fn tcc_prompting_roots_hold_the_private_set_and_what_it_omits() {
    let home = p("/Users//a");
    let roots = tcc_prompting_roots(&home, &[]);
    for sub in crate::sbpl::PRIVATE_SUBDIRS {
        assert!(roots.contains(&home.join(sub)), "{sub}");
    }
    for sub in TCC_PROMPTING_SUBPATHS {
        assert!(roots.contains(&home.join(sub)), "{sub}");
    }
    for abs in TCC_PROMPTING_ABSOLUTE {
        assert!(roots.contains(&p(abs)), "{abs}");
    }
    let narrowed = tcc_prompting_roots(&home, &[String::from("~/Projects/secret")]);
    assert!(narrowed.contains(&home.join("Documents")));
    assert!(narrowed.contains(&p("/Users//a/Projects/secret")));
    assert_eq!(narrowed.len(), roots.len() + 1);
}

#[cfg(unix)]
#[test]
fn other_homes_sit_beside_this_one() {
    assert_eq!(other_homes_root(Path::new("/Users//a")), Some(p("/Users")));
    assert_eq!(
        other_homes_root(Path::new("/System/Volumes/Data/Users//a")),
        Some(p("/System/Volumes/Data/Users"))
    );
    assert_eq!(other_homes_root(Path::new("/root")), None);
    assert_eq!(other_homes_root(Path::new("relative/a")), None);
}

#[test]
fn containment_is_a_prefix_test_over_whole_components() {
    let roots = vec![p("/Users//a/Documents")];
    assert!(is_under_protected_root(
        Path::new("/Users//a/Documents"),
        &roots
    ));
    assert!(is_under_protected_root(
        Path::new("/Users//a/Documents/x/y.app"),
        &roots
    ));
    assert!(!is_under_protected_root(
        Path::new("/Users//a/DocumentsOld/x"),
        &roots
    ));
    assert!(!is_under_protected_root(Path::new("/Users//a"), &roots));
    assert!(!is_under_protected_root(Path::new("/Applications"), &[]));
}

#[test]
fn folder_names_services_and_paths() {
    let home = p("/Users//a");
    let cases = [
        (
            Folder::Documents,
            "documents",
            "SystemPolicyDocumentsFolder",
            "/Users//a/Documents",
        ),
        (
            Folder::Desktop,
            "desktop",
            "SystemPolicyDesktopFolder",
            "/Users//a/Desktop",
        ),
        (
            Folder::Downloads,
            "downloads",
            "SystemPolicyDownloadsFolder",
            "/Users//a/Downloads",
        ),
        (
            Folder::AppData,
            "app-data",
            "SystemPolicyAppData",
            "/Users//a/Library/Containers/com.apple.Notes",
        ),
    ];
    for (folder, name, service, path) in cases {
        assert_eq!(folder.as_str(), name);
        assert_eq!(folder.tcc_service(), service);
        assert_eq!(folder.path(&home), p(path));
        assert_eq!(folder.absent_means_allowed(), folder == Folder::AppData);
        assert_eq!(Folder::parse(name), Some(folder));
        assert_eq!(Folder::parse(&name.to_uppercase()), Some(folder));
    }
    for spelling in ["app_data", "appdata", " App_Data "] {
        assert_eq!(
            Folder::parse(spelling),
            Some(Folder::AppData),
            "{spelling:?}"
        );
    }
    for unknown in ["pictures", "", "app data", "apps"] {
        assert_eq!(Folder::parse(unknown), None, "{unknown:?}");
    }
}

#[test]
fn folder_paths_are_resolved_data_for_the_warmup() {
    let home = p("/Users//a");
    let names = [
        String::from("documents"),
        String::from("nope"),
        String::from("downloads"),
    ];
    assert_eq!(
        folder_paths_in(Some(&home), &names),
        vec![
            (Folder::Documents, p("/Users//a/Documents")),
            (Folder::Downloads, p("/Users//a/Downloads")),
        ]
    );
    assert!(folder_paths_in(None, &names).is_empty());
}

#[test]
fn tccutil_presence_is_a_table_and_only_executable_may_run() {
    let (run, notx, gone) = (
        TccutilPresence::Executable,
        TccutilPresence::NotExecutable,
        TccutilPresence::Missing,
    );
    let cases = [
        (true, true, run, true),
        (true, false, notx, false),
        (false, false, gone, false),
        (false, true, gone, false),
    ];
    for (is_file, executable, want, can_run) in cases {
        let got = classify_tccutil(is_file, executable);
        assert_eq!(got, want, "is_file={is_file} executable={executable}");
        assert_eq!(got.can_run(), can_run);
    }
    assert_eq!(TccutilPresence::default(), TccutilPresence::Unknown);
    assert!(!TccutilPresence::Unknown.can_run());
}

#[test]
fn a_usable_bundle_id_is_non_blank_flagless_and_whitespace_free() {
    for good in ["a.b.c", "  a.b.c  ", "x", "a-b_c.d"] {
        assert!(bundle_id_is_usable(good), "{good:?} should be usable");
    }
    for bad in ["", "   ", "\t\n", "-x", "a b", "a\tb", "a\u{0}b", "a\u{7}"] {
        assert!(!bundle_id_is_usable(bad), "{bad:?} must be refused");
    }
}

#[test]
fn the_reset_command_can_never_take_the_every_app_form() {
    for bad in ["", "   ", "-not-a-flag"] {
        assert!(ResetSubject::from_usable(bad).is_none(), "{bad:?}");
    }
    let subject = ResetSubject::from_usable("a.b.c").expect("usable");
    for folder in Folder::ALL {
        let argv = tccutil_reset_command(&subject, *folder);
        assert_eq!(argv.len(), 4, "{folder:?}");
        assert_eq!(argv[0], TCCUTIL_PATH);
        assert_eq!(argv[1], "reset");
        assert_eq!(argv[2], folder.tcc_service());
        assert_eq!(argv[3], "a.b.c");
    }
}

#[test]
fn a_reset_subject_is_read_from_a_bundles_plist() {
    let scratch = crate::test_scratch_dir("subject");
    let root = scratch.path().join("Subject.app");
    let contents = root.join("Contents");
    std::fs::create_dir_all(&contents).expect("bundle contents");
    let plist = |id: &str| {
        format!("<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>")
    };
    std::fs::write(contents.join("Info.plist"), plist(" a.b.c ")).expect("plist");
    let subject = ResetSubject::read(&root, &[]).expect("a subject");
    assert_eq!(subject.as_str(), "a.b.c", "the subject is trimmed once");
    std::fs::write(contents.join("Info.plist"), plist("-not-a-flag")).expect("plist");
    assert_eq!(ResetSubject::read(&root, &[]), None);
}

#[test]
fn a_plan_is_one_command_per_distinct_folder_from_one_subject() {
    let subject = ResetSubject::from_usable(" a.b.c ").expect("usable");
    let plan = ResetPlan::new(
        &subject,
        &[
            Folder::Downloads,
            Folder::Documents,
            Folder::Downloads,
            Folder::Documents,
        ],
    )
    .expect("plan builds");
    assert_eq!(plan.bundle_id(), "a.b.c", "the subject is trimmed once");
    assert_eq!(plan.folders(), &[Folder::Downloads, Folder::Documents]);
    let commands = plan.commands();
    assert_eq!(
        commands,
        vec![
            (
                Folder::Downloads,
                tccutil_reset_command(&subject, Folder::Downloads)
            ),
            (
                Folder::Documents,
                tccutil_reset_command(&subject, Folder::Documents)
            ),
        ]
    );
    assert!(ResetPlan::new(&subject, &[]).is_none());
}

fn offer_inputs<'a>(
    dr: DrClass,
    tccutil: TccutilPresence,
    bundle_id: Option<&'a ResetSubject>,
) -> ResetOfferInputs<'a> {
    ResetOfferInputs {
        dr,
        tccutil,
        bundle_id,
    }
}

#[test]
fn the_repair_offer_is_a_table() {
    let (run, notx, gone, unk) = (
        TccutilPresence::Executable,
        TccutilPresence::NotExecutable,
        TccutilPresence::Missing,
        TccutilPresence::Unknown,
    );
    let subject = ResetSubject::from_usable("a.b.c").expect("usable");
    let id = Some(&subject);
    let cases = [
        (DrClass::Identity, run, id, ResetOffer::Offer),
        (DrClass::Unknown, run, id, ResetOffer::Offer),
        (DrClass::Cdhash, run, id, ResetOffer::ExplainRebuild),
        (DrClass::Unsigned, run, id, ResetOffer::ExplainRebuild),
        (DrClass::Cdhash, gone, id, ResetOffer::ExplainRebuild),
        (DrClass::Cdhash, run, None, ResetOffer::ExplainRebuild),
        (DrClass::Identity, gone, id, ResetOffer::HideNoTool),
        (DrClass::Identity, notx, id, ResetOffer::HideNoTool),
        (DrClass::Identity, unk, id, ResetOffer::HideNoTool),
        (DrClass::Identity, run, None, ResetOffer::HideNoBundleId),
    ];
    for (dr, tool, bundle, want) in cases {
        let got = reset_offer(offer_inputs(dr, tool, bundle));
        assert_eq!(got, want, "dr={dr:?} tool={tool:?} bundle={bundle:?}");
        let offered = want == ResetOffer::Offer;
        assert_eq!(got.shows_button(), offered);
        assert_eq!(
            ResetPlan::for_offer(offer_inputs(dr, tool, bundle), Folder::ALL).is_some(),
            offered,
            "dr={dr:?} tool={tool:?} bundle={bundle:?}"
        );
    }
    assert_eq!(
        reset_offer(ResetOfferInputs::default()),
        ResetOffer::HideNoTool
    );
}

#[test]
fn one_invocations_status_is_read_from_its_exit_alone() {
    assert_eq!(ResetStatus::from_exit_status(Some(0)), ResetStatus::Reset);
    assert_eq!(
        ResetStatus::from_exit_status(Some(64)),
        ResetStatus::Declined { code: Some(64) }
    );
    assert_eq!(
        ResetStatus::from_exit_status(None),
        ResetStatus::Declined { code: None }
    );
}

#[test]
fn the_reset_outcome_is_a_table() {
    let ok = |f: Folder| ResetAttempt::new(f, ResetStatus::Reset);
    let bad = |f: Folder, c: i32| ResetAttempt::from_exit_status(f, Some(c));
    let gone = ResetAttempt::tool_absent;
    let cases: [(&[ResetAttempt], ResetOutcome); 7] = [
        (
            &[
                ok(Folder::Documents),
                ok(Folder::Desktop),
                ok(Folder::Downloads),
            ],
            ResetOutcome::AllReset,
        ),
        (
            &[
                ok(Folder::Documents),
                bad(Folder::Desktop, 1),
                ok(Folder::Downloads),
            ],
            ResetOutcome::Partial,
        ),
        (
            &[
                bad(Folder::Documents, 1),
                bad(Folder::Desktop, 1),
                bad(Folder::Downloads, 70),
            ],
            ResetOutcome::NoneReset,
        ),
        (
            &[
                gone(Folder::Documents),
                gone(Folder::Desktop),
                gone(Folder::Downloads),
            ],
            ResetOutcome::ToolAbsent,
        ),
        (
            &[ok(Folder::Documents), gone(Folder::Desktop)],
            ResetOutcome::Partial,
        ),
        (
            &[bad(Folder::Documents, 1), gone(Folder::Desktop)],
            ResetOutcome::NoneReset,
        ),
        (&[], ResetOutcome::NotAttempted),
    ];
    for (attempts, want) in cases {
        assert_eq!(reset_outcome(attempts), want, "{attempts:?}");
    }
}

#[test]
fn only_a_reset_folder_is_warmed_and_only_an_asked_one_declined() {
    let attempts = [
        ResetAttempt::new(Folder::Documents, ResetStatus::Reset),
        ResetAttempt::from_exit_status(Folder::Desktop, Some(1)),
        ResetAttempt::from_exit_status(Folder::Downloads, None),
        ResetAttempt::tool_absent(Folder::AppData),
    ];
    assert_eq!(folders_reset(&attempts), vec![Folder::Documents]);
    assert_eq!(
        folders_declined(&attempts),
        vec![(Folder::Desktop, Some(1)), (Folder::Downloads, None)]
    );
}

fn inputs(adoption: Attribution, fda: FdaState) -> PostureInputs {
    PostureInputs {
        adoption,
        fda,
        ..PostureInputs::default()
    }
}

#[test]
fn posture_join_is_a_table_under_todays_evidence() {
    let cases = [
        (Attribution::Live, FdaState::Granted),
        (Attribution::Live, FdaState::Denied),
        (Attribution::Live, FdaState::Unknown),
        (Attribution::Adopted, FdaState::Granted),
        (Attribution::Adopted, FdaState::Denied),
        (Attribution::Adopted, FdaState::Unknown),
        (Attribution::Unknown, FdaState::Granted),
        (Attribution::Unknown, FdaState::Denied),
        (Attribution::Unknown, FdaState::Unknown),
    ];
    for evidence in [SpikeEvidence::UNMEASURED, SpikeEvidence::MEASURED] {
        for (adoption, fda) in cases {
            let got = ConsentPosture::join(PostureInputs {
                evidence,
                ..inputs(adoption, fda)
            });
            assert_eq!(got.attribution, adoption);
            assert_eq!(got.fda, fda);
            assert_eq!(
                got.fs_consent,
                FsConsent::Unknown,
                "unmeasured coverage must never report covered: {adoption:?}/{fda:?}"
            );
            assert!(
                got.prompt_possible,
                "unmeasured coverage leaves a prompt possible: {adoption:?}/{fda:?}"
            );
            assert_eq!(got.fda_scope, FdaScope::Unknown);
        }
    }
}

#[test]
fn an_observed_eperm_is_the_only_route_to_denied() {
    for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
        let mut i = inputs(Attribution::Live, fda);
        i.observed_eperm = true;
        assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
    }
    assert_eq!(
        ConsentPosture::join(inputs(Attribution::Live, FdaState::Denied)).fs_consent,
        FsConsent::Unknown
    );
}

#[test]
fn an_adopted_session_is_never_denied_before_s2() {
    let mut i = inputs(Attribution::Adopted, FdaState::Denied);
    i.observed_eperm = true;
    let got = ConsentPosture::join(i);
    assert_eq!(got.attribution, Attribution::Adopted);
    assert_eq!(
        got.fs_consent,
        FsConsent::Unknown,
        "design §3.9 pt3: adopted reports unknown, never denied, until S2"
    );

    i.evidence.handoff_attribution_measured = true;
    assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
}

#[test]
fn covered_requires_grant_plus_live_plus_measured_coverage() {
    let measured = SpikeEvidence {
        fda_coverage_measured: true,
        fda_scope: FdaScope::ThisProcess,
        ..SpikeEvidence::UNMEASURED
    };
    let mut i = inputs(Attribution::Live, FdaState::Granted);
    i.evidence = measured;
    let got = ConsentPosture::join(i);
    assert_eq!(got.fs_consent, FsConsent::Covered);
    assert!(!got.prompt_possible);
    assert_eq!(
        got.fda_scope,
        FdaScope::ThisProcess,
        "carried from the evidence"
    );

    let mut not_live = i;
    not_live.adoption = Attribution::Unknown;
    assert_eq!(
        ConsentPosture::join(not_live).fs_consent,
        FsConsent::Unknown
    );

    let mut not_granted = i;
    not_granted.fda = FdaState::Unknown;
    assert_eq!(
        ConsentPosture::join(not_granted).fs_consent,
        FsConsent::Unknown
    );
    assert!(ConsentPosture::join(not_granted).prompt_possible);

    let mut not_measured = i;
    not_measured.evidence = SpikeEvidence::UNMEASURED;
    assert_eq!(
        ConsentPosture::join(not_measured).fs_consent,
        FsConsent::Unknown
    );
}

#[test]
fn attribution_comes_from_the_adoption_record_not_the_spi() {
    for corroboration in [
        Responsible::Unknown,
        Responsible::SelfProcess,
        Responsible::Exited,
        Responsible::Other(4402),
    ] {
        for record in [
            Attribution::Live,
            Attribution::Adopted,
            Attribution::Unknown,
        ] {
            let mut i = inputs(record, FdaState::Unknown);
            i.responsible = corroboration;
            let got = ConsentPosture::join(i);
            assert_eq!(
                got.attribution, record,
                "record {record:?} must survive corroboration {corroboration:?}"
            );
            assert_eq!(got.responsible, corroboration, "corroboration is carried");
        }
    }
}

#[test]
fn the_wire_tokens_are_the_documented_spellings() {
    let tokens = [
        (FdaState::Granted.as_str(), "granted"),
        (FdaState::Denied.as_str(), "denied"),
        (FdaState::Unknown.as_str(), "unknown"),
        (Attribution::Live.as_str(), "live"),
        (Attribution::Adopted.as_str(), "adopted"),
        (Attribution::Unknown.as_str(), "unknown"),
        (FsConsent::Covered.as_str(), "covered"),
        (FsConsent::Denied.as_str(), "denied"),
        (FsConsent::Unknown.as_str(), "unknown"),
        (FdaScope::Unknown.as_str(), "unknown"),
        (FdaScope::ThisProcess.as_str(), "this_process"),
        (FdaScope::NewProcesses.as_str(), "new_processes"),
        (DrClass::Identity.as_str(), "identity"),
        (DrClass::Cdhash.as_str(), "cdhash"),
        (DrClass::Unsigned.as_str(), "unsigned"),
        (DrClass::Unknown.as_str(), "unknown"),
        (ImageAnchor::Live.as_str(), "live"),
        (ImageAnchor::Displaced.as_str(), "displaced"),
        (ImageAnchor::Deleted.as_str(), "deleted"),
        (ImageAnchor::NotBundled.as_str(), "not-bundled"),
        (ImageAnchor::Unknown.as_str(), "unknown"),
        (DebuggerMode::Enabled.as_str(), "enabled"),
        (DebuggerMode::Disabled.as_str(), "disabled"),
        (DebuggerMode::Unrecognized.as_str(), "unrecognized"),
        (DebuggerMode::Unknown.as_str(), "unknown"),
        (DebuggerMember::Admin.as_str(), "admin"),
        (DebuggerMember::Developer.as_str(), "developer"),
        (DebuggerMember::NotMember.as_str(), "no"),
        (DebuggerMember::Unknown.as_str(), "unknown"),
        (SelfHardening::Hardened.as_str(), "yes"),
        (SelfHardening::NotHardened.as_str(), "no"),
        (SelfHardening::Unknown.as_str(), "unknown"),
        (DevToolState::Allowed.as_str(), "allowed"),
        (DevToolState::Denied.as_str(), "denied"),
        (DevToolState::Unlisted.as_str(), "unlisted"),
        (DevToolState::Unknown.as_str(), "unknown"),
        (DevToolListing::On.as_str(), "on"),
        (DevToolListing::Off.as_str(), "off"),
        (DevToolListing::Absent.as_str(), "absent"),
        (DevToolListing::Unknown.as_str(), "unknown"),
        (DevToolGrant::Granted.as_str(), "granted"),
        (DevToolGrant::NotGranted.as_str(), "not_granted"),
        (DevToolGrant::Unknown.as_str(), "unknown"),
    ];
    for (got, want) in tokens {
        assert_eq!(got, want);
    }

    use ProbeLabel as F;
    for (label, token, refused) in [
        (F::Pending, "pending", false),
        (F::OpenOk, "open_ok", false),
        (F::OpenEperm, "open_eperm", false),
        (F::OpenErrno(9), "open_errno", false),
        (F::RefusedOutOfBundle, "refused_out_of_bundle", true),
        (F::RefusedNoExe, "refused_no_exe", true),
        (F::RefusedDisabled, "refused_disabled", true),
        (F::RefusedNoHome, "refused_no_home", true),
        (F::RefusedBadPath, "refused_bad_path", true),
        (F::UnsupportedPlatform, "unsupported_platform", true),
    ] {
        assert_eq!((label.as_str(), label.refused()), (token, refused));
    }
    use DebuggerProbeLabel as D;
    for (label, token, refused) in [
        (D::Pending, "pending", false),
        (D::StatusOk, "status_ok", false),
        (D::StatusUnrecognized, "status_unrecognized", false),
        (D::ToolAbsent, "tool_absent", false),
        (D::SpawnFailed, "spawn_failed", false),
        (D::TimedOut, "timed_out", false),
        (D::OutputUnreadable, "output_unreadable", false),
        (D::RefusedDisabled, "refused_disabled", true),
        (D::RefusedOutOfBundle, "refused_out_of_bundle", true),
        (D::RefusedNoExe, "refused_no_exe", true),
        (D::UnsupportedPlatform, "unsupported_platform", true),
    ] {
        assert_eq!((label.as_str(), label.refused()), (token, refused));
    }
    use DevToolProbeLabel as T;
    for (label, token, refused) in [
        (T::Pending, "pending", false),
        (T::Observed, "observed", false),
        (T::NoRecentRequest, "no_recent_request", false),
        (T::LogUnavailable, "log_unavailable", false),
        (T::LogFailed, "log_failed", false),
        (T::FormatUnrecognized, "format_unrecognized", false),
        (T::TimedOut, "timed_out", false),
        (T::SpawnFailed, "spawn_failed", false),
        (T::OutputUnreadable, "output_unreadable", false),
        (T::RefusedDisabled, "refused_disabled", true),
        (T::RefusedObserverOff, "refused_observer_off", true),
        (T::RefusedOutOfBundle, "refused_out_of_bundle", true),
        (T::RefusedNoExe, "refused_no_exe", true),
        (T::RefusedNoSubject, "refused_no_subject", true),
        (T::UnsupportedPlatform, "unsupported_platform", true),
    ] {
        assert_eq!((label.as_str(), label.refused()), (token, refused));
    }
    use DevToolListLabel as L;
    for (label, token, refused) in [
        (L::Pending, "pending", false),
        (L::Read, "read", false),
        (L::NoFullDiskAccess, "no_full_disk_access", false),
        (L::OpenFailed, "open_failed", false),
        (L::ToolFailed, "sqlite_failed", false),
        (L::FormatUnrecognized, "format_unrecognized", false),
        (L::TimedOut, "timed_out", false),
        (L::SpawnFailed, "spawn_failed", false),
        (L::OutputUnreadable, "output_unreadable", false),
        (L::RefusedDisabled, "refused_disabled", true),
        (L::RefusedObserverOff, "refused_observer_off", true),
        (L::RefusedOutOfBundle, "refused_out_of_bundle", true),
        (L::RefusedNoExe, "refused_no_exe", true),
        (L::RefusedNoSubject, "refused_no_subject", true),
        (L::UnsupportedPlatform, "unsupported_platform", true),
    ] {
        assert_eq!((label.as_str(), label.refused()), (token, refused));
    }

    let unobserved = [
        FdaState::default() == FdaState::Unknown,
        Attribution::default() == Attribution::Unknown,
        FsConsent::default() == FsConsent::Unknown,
        DebuggerMode::default() == DebuggerMode::Unknown,
        DebuggerMember::default() == DebuggerMember::Unknown,
        SelfHardening::default() == SelfHardening::Unknown,
        DevToolState::default() == DevToolState::Unknown,
        DevToolListing::default() == DevToolListing::Unknown,
        DevToolGrant::default() == DevToolGrant::Unknown,
    ];
    assert_eq!(unobserved, [true; 9], "a default that is a verdict");
}

#[test]
fn the_change_builders_always_carry_their_flags() {
    assert!(
        Path::new(DEVTOOLSSECURITY_PATH).has_root(),
        "never resolved through PATH"
    );
    assert_eq!(
        DebuggerChange::On.command(),
        [DEVTOOLSSECURITY_PATH, "-enable"]
    );
    let disable = DebuggerChange::Off.command();
    assert_eq!(disable, [DEVTOOLSSECURITY_PATH, "-disable"]);
    assert_eq!(DEBUGGER_ACCESS_REVERT, disable.join(" "));
    assert_eq!(DebuggerChange::On.precondition(), DebuggerMode::Disabled);
    assert_eq!(DebuggerChange::Off.precondition(), DebuggerMode::Enabled);
    assert_eq!(DebuggerChange::On.target(), DebuggerMode::Enabled);
    assert_eq!(DebuggerChange::Off.target(), DebuggerMode::Disabled);
    for change in [DebuggerChange::On, DebuggerChange::Off] {
        assert_eq!(DebuggerChange::from_token(change.as_str()), Some(change));
    }
    assert_eq!(DebuggerChange::from_token("enable"), None);
}

fn probe_reading(mode: DebuggerMode) -> DebuggerProbe {
    DebuggerProbe {
        mode,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    }
}

#[test]
fn a_change_spawns_only_over_a_fresh_read_of_its_precondition() {
    let never_spawn = |argv: &[&str]| -> DevtoolsChangeRun {
        panic!("spawned {argv:?} over a read that is not the precondition")
    };
    for change in [DebuggerChange::On, DebuggerChange::Off] {
        for mode in [
            DebuggerMode::Enabled,
            DebuggerMode::Disabled,
            DebuggerMode::Unrecognized,
            DebuggerMode::Unknown,
        ] {
            if mode == change.precondition() {
                continue;
            }
            let observed = probe_reading(mode);
            assert_eq!(
                devtools_change_with(change, || observed, never_spawn),
                DevtoolsChangeRun::NotDue { observed },
                "{change:?} over {mode:?}"
            );
        }
        for label in [
            DebuggerProbeLabel::RefusedDisabled,
            DebuggerProbeLabel::RefusedOutOfBundle,
            DebuggerProbeLabel::RefusedNoExe,
            DebuggerProbeLabel::UnsupportedPlatform,
        ] {
            let refused = DebuggerProbe::refused(label);
            assert!(matches!(
                devtools_change_with(change, || refused, never_spawn),
                DevtoolsChangeRun::NotDue { .. }
            ));
        }
        let mut spawned = Vec::new();
        let run = devtools_change_with(
            change,
            || probe_reading(change.precondition()),
            |argv| {
                spawned.push(argv.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
                DevtoolsChangeRun::Exited {
                    code: Some(0),
                    stdout_head: String::new(),
                    stderr_head: String::new(),
                }
            },
        );
        assert!(run.spawned());
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0], change.command());
    }
}

#[cfg(unix)]
#[test]
fn the_change_runner_keeps_the_exit_and_a_head_of_each_stream() {
    // The child's readers must reach EOF (nothing outlives it to hold a pipe), so the first
    // grace poll waits for them on the hang detector, not the product's 2 s grace.
    GRACE_POLL.with(|slot| {
        let act = |_: &mut u32, readers_done: &dyn Fn() -> bool| {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the change's readers reached EOF",
                || readers_done().then_some(()),
            );
        };
        *slot.borrow_mut() = Some((0, Box::new(act)));
    });
    let mut seen = None;
    let run = run_to_exit_keeping_heads(
        &[
            "/bin/sh",
            "-c",
            "printf 'in=%s\\n' \"$(wc -c | tr -d ' ')\"; \
             i=0; while [ $i -lt 100 ]; do printf '%0100d' 0; i=$((i+1)); done; \
             echo nope >&2; exit 3",
        ],
        |pid| seen = Some((pid, process_birth(pid))),
    );
    let (pid, birth) = seen.expect("on_spawn is called");
    assert!(pid > 1);
    if cfg!(target_os = "macos") {
        assert!(birth.is_some(), "the live child has a birth record");
    }
    let DevtoolsChangeRun::Exited {
        code,
        stdout_head,
        stderr_head,
    } = run
    else {
        panic!("{run:?}");
    };
    assert_eq!(code, Some(3));
    assert!(stdout_head.starts_with("in=0\n"), "{stdout_head:?}");
    assert_eq!(stdout_head.len(), DEVTOOLS_CHANGE_HEAD_BYTES);
    assert_eq!(stderr_head, "nope\n");
}

#[cfg(target_os = "macos")]
#[test]
fn a_change_reader_that_lags_the_exit_still_reaches_the_head() {
    let dir = crate::test_scratch_dir("change-lag");
    let gate = fifo_gate(dir.path());
    let _detector = gate_hang_detector(&gate);
    let armed = gate.clone();
    GRACE_POLL.with(|slot| {
        let act = move |_: &mut u32, readers_done: &dyn Fn() -> bool| {
            release_gate(&armed).unwrap();
            let released = std::time::Instant::now();
            while !readers_done() {
                assert!(
                    released.elapsed() < HANG_DETECTOR,
                    "the readers never ended"
                );
                aterm_uds::deadline::poll_yield();
            }
        };
        *slot.borrow_mut() = Some((1, Box::new(act)));
    });
    let run = run_to_exit_keeping_heads(
        &[
            "/bin/sh",
            "-c",
            GATED_GRANDCHILD,
            "sh",
            gate.to_str().unwrap(),
        ],
        |_| {},
    );
    let _ = release_gate(&gate);
    assert_eq!(
        run,
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: "earlylate".to_string(),
            stderr_head: String::new(),
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_change_reader_still_running_past_the_grace_leaves_an_empty_head() {
    let dir = crate::test_scratch_dir("change-grace");
    let gate = fifo_gate(dir.path());
    let _detector = gate_hang_detector(&gate);
    GRACE_POLL.with(|slot| {
        let act = |polls: &mut u32, readers_done: &dyn Fn() -> bool| {
            assert!(!readers_done(), "the grandchild holds both pipes");
            *polls = CHANGE_READER_GRACE_POLLS - 1;
            GRACE_POLL.with(|slot| {
                *slot.borrow_mut() = Some((CHANGE_READER_GRACE_POLLS, Box::new(|_, _| {})));
            });
        };
        *slot.borrow_mut() = Some((0, Box::new(act)));
    });
    let run = run_to_exit_keeping_heads(
        &[
            "/bin/sh",
            "-c",
            GATED_GRANDCHILD,
            "sh",
            gate.to_str().unwrap(),
        ],
        |_| {},
    );
    let armed = GRACE_POLL.with(|slot| slot.borrow_mut().take().map(|(at, _)| at));
    let held = release_gate(&gate);
    assert_eq!(
        armed,
        Some(CHANGE_READER_GRACE_POLLS),
        "the first grace poll ran its act (it left Some(0) armed), and no poll ran past \
         the last (it would have taken the act, None)"
    );
    assert_eq!(
        run,
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    );
    assert!(
        held.is_ok(),
        "the grandchild still held the pipes when the run returned: {held:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_change_runner_names_a_missing_program() {
    let scratch = crate::test_scratch_dir("devtools-miss");
    let missing = scratch.path().join("DevToolsSecurity-stand-in");
    let missing = missing.to_str().expect("utf-8 temp dir");
    let run = run_to_exit_keeping_heads(&[missing, "-enable"], |_| panic!("nothing was spawned"));
    assert_eq!(run, DevtoolsChangeRun::Absent);
    assert_eq!(
        run_to_exit_keeping_heads(&[], |_| panic!("nothing was spawned")),
        DevtoolsChangeRun::SpawnFailed
    );
    let folder = scratch.path().to_str().expect("utf-8 temp dir");
    assert_eq!(
        run_to_exit_keeping_heads(&[folder], |_| panic!("nothing was spawned")),
        DevtoolsChangeRun::SpawnFailed,
        "a folder is there but runs nothing"
    );
    assert!(
        DevtoolsChangeRun::WaitFailed.spawned(),
        "a child whose wait failed was still started"
    );
}

#[test]
fn a_recorded_child_runs_only_while_its_birth_matches() {
    let me = std::process::id();
    let no_birth = DevtoolsChild {
        pid: me,
        birth: None,
    };
    assert!(!no_birth.is_running());
    assert!(
        !DevtoolsChild {
            pid: 0,
            birth: None
        }
        .is_running(),
        "no birth matches no birth: a pid the kernel will not describe is not running"
    );
    let Some(birth) = process_birth(me) else {
        assert!(!cfg!(target_os = "macos"), "our own pid has a birth record");
        return;
    };
    assert!(
        DevtoolsChild {
            pid: me,
            birth: Some(birth)
        }
        .is_running()
    );
    let other = ProcessBirth {
        seconds: birth.seconds.wrapping_add(1),
        ..birth
    };
    assert!(
        !DevtoolsChild {
            pid: me,
            birth: Some(other)
        }
        .is_running()
    );
    assert_eq!(process_birth(0), None);
    assert_eq!(process_birth(1), None);
}

fn exited(code: Option<i32>, stdout: &str, stderr: &str) -> ToolRun {
    ToolRun::Exited {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

#[test]
fn the_status_classifier_reads_only_what_the_tool_says() {
    use DebuggerMode as M;
    use DebuggerProbeLabel as L;
    let ok = L::StatusOk;
    let no = (M::Unknown, L::StatusUnrecognized);
    let cases: &[(ToolRun, (DebuggerMode, DebuggerProbeLabel))] = &[
        (
            exited(Some(0), "Developer mode is currently disabled.\n", ""),
            (M::Disabled, ok),
        ),
        (
            exited(Some(0), "Developer mode is currently enabled.\n", ""),
            (M::Enabled, ok),
        ),
        (
            exited(Some(0), "Developer mode is already enabled.", ""),
            (M::Enabled, ok),
        ),
        (
            exited(Some(0), "Developer mode is enabled.", ""),
            (M::Enabled, ok),
        ),
        (
            exited(Some(0), "Developer mode is now enabled.\n", ""),
            (M::Enabled, ok),
        ),
        (
            exited(Some(0), "  Developer mode is currently disabled.\r\n", ""),
            (M::Disabled, ok),
        ),
        (
            exited(Some(0), "", "Developer mode is currently disabled.\n"),
            (M::Disabled, ok),
        ),
        (
            exited(
                Some(0),
                "Getting rights definition for: system.privilege.taskport.debug\n\
                 Developer mode is currently enabled.\n",
                "",
            ),
            (M::Enabled, ok),
        ),
        (
            exited(
                Some(0),
                "Developer mode rights are currently in an unknown state.\n",
                "",
            ),
            (M::Unrecognized, ok),
        ),
        (
            exited(
                Some(1),
                "Developer mode rights are currently in an unknown state.\n",
                "",
            ),
            (M::Unrecognized, ok),
        ),
        (
            exited(Some(1), "Developer mode is currently disabled.\n", ""),
            no,
        ),
        (
            exited(None, "Developer mode is currently enabled.\n", ""),
            no,
        ),
        (exited(Some(0), "", ""), no),
        (
            exited(
                Some(0),
                "Right definition is already correct; exiting with success.\n",
                "",
            ),
            no,
        ),
        (exited(Some(0), "Developer mode is not enabled.\n", ""), no),
        (
            exited(Some(0), "Developer mode is currently disabled\n", ""),
            no,
        ),
        (
            exited(Some(0), "developer mode is currently disabled.\n", ""),
            no,
        ),
        (
            exited(Some(0), "Developer mode is newly enabled.\n", ""),
            no,
        ),
        (
            exited(
                Some(0),
                "Developer mode rights are newly in an unknown state.\n",
                "",
            ),
            no,
        ),
        (
            exited(Some(0), "Developer mode is currently disenabled.\n", ""),
            no,
        ),
        (
            exited(
                Some(0),
                "Developer mode is currently enabled.\n",
                "Developer mode is currently disabled.\n",
            ),
            no,
        ),
        (ToolRun::Absent, (M::Unknown, L::ToolAbsent)),
        (ToolRun::SpawnFailed, (M::Unknown, L::SpawnFailed)),
        (ToolRun::TimedOut, (M::Unknown, L::TimedOut)),
        (ToolRun::Unreadable, (M::Unknown, L::OutputUnreadable)),
    ];
    for (run, want) in cases {
        assert_eq!(classify_devtools_status(run), *want, "{run:?}");
    }
    assert_eq!(
        classify_devtools_status(&exited(
            Some(0),
            "Developer mode is currently disabled.\n",
            "Developer mode is currently disabled.\n",
        )),
        (M::Disabled, ok)
    );
}

#[test]
fn membership_names_the_first_rule_that_matches() {
    use DebuggerMember as D;
    let cases = [
        ((Some(true), None), D::Admin),
        ((Some(true), Some(true)), D::Admin),
        ((Some(true), Some(false)), D::Admin),
        ((Some(false), Some(true)), D::Developer),
        ((Some(false), Some(false)), D::NotMember),
        ((Some(false), None), D::Unknown),
        ((None, Some(true)), D::Unknown),
        ((None, Some(false)), D::Unknown),
        ((None, None), D::Unknown),
    ];
    for ((admin, developer), want) in cases {
        assert_eq!(
            classify_membership(admin, developer),
            want,
            "admin={admin:?} developer={developer:?}"
        );
    }
}

#[test]
fn hardening_is_the_runtime_without_get_task_allow() {
    use SelfHardening as H;
    let cases = [
        (None, H::Unknown),
        (Some(0x0001_0000), H::Hardened),
        (Some(0x0001_0000 | 0x0000_0001), H::Hardened),
        (Some(0x0001_0000 | CS_GET_TASK_ALLOW), H::NotHardened),
        (Some(0x0000_0002), H::NotHardened),
        (Some(0x0002_0002), H::NotHardened),
        (Some(CS_GET_TASK_ALLOW), H::NotHardened),
        (Some(0), H::NotHardened),
    ];
    for (flags, want) in cases {
        assert_eq!(classify_cs_status(flags), want, "{flags:x?}");
    }
    assert_eq!(CS_RUNTIME, 0x1_0000);
    assert_eq!(CS_OPS_STATUS, 0);
}

/// A team is quoted into a requirement only in Apple's shape, so a requirement names nothing
/// but a leaf certificate's team. NEGATIVE CONTROL (run): with the shape check gone, the
/// quoting row is a requirement.
#[test]
fn a_team_requirement_quotes_only_a_team_in_apples_shape() {
    assert_eq!(
        team_requirement("QZPTEAM001").as_deref(),
        Some("anchor apple generic and certificate leaf[subject.OU] = \"QZPTEAM001\"")
    );
    let longest = "A".repeat(TEAM_MAX);
    assert!(team_requirement(&longest).is_some());
    let too_long = "A".repeat(TEAM_MAX + 1);
    for bad in [
        "",
        "qzpteam001",
        "QZP\"TEAM",
        "QZP TEAM",
        "QZPTEAM\u{0}",
        too_long.as_str(),
    ] {
        assert_eq!(team_requirement(bad), None, "{bad:?}");
    }
    assert!(!process_meets_team(0, "QZPTEAM001"));
    assert!(!process_meets_team(std::process::id(), "QZP\"TEAM"));
}

/// An ad hoc signature carries no certificate, so a running copy of `sleep` signed ad hoc
/// meets no team's requirement, Claude Code's included, though the kernel holds it valid.
/// NEGATIVE CONTROL (run): `process_meets_team` answering whether `codesign` ran says it
/// meets them.
#[cfg(target_os = "macos")]
#[test]
fn a_running_ad_hoc_copy_meets_no_teams_requirement() {
    // `sleep`'s kept stand-in, run in place: the child is killed at once, maybe while macOS
    // still assesses its exec, and a copy unlinked then crashes `syspolicyd`.
    let copy = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"));
    let mut child = aterm_uds::spawnfd::command(&copy)
        .arg("30")
        .spawn()
        .expect("the copy runs");
    let pid = child.id();
    let met = ["QZPTEAM001", "Q6L2SF6YDW"].map(|team| process_meets_team(pid, team));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(met, [false, false]);
}

#[test]
fn the_debugger_probe_refuses_without_spawning() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let never_spawn = |_: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
    let never_ask = || -> DebuggerMember { panic!("a refusal must not query membership") };
    for gate in [
        ProbeGate {
            enabled: false,
            check: true,
        },
        ProbeGate {
            enabled: true,
            check: false,
        },
    ] {
        let got = probe_debugger_with(gate, Some(&bundled), never_spawn, never_ask);
        assert_eq!(
            got,
            DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled)
        );
    }
    let got = probe_debugger_with(ProbeGate::on(), None, never_spawn, never_ask);
    assert_eq!(
        got,
        DebuggerProbe::refused(DebuggerProbeLabel::RefusedNoExe)
    );
    let deps = p("/Users//a/aterm/target/debug/deps/aterm_containment-abc123");
    let got = probe_debugger_with(ProbeGate::on(), Some(&deps), never_spawn, never_ask);
    assert_eq!(
        got,
        DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle)
    );
    assert_eq!(
        (got.mode, got.member),
        (DebuggerMode::Unknown, DebuggerMember::Unknown)
    );
}

#[test]
fn the_debugger_probe_spawns_only_the_status_argv() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let mut seen: Vec<Vec<String>> = Vec::new();
    let got = probe_debugger_with(
        ProbeGate::on(),
        Some(&bundled),
        |argv| {
            seen.push(argv.iter().map(|a| (*a).to_string()).collect());
            exited(Some(0), "Developer mode is currently disabled.\n", "")
        },
        || DebuggerMember::Admin,
    );
    assert_eq!(
        got,
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    );
    assert_eq!(
        seen,
        vec![vec![
            DEVTOOLSSECURITY_PATH.to_string(),
            "-status".to_string()
        ]]
    );
    let got = probe_debugger_with(
        ProbeGate::on(),
        Some(&bundled),
        |_| ToolRun::TimedOut,
        || DebuggerMember::Developer,
    );
    assert_eq!(got.mode, DebuggerMode::Unknown);
    assert_eq!(got.member, DebuggerMember::Developer);
    assert_eq!(got.label, DebuggerProbeLabel::TimedOut);
}

const CAPTURED_RECORD: &str = r#"{"timezoneName":"","messageType":"Info","eventType":"logEvent","source":null,"formatString":"Handling access request to %{public}@, from %{public}@, %{public}@","userID":0,"activityIdentifier":223003332,"subsystem":"com.apple.TCC","category":"access","threadID":173778168,"senderImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","backtrace":{"frames":[{"imageOffset":74432,"imageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5"}]},"bootUUID":"00000000-0000-0000-0000-000000000000","processImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","senderImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","timestamp":"2026-10-07 13:58:21.820241+0000","machTimestamp":66148736620656,"eventMessage":"Handling access request to kTCCServiceDeveloperTool, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, pid=56408, auid=501, euid=501, responsible_path=\/Applications\/aterm.app\/Contents\/MacOS\/aterm, binary_path=\/bin\/sh}, ReqResult(Auth Right: Unknown (None), promptType: 1,DB Action:None, UpdateVerifierData)","processImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","traceID":2660156748005636,"processID":588,"senderProgramCounter":74432,"parentActivityIdentifier":0}"#;

const CAPTURED_AT_MS: i64 = 1_791_381_501_820;

const ATERM: &str = "com.aterm.aterm";

const FORGED_SUB: &str = "com.aterm.aterm}Resp:{x";

fn ndjson_escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

fn tccd_message(subject: &str, binary: &str, answer: &str) -> String {
    format!(
        "Handling access request to kTCCServiceDeveloperTool, from Sub:{{{subject}}}Resp:\
         {{TCCDProcess: identifier={subject}, pid=56408, auid=501, euid=501, \
         responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
         binary_path={binary}}}, ReqResult(Auth Right: {answer}, promptType: 1,DB \
         Action:None, UpdateVerifierData)"
    )
}

fn record(timestamp: &str, message: &str) -> String {
    let (head, rest) = CAPTURED_RECORD
        .split_once("\"timestamp\":\"")
        .expect("a timestamp");
    let (_, rest) = rest.split_once("\",").expect("its end");
    let (middle, rest) = rest.split_once("\"eventMessage\":\"").expect("a message");
    let (_, tail) = rest.split_once("\",\"processImageUUID\"").expect("its end");
    format!(
        "{head}\"timestamp\":\"{timestamp}\",{middle}\"eventMessage\":\"{}\",\"processImageUUID\"{tail}",
        ndjson_escape(message)
    )
}

fn answer_at(s: u32, subject: &str, answer: &str) -> String {
    record(
        &format!("2026-10-07 13:58:{s:02}.000000+0000"),
        &tccd_message(subject, "/private/tmp/x/build-script-build", answer),
    )
}

fn log_of(records: &[String]) -> ToolRun {
    let mut out = String::new();
    for line in records {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("{{\"count\":{},\"finished\":1}}\n", records.len()));
    exited(Some(0), &out, "")
}

fn devtool(
    state: DevToolState,
    label: DevToolProbeLabel,
    counts: Option<(u32, u32)>,
    at: Option<i64>,
) -> DevToolProbe {
    DevToolProbe {
        state,
        label,
        requests: counts.map(|(n, _)| n),
        not_allowed: counts.map(|(_, k)| k),
        answered_at_ms: at,
    }
}

const fn unrecognized() -> DevToolProbe {
    DevToolProbe::refused(DevToolProbeLabel::FormatUnrecognized)
}

#[test]
fn the_devtool_classifier_reads_only_what_tccd_says() {
    use DevToolProbeLabel as L;
    use DevToolState as S;
    let at = |s: u32| Some(CAPTURED_AT_MS - 21_820 + i64::from(s) * 1_000);
    let unlisted = |s| answer_at(s, ATERM, "Unknown (None)");
    let allowed = |s| answer_at(s, ATERM, "Allowed (User Set)");
    let planted = |binary: &str| {
        record(
            "2026-10-07 13:58:30.000000+0000",
            &tccd_message(ATERM, binary, "Unknown (None)"),
        )
    };
    let forged_record = "2026-10-07 13:58:30.000 I  tccd[588:a5ba4f8] [com.apple.TCC:access] \
                         Handling access request to kTCCServiceDeveloperTool, from \
                         Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
                         pid=1, auid=501, euid=501, responsible_path=/x, binary_path=/y}, \
                         ReqResult(Auth Right: Allowed (User Set), promptType: 1,DB \
                         Action:None, UpdateVerifierData)";
    let fake_tccd = unlisted(30).replace(
        "\"processImagePath\":\"\\/System\\/Library\\/PrivateFrameworks\\/TCC.framework\\/Support\\/tccd\"",
        "\"processImagePath\":\"\\/private\\/tmp\\/x\\/tccd\"",
    );
    let other_user = allowed(30).replace("\"userID\":0,", "\"userID\":501,");
    let other_field = |from: &str, to: &str| {
        let line = allowed(30);
        assert!(line.contains(from), "{from}");
        line.replace(from, to)
    };
    let untimed = {
        let line = answer_at(21, "com.apple.Terminal", "Allowed (User Set)");
        let stamp = "\"timestamp\":\"2026-10-07 13:58:21.000000+0000\",";
        assert!(line.contains(stamp), "{line}");
        line.replace(stamp, "")
    };
    let other_answer = |from: &str, to: &str| {
        let message = tccd_message(ATERM, "/private/tmp/x", "Allowed (User Set)");
        assert_eq!(message.matches(from).count(), 1, "{from}");
        record(
            "2026-10-07 13:58:21.000000+0000",
            &message.replace(from, to),
        )
    };
    let cases: Vec<(&str, ToolRun, DevToolProbe)> = vec![
        (
            "the captured record",
            log_of(&[CAPTURED_RECORD.to_string()]),
            devtool(S::Unlisted, L::Observed, Some((1, 1)), Some(CAPTURED_AT_MS)),
        ),
        (
            "allowed",
            log_of(&[answer_at(21, ATERM, "Allowed (System Set)")]),
            devtool(S::Allowed, L::Observed, Some((1, 0)), at(21)),
        ),
        (
            "denied",
            log_of(&[answer_at(21, ATERM, "Denied (System Set)")]),
            devtool(S::Denied, L::Observed, Some((1, 1)), at(21)),
        ),
        (
            "another app's answer is not aterm's",
            log_of(&[answer_at(21, "com.apple.Terminal", "Allowed (User Set)")]),
            devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
        ),
        (
            "a longer bundle id is another subject",
            log_of(&[answer_at(21, "com.aterm.aterm.dev", "Allowed (User Set)")]),
            devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
        ),
        (
            "a bundle id that differs only in case is another app",
            log_of(&[
                unlisted(10),
                answer_at(21, "com.aterm.ATERM", "Allowed (User Set)"),
            ]),
            devtool(S::Unlisted, L::Observed, Some((1, 1)), at(10)),
        ),
        (
            "a bundle id with a leading space is another app",
            log_of(&[
                unlisted(10),
                answer_at(21, " com.aterm.aterm", "Allowed (User Set)"),
            ]),
            devtool(S::Unlisted, L::Observed, Some((1, 1)), at(10)),
        ),
        (
            "the closing count alone",
            log_of(&[]),
            devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
        ),
        (
            "allowed, then unlisted",
            log_of(&[allowed(10), unlisted(20)]),
            devtool(S::Unlisted, L::Observed, Some((2, 1)), at(20)),
        ),
        (
            "unlisted, then allowed",
            log_of(&[unlisted(10), allowed(20)]),
            devtool(S::Allowed, L::Observed, Some((2, 1)), at(20)),
        ),
        (
            "newest by tccd's clock, not by line",
            log_of(&[unlisted(20), allowed(10)]),
            devtool(S::Unlisted, L::Observed, Some((2, 1)), at(20)),
        ),
        (
            "a tie on tccd's clock goes to the later line",
            log_of(&[unlisted(20), allowed(20)]),
            devtool(S::Allowed, L::Observed, Some((2, 1)), at(20)),
        ),
        (
            "a newline in a path is only a path",
            log_of(&[planted("/private/tmp/x/a\nb")]),
            devtool(S::Unlisted, L::Observed, Some((1, 1)), at(30)),
        ),
        (
            "limited",
            log_of(&[answer_at(21, ATERM, "Limited (System Set)")]),
            unrecognized(),
        ),
        (
            "unknown for a reason",
            log_of(&[answer_at(21, ATERM, "Unknown (Service Policy)")]),
            unrecognized(),
        ),
        (
            "a bare value",
            log_of(&[answer_at(21, ATERM, "Allowed")]),
            unrecognized(),
        ),
        (
            "an unknown reason",
            log_of(&[answer_at(21, ATERM, "Allowed (Yes)")]),
            unrecognized(),
        ),
        (
            "no promptType",
            log_of(&[record(
                "2026-10-07 13:58:21.000000+0000",
                &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)")
                    .replace(", promptType: 1", ""),
            )]),
            unrecognized(),
        ),
        (
            "a promptType that is not a number",
            log_of(&[other_answer("promptType: 1,", "promptType: x,")]),
            unrecognized(),
        ),
        (
            "an empty promptType",
            log_of(&[other_answer("promptType: 1,", "promptType: ,")]),
            unrecognized(),
        ),
        (
            "no DB Action",
            log_of(&[other_answer(",DB Action:None", "")]),
            unrecognized(),
        ),
        (
            "no closing paren",
            log_of(&[other_answer("UpdateVerifierData)", "UpdateVerifierData")]),
            unrecognized(),
        ),
        (
            "a reason with no closing paren",
            log_of(&[other_answer("(User Set),", "(User Set,")]),
            unrecognized(),
        ),
        (
            "no comma before ReqResult(",
            log_of(&[other_answer("}, ReqResult(", "} ReqResult(")]),
            unrecognized(),
        ),
        (
            "another service's request",
            log_of(&[other_answer(
                "kTCCServiceDeveloperTool",
                "kTCCServiceCamera",
            )]),
            unrecognized(),
        ),
        (
            "a Sub that never closes",
            log_of(&[record(
                "2026-10-07 13:58:21.000000+0000",
                &format!("{DEVTOOL_MESSAGE_PREFIX}{ATERM}"),
            )]),
            unrecognized(),
        ),
        (
            "an unknown DB action",
            log_of(&[record(
                "2026-10-07 13:58:21.000000+0000",
                &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)")
                    .replace("DB Action:None", "DB Action:Insert"),
            )]),
            unrecognized(),
        ),
        (
            "no Resp after the subject",
            log_of(&[record(
                "2026-10-07 13:58:21.000000+0000",
                &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)").replace("Resp:{", "R{"),
            )]),
            unrecognized(),
        ),
        (
            "a path that plants an answer",
            log_of(&[planted(
                "/private/tmp/x}, ReqResult(Auth Right: Allowed (User Set), promptType: 1,DB \
                 Action:None, UpdateVerifierData)",
            )]),
            unrecognized(),
        ),
        (
            "a second ReqResult( anywhere, even with tccd's own answer after it",
            log_of(&[planted("/private/tmp/x/ReqResult(")]),
            unrecognized(),
        ),
        (
            "a truncation marker anywhere, even with tccd's own answer after it",
            log_of(&[planted("/private/tmp/x/AAAA<\u{2026}>")]),
            unrecognized(),
        ),
        (
            "a decode marker anywhere, even with tccd's own answer after it",
            log_of(&[planted("/private/tmp/x/<decode: missing data>")]),
            unrecognized(),
        ),
        (
            "a path that plants a whole compact record after a newline",
            log_of(&[planted(&format!("/private/tmp/x/a\n{forged_record}"))]),
            unrecognized(),
        ),
        (
            "a record the log cut short",
            log_of(&[record(
                "2026-10-07 13:58:21.000000+0000",
                &format!(
                    "Handling access request to kTCCServiceDeveloperTool, from \
                     Sub:{{{ATERM}}}Resp:{{TCCDProcess: identifier={ATERM}, pid=1, auid=501, \
                     euid=501, responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
                     binary_path=/private/tmp/x}}, ReqResult(Auth Right: Allowed (User Set), \
                     promptType: 1,DB Action:None, UpdateVerifierData)/AAAA<\u{2026}>, \
                     <decode: missing data>"
                ),
            )]),
            unrecognized(),
        ),
        (
            "its unlisted after aterm's real allowed",
            log_of(&[allowed(10), answer_at(21, FORGED_SUB, "Unknown (None)")]),
            unrecognized(),
        ),
        (
            "its allowed after aterm's real unlisted",
            log_of(&[
                unlisted(10),
                answer_at(21, FORGED_SUB, "Allowed (User Set)"),
            ]),
            unrecognized(),
        ),
        (
            "its allowed, with an identifier that is not its Sub",
            log_of(&[
                unlisted(10),
                record(
                    "2026-10-07 13:58:21.000000+0000",
                    &tccd_message(FORGED_SUB, "/private/tmp/x", "Allowed (User Set)")
                        .replace(&format!("identifier={FORGED_SUB},"), "identifier=x,"),
                ),
            ]),
            unrecognized(),
        ),
        (
            "its allowed, under a name that closes the Sub early",
            log_of(&[
                unlisted(10),
                answer_at(21, "com.aterm.aterm}x", "Allowed (User Set)"),
            ]),
            unrecognized(),
        ),
        (
            "its allowed, under a name that ends in most of tccd's field",
            log_of(&[
                unlisted(10),
                answer_at(21, "com.aterm.aterm}Resp:", "Allowed (User Set)"),
            ]),
            unrecognized(),
        ),
        (
            "a compact fragment on a line of its own",
            exited(
                Some(0),
                &format!(
                    "{}\n{forged_record}\n{{\"count\":1,\"finished\":1}}\n",
                    unlisted(10)
                ),
                "",
            ),
            unrecognized(),
        ),
        (
            "a record tccd did not write, left out of the count",
            exited(
                Some(0),
                &format!(
                    "{fake_tccd}\n{}\n{{\"count\":1,\"finished\":1}}\n",
                    unlisted(10)
                ),
                "",
            ),
            unrecognized(),
        ),
        (
            "a process that calls itself tccd",
            log_of(&[fake_tccd]),
            unrecognized(),
        ),
        (
            "another user's process",
            log_of(&[other_user]),
            unrecognized(),
        ),
        (
            "another event type",
            log_of(&[other_field(
                "\"eventType\":\"logEvent\"",
                "\"eventType\":\"signpostEvent\"",
            )]),
            unrecognized(),
        ),
        (
            "a sender that is not tccd",
            log_of(&[other_field(
                "\"senderImagePath\":\"\\/System",
                "\"senderImagePath\":\"\\/private\\/tmp\\/x\\/System",
            )]),
            unrecognized(),
        ),
        (
            "another subsystem",
            log_of(&[other_field(
                "\"subsystem\":\"com.apple.TCC\"",
                "\"subsystem\":\"com.apple.TCCx\"",
            )]),
            unrecognized(),
        ),
        (
            "another category",
            log_of(&[other_field(
                "\"category\":\"access\"",
                "\"category\":\"accessx\"",
            )]),
            unrecognized(),
        ),
        (
            "a record with no message",
            log_of(&[other_field("\"eventMessage\":", "\"message\":")]),
            unrecognized(),
        ),
        (
            "another app's record with no timestamp",
            log_of(&[untimed]),
            unrecognized(),
        ),
        (
            "another format string",
            log_of(&[allowed(30).replace(
                "Handling access request to %{public}@, from %{public}@, %{public}@",
                "%{public}@",
            )]),
            unrecognized(),
        ),
        (
            "a repeated key",
            log_of(&[unlisted(30).replacen(
                "\"eventMessage\":",
                &format!(
                    "\"eventMessage\":\"{}\",\"eventMessage\":",
                    ndjson_escape(&tccd_message(ATERM, "/bin/sh", "Allowed (User Set)"))
                ),
                1,
            )]),
            unrecognized(),
        ),
        (
            "a malformed timestamp",
            log_of(&[record(
                "2026-10-07T13:58:21+0000",
                &tccd_message(ATERM, "/bin/sh", "Unknown (None)"),
            )]),
            unrecognized(),
        ),
        ("empty output", exited(Some(0), "", ""), unrecognized()),
        (
            "no closing count",
            exited(Some(0), &format!("{}\n", allowed(10)), ""),
            unrecognized(),
        ),
        (
            "a count that disagrees",
            exited(
                Some(0),
                &format!("{}\n{{\"count\":2,\"finished\":1}}\n", allowed(10)),
                "",
            ),
            unrecognized(),
        ),
        (
            "a record after the count",
            exited(
                Some(0),
                &format!("{{\"count\":0,\"finished\":1}}\n{}\n", allowed(10)),
                "",
            ),
            unrecognized(),
        ),
        (
            "an unfinished read",
            exited(
                Some(0),
                &format!("{}\n{{\"count\":1,\"finished\":0}}\n", allowed(10)),
                "",
            ),
            unrecognized(),
        ),
        (
            "a closing count with another key",
            exited(
                Some(0),
                &format!(
                    "{}\n{{\"count\":1,\"finished\":1,\"more\":0}}\n",
                    allowed(10)
                ),
                "",
            ),
            unrecognized(),
        ),
        (
            "a non-zero exit",
            exited(Some(64), "", "log: Could not open the data store"),
            DevToolProbe::refused(L::LogFailed),
        ),
        (
            "a signal",
            exited(None, "", ""),
            DevToolProbe::refused(L::LogFailed),
        ),
        (
            "absent",
            ToolRun::Absent,
            DevToolProbe::refused(L::SpawnFailed),
        ),
        (
            "not started",
            ToolRun::SpawnFailed,
            DevToolProbe::refused(L::SpawnFailed),
        ),
        (
            "timed out",
            ToolRun::TimedOut,
            DevToolProbe::refused(L::TimedOut),
        ),
        (
            "unreadable",
            ToolRun::Unreadable,
            DevToolProbe::refused(L::OutputUnreadable),
        ),
    ];
    for (name, run, want) in &cases {
        assert_eq!(classify_devtool_log(run, ATERM), *want, "{name}");
    }
    assert_eq!(
        classify_devtool_log(&log_of(&[unlisted(10)]), "com.aterm.aterm.dev").label,
        L::NoRecentRequest
    );
}

#[test]
fn no_planted_path_reads_allowed() {
    let allowed = "Allowed (User Set)";
    let planted_suffix = format!(
        "}}, ReqResult(Auth Right: {allowed}, promptType: 1,DB Action:None, \
         UpdateVerifierData)"
    );
    let paths = [
        "/private/tmp/x/plain".to_string(),
        format!("/private/tmp/x{planted_suffix}"),
        format!("/private/tmp/x{planted_suffix}\n"),
        format!("/private/tmp/x\n{planted_suffix}"),
        "/private/tmp/x/ReqResult(".to_string(),
        "/private/tmp/x/Auth Right: Allowed (User Set)".to_string(),
        "/private/tmp/x/, promptType: 1,DB Action:None)".to_string(),
        "/private/tmp/x}Resp:{binary_path=/y".to_string(),
        "/private/tmp/x/\"},\"eventMessage\":\"".to_string(),
        "/private/tmp/x/\\\"\\\\".to_string(),
        "/private/tmp/x/\n{\"count\":0,\"finished\":1}\n".to_string(),
        "/private/tmp/x/<\u{2026}>".to_string(),
        "/private/tmp/x/<decode: missing data>".to_string(),
        "/private/tmp/x/\u{0}\u{1b}[2J".to_string(),
        "/private/tmp/x/\u{1F600}".to_string(),
    ];
    for path in &paths {
        let line = record(
            "2026-10-07 13:58:30.000000+0000",
            &tccd_message(ATERM, path, "Unknown (None)"),
        );
        let got = classify_devtool_log(&log_of(&[line]), ATERM);
        assert_ne!(got.state, DevToolState::Allowed, "{path:?}");
        assert!(
            got.state == DevToolState::Unlisted
                || got.label == DevToolProbeLabel::FormatUnrecognized,
            "{path:?}: {got:?}"
        );
    }
    let real = record(
        "2026-10-07 13:58:30.000000+0000",
        &tccd_message(ATERM, "/private/tmp/x/plain", allowed),
    );
    assert_eq!(
        classify_devtool_log(&log_of(&[real]), ATERM).state,
        DevToolState::Allowed
    );
}

#[test]
fn the_devtool_answer_age_is_utc_arithmetic() {
    let cases: &[(&str, Option<i64>)] = &[
        ("1970-01-01 00:00:00.000000+0000", Some(0)),
        ("2026-10-07 13:58:21.820241+0000", Some(CAPTURED_AT_MS)),
        ("2026-10-07 06:58:21.820241-0700", Some(CAPTURED_AT_MS)),
        ("2026-10-07 13:58:21+0000", Some(CAPTURED_AT_MS - 820)),
        ("2026-10-07 13:58:21.8+0000", Some(CAPTURED_AT_MS - 20)),
        ("2024-02-29 00:00:00.000000+0000", Some(1_709_164_800_000)),
        ("2000-02-29 00:00:00.000000+0000", Some(951_782_400_000)),
        ("2025-12-31 23:59:59.999000+0000", Some(1_767_225_599_999)),
        ("2026-01-01 00:00:00.000000+0000", Some(1_767_225_600_000)),
        ("2026-13-01 00:00:00.000000+0000", None),
        ("2026-10-32 00:00:00.000000+0000", None),
        ("2026-10-07 24:00:00.000000+0000", None),
        ("2026-10-07 13:60:00.000000+0000", None),
        ("2026-10-07 13:58:60.000000+0000", None),
        ("2026-10-07T13:58:21.820241+0000", None),
        ("2026-10-07 13:58:21.820241", None),
        ("2026-10-07 13:58:21.+0000", None),
        ("2026-10-07 13:58:21.8202410000+0000", None),
        ("2026-10-07 13:58:21.820241Z0000", None),
        ("2026-10-07 13:58:21.820241+2400", None),
        ("26-10-07 13:58:21.820241+0000", None),
        ("", None),
        ("2026-10-00 00:00:00.000000+0000", None),
        ("2026-10-07-01 13:58:21.820241+0000", None),
        ("2026-+1-07 13:58:21.820241+0000", None),
        ("2026-10-07 13:58:21:00.820241+0000", None),
        ("2026-10-07 13:58:21.8a+0000", None),
        ("2026-10-07 13:58:21.820241+0060", None),
    ];
    for (text, want) in cases {
        assert_eq!(devtool_timestamp_ms(text), *want, "{text:?}");
    }
    let midnight =
        |y: i64, m: i64, d: i64| devtool_timestamp_ms(&format!("{y}-{m:02}-{d:02} 00:00:00+0000"));
    for y in [2000, 2023, 2024, 2100] {
        for m in 1..=12 {
            let last = (28..=31)
                .rev()
                .find(|&d| midnight(y, m, d).is_some())
                .expect("every month has a 28th");
            let next = if m == 12 {
                midnight(y + 1, 1, 1)
            } else {
                midnight(y, m + 1, 1)
            };
            assert_eq!(
                midnight(y, m, last).map(|ms| ms + 86_400_000),
                next,
                "{y}-{m:02}-{last}"
            );
        }
    }
    assert_eq!(devtool_answer_age_ms(1_000, 5_100), 4_100);
    assert_eq!(devtool_answer_age_ms(5_000, 5_000), 0);
    assert_eq!(devtool_answer_age_ms(9_000, 5_000), 0, "a future stamp");
    assert_eq!(
        devtool_answer_age_ms(i64::MIN, i64::MAX),
        i64::MAX.unsigned_abs(),
        "saturates"
    );
}

#[test]
fn the_ndjson_reader_is_strict() {
    let fields = json::object_line(
        r#"{"a":"x\/y\né😀","n":0,"m":12,"z":null,"t":true,"f":false,"o":{"p":[1,2.5,-3e2,4E+1,5e-1,[]]}}"#,
    )
    .expect("well formed");
    assert_eq!(json::str(&fields, "a"), Some("x/y\n\u{e9}\u{1F600}"));
    assert_eq!(json::uint(&fields, "n"), Some(0));
    assert_eq!(json::uint(&fields, "m"), Some(12));
    assert_eq!(json::uint(&fields, "a"), None, "a string is not a number");
    assert_eq!(json::str(&fields, "missing"), None);
    let u = |hex: &str| format!("{}u{hex}", '\u{5c}');
    let escaped = format!("{{\"a\":\"{}{}{}\"}}", u("00e9"), u("d83d"), u("de00"));
    let escaped = json::object_line(&escaped).expect("escapes");
    assert_eq!(json::str(&escaped, "a"), Some("\u{e9}\u{1F600}"));
    let unpaired = format!("{{\"a\":\"{}{}\"}}", u("d83d"), u("0041"));
    assert!(json::object_line(&unpaired).is_none(), "{unpaired}");
    for bad in [
        r#"{"a":"\u+0e9"}"#,
        "",
        "[]",
        "\"x\"",
        "{",
        "{}x",
        "{} {}",
        r#"{"a":1,"a":2}"#,
        r#"{"a":"\ud83d"}"#,
        r#"{"a":"\udc00"}"#,
        "{\"a\":\"\u{1}\"}",
        "{\"a\":\"line\nbreak\"}",
        r#"{"a":"\x"}"#,
        r#"{"a":01}"#,
        r#"{"a":-}"#,
        r#"{"a":1.}"#,
        r#"{"a":1e}"#,
        r#"{"a":tru}"#,
        r#"{"a":[1,]}"#,
        r#"{"a":1,}"#,
        r#"{a:1}"#,
    ] {
        assert!(json::object_line(bad).is_none(), "{bad:?}");
    }
    assert!(json::object_line("{}").is_some());
    let deep = format!("{{\"a\":{}1{}}}", "[".repeat(20), "]".repeat(20));
    assert!(json::object_line(&deep).is_none(), "nesting is bounded");
    let shallow = format!("{{\"a\":{}1{}}}", "[".repeat(3), "]".repeat(3));
    assert!(json::object_line(&shallow).is_some());
    let nested = |arrays: usize| format!("{{\"a\":{}1{}}}", "[".repeat(arrays), "]".repeat(arrays));
    assert!(json::object_line(&nested(15)).is_some());
    assert!(json::object_line(&nested(16)).is_none());
    for (text, want) in [("7", Some(7)), ("-7", None), ("7.0", None), ("7e0", None)] {
        let fields = json::object_line(&format!("{{\"n\":{text}}}")).expect("a number");
        assert_eq!(json::uint(&fields, "n"), want, "{text}");
    }
}

#[test]
fn the_devtool_probe_refuses_without_spawning() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let never_spawn = |_: &str, _: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
    let never_look = || -> bool { panic!("a refusal ahead of the store check reads nothing") };
    for gate in [
        ProbeGate {
            enabled: false,
            check: true,
        },
        ProbeGate {
            enabled: true,
            check: false,
        },
    ] {
        assert_eq!(
            probe_devtool_with(gate, Some(&bundled), ATERM, never_look, never_spawn),
            DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
        );
    }
    assert_eq!(
        probe_devtool_with(ProbeGate::on(), None, ATERM, never_look, never_spawn),
        DevToolProbe::refused(DevToolProbeLabel::RefusedNoExe)
    );
    let deps = p("/private/tmp/x/target/debug/deps/aterm_containment-abc123");
    assert_eq!(
        probe_devtool_with(ProbeGate::on(), Some(&deps), ATERM, never_look, never_spawn),
        DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle)
    );
    for subject in [
        "",
        "  ",
        "-x",
        "a b",
        "com.aterm.aterm}Resp:{",
        "com.aterm.aterm\"",
        "a/b",
        "a\u{0}b",
        "caf\u{e9}",
    ] {
        assert!(!devtool_subject_is_usable(subject), "{subject:?}");
        assert_eq!(
            probe_devtool_with(
                ProbeGate::on(),
                Some(&bundled),
                subject,
                never_look,
                never_spawn
            ),
            DevToolProbe::refused(DevToolProbeLabel::RefusedNoSubject),
            "{subject:?}"
        );
    }
    assert_eq!(
        probe_devtool_with(
            ProbeGate::on(),
            Some(&bundled),
            ATERM,
            || false,
            never_spawn
        ),
        DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
        "not an administrator: the store is not readable"
    );
    assert_eq!(
        probe_devtool_with(
            ProbeGate::on(),
            Some(&bundled),
            ATERM,
            || true,
            |_, _| exited(Some(64), "", "log: bad predicate")
        ),
        DevToolProbe::refused(DevToolProbeLabel::LogFailed),
    );
    assert!(devtool_subject_is_usable(ATERM));
    assert!(devtool_subject_is_usable("com.aterm.aterm.dev"));
}

#[test]
fn the_devtool_probe_spawns_only_the_log_argv() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let subject = "com.example.subject";
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    let mut looked = 0;
    let got = probe_devtool_with(
        ProbeGate::on(),
        Some(&bundled),
        subject,
        || {
            looked += 1;
            true
        },
        |program, args| {
            seen.push((
                program.to_string(),
                args.iter().map(|a| (*a).to_string()).collect(),
            ));
            log_of(&[answer_at(21, subject, "Unknown (None)")])
        },
    );
    assert_eq!(looked, 1);
    assert_eq!(got.state, DevToolState::Unlisted);
    assert_eq!(seen.len(), 1, "exactly one run");
    let (program, args) = &seen[0];
    assert_eq!(program, LOG_TOOL);
    assert_eq!(
        program, "/usr/bin/log",
        "named absolutely: `log` is a zsh builtin"
    );
    assert_eq!(args, &DEVTOOL_LOG_ARGS.map(str::to_string));
    assert!(
        args.iter()
            .all(|a| !a.contains(subject) && !a.contains("example")),
        "the subject never reaches an argv: {args:?}"
    );
    let last = args
        .iter()
        .position(|a| a == "--last")
        .map(|i| args[i + 1].clone());
    assert_eq!(last, Some(format!("{}m", DEVTOOL_WINDOW_S / 60)));
    assert!(args.contains(&"ndjson".to_string()), "the escaped framing");
}

const TCC_ACCESS_DDL: &str = "CREATE TABLE access (service TEXT NOT NULL, \
    client TEXT NOT NULL, client_type INTEGER NOT NULL, auth_value INTEGER NOT NULL, \
    csreq BLOB, last_modified INTEGER NOT NULL DEFAULT 0)";

fn push_sqlite_uvarint(out: &mut Vec<u8>, mut v: u64) {
    let mut rev = [0u8; 9];
    let mut len = 0usize;
    rev[0] = (v & 0x7f) as u8;
    len += 1;
    v >>= 7;
    while v > 0 {
        rev[len] = 0x80 | ((v & 0x7f) as u8);
        len += 1;
        v >>= 7;
    }
    for i in (0..len).rev() {
        out.push(rev[i]);
    }
}

fn encode_sqlite_record(cols: &[aterm_uds::sqlite_read::SqlValue<'_>]) -> Vec<u8> {
    use aterm_uds::sqlite_read::SqlValue;
    let mut serials = Vec::new();
    let mut body = Vec::new();
    for col in cols {
        match col {
            SqlValue::Null => push_sqlite_uvarint(&mut serials, 0),
            SqlValue::Int(n) => {
                push_sqlite_uvarint(&mut serials, 6);
                body.extend_from_slice(&n.to_be_bytes());
            }
            SqlValue::Float(f) => {
                push_sqlite_uvarint(&mut serials, 7);
                body.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            SqlValue::Blob(b) => {
                push_sqlite_uvarint(&mut serials, (b.len() as u64) * 2 + 12);
                body.extend_from_slice(b);
            }
            SqlValue::Text(s) => {
                push_sqlite_uvarint(&mut serials, (s.len() as u64) * 2 + 13);
                body.extend_from_slice(s.as_bytes());
            }
        }
    }
    let mut hdr_len = Vec::new();
    let mut total_hdr = (serials.len() + 1) as u64;
    loop {
        hdr_len.clear();
        push_sqlite_uvarint(&mut hdr_len, total_hdr);
        let actual = (hdr_len.len() + serials.len()) as u64;
        if actual == total_hdr {
            break;
        }
        total_hdr = actual;
    }
    let mut out = hdr_len;
    out.extend_from_slice(&serials);
    out.extend_from_slice(&body);
    out
}

fn encode_leaf_cell(rowid: u64, record: &[u8]) -> Vec<u8> {
    let mut cell = Vec::new();
    push_sqlite_uvarint(&mut cell, record.len() as u64);
    push_sqlite_uvarint(&mut cell, rowid);
    cell.extend_from_slice(record);
    cell
}

fn sqlite_db_with_table(
    table_name: &str,
    ddl: &str,
    rows: &[Vec<aterm_uds::sqlite_read::SqlValue<'_>>],
) -> Vec<u8> {
    use aterm_uds::sqlite_read::SqlValue;
    let mut db = vec![0u8; 8192];
    db[..16].copy_from_slice(b"SQLite format 3\0");
    db[16..18].copy_from_slice(&4096u16.to_be_bytes());
    db[18] = 1;
    db[19] = 1;
    db[28..32].copy_from_slice(&2u32.to_be_bytes());
    db[56..60].copy_from_slice(&1u32.to_be_bytes());

    let schema_rec = encode_sqlite_record(&[
        SqlValue::Text("table".into()),
        SqlValue::Text(table_name.into()),
        SqlValue::Text(table_name.into()),
        SqlValue::Int(2),
        SqlValue::Text(ddl.into()),
    ]);
    let schema_cell = encode_leaf_cell(1, &schema_rec);
    let schema_off = 4096 - schema_cell.len();
    db[100] = 0x0d;
    db[103..105].copy_from_slice(&1u16.to_be_bytes());
    db[105..107].copy_from_slice(&u16::try_from(schema_off).unwrap().to_be_bytes());
    db[108..110].copy_from_slice(&u16::try_from(schema_off).unwrap().to_be_bytes());
    db[schema_off..4096].copy_from_slice(&schema_cell);

    let page2 = &mut db[4096..8192];
    page2[0] = 0x0d;
    page2[3..5].copy_from_slice(&u16::try_from(rows.len()).unwrap().to_be_bytes());
    let mut cursor = 4096usize;
    for (idx, row) in rows.iter().enumerate() {
        let cell = encode_leaf_cell((idx + 1) as u64, &encode_sqlite_record(row));
        cursor -= cell.len();
        assert!(cursor >= 8 + rows.len() * 2, "page 2 cell room");
        page2[cursor..cursor + cell.len()].copy_from_slice(&cell);
        let ptr = 8 + idx * 2;
        page2[ptr..ptr + 2].copy_from_slice(&u16::try_from(cursor).unwrap().to_be_bytes());
    }
    page2[5..7].copy_from_slice(&u16::try_from(cursor).unwrap().to_be_bytes());
    db
}

fn db_row_values<'a>(
    service: &'a str,
    client: aterm_uds::sqlite_read::SqlValue<'a>,
    kind: aterm_uds::sqlite_read::SqlValue<'a>,
    auth: aterm_uds::sqlite_read::SqlValue<'a>,
    modified: aterm_uds::sqlite_read::SqlValue<'a>,
) -> Vec<aterm_uds::sqlite_read::SqlValue<'a>> {
    use aterm_uds::sqlite_read::SqlValue;
    vec![
        SqlValue::Text(service.into()),
        client,
        kind,
        auth,
        SqlValue::Null,
        modified,
    ]
}

fn db_row(
    client: &str,
    kind: u32,
    auth: u32,
    modified: i64,
) -> Vec<aterm_uds::sqlite_read::SqlValue<'static>> {
    use aterm_uds::sqlite_read::SqlValue;
    vec![
        SqlValue::Text(DEVTOOL_SERVICE.into()),
        SqlValue::Text(client.to_owned().into()),
        SqlValue::Int(i64::from(kind)),
        SqlValue::Int(i64::from(auth)),
        SqlValue::Null,
        SqlValue::Int(modified),
    ]
}

fn db_of(rows: &[Vec<aterm_uds::sqlite_read::SqlValue<'_>>]) -> Vec<u8> {
    sqlite_db_with_table("access", TCC_ACCESS_DDL, rows)
}

fn listed(listing: DevToolListing, modified_at_s: Option<i64>) -> DevToolList {
    listed_of(listing, modified_at_s, 1)
}

fn listed_of(listing: DevToolListing, modified_at_s: Option<i64>, rows: u32) -> DevToolList {
    DevToolList {
        listing,
        label: DevToolListLabel::Read,
        modified_at_s,
        rows: Some(rows),
    }
}

#[test]
fn the_list_classifier_reads_only_the_subjects_bundle_row() {
    use aterm_uds::sqlite_read::SqlValue;
    let unrecognized = DevToolList::refused(DevToolListLabel::FormatUnrecognized);
    let before = db_of(&[
        db_row("com.apple.Terminal", 0, 0, 1_781_819_370),
        db_row_values(
            "kTCCServiceAccessibility",
            SqlValue::Text(ATERM.into()),
            SqlValue::Int(0),
            SqlValue::Int(2),
            SqlValue::Int(999),
        ),
    ]);
    let after = db_of(&[
        db_row("com.apple.Terminal", 0, 2, 1_792_020_639),
        db_row(ATERM, 0, 2, 1_792_020_637),
    ]);
    assert_eq!(
        classify_devtool_db_bytes(&before, None, ATERM),
        Some(listed(DevToolListing::Absent, None))
    );
    assert_eq!(
        classify_devtool_db_bytes(&after, None, ATERM),
        Some(listed_of(DevToolListing::On, Some(1_792_020_637), 2))
    );
    assert_eq!(
        classify_devtool_db_bytes(&before, None, "com.apple.Terminal"),
        Some(listed(DevToolListing::Off, Some(1_781_819_370)))
    );
    for (name, rows, want) in [
        (
            "an empty list: no pane yet",
            vec![],
            listed_of(DevToolListing::Absent, None, 0),
        ),
        (
            "switched on",
            vec![db_row(ATERM, 0, 2, 5)],
            listed(DevToolListing::On, Some(5)),
        ),
        (
            "switched off",
            vec![db_row(ATERM, 0, 0, 6)],
            listed(DevToolListing::Off, Some(6)),
        ),
        (
            "blob client bytes decode lossily as UTF-8",
            vec![db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Blob(ATERM.as_bytes().into()),
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Int(11),
            )],
            listed(DevToolListing::On, Some(11)),
        ),
        (
            "another app's row",
            vec![db_row("com.example.other", 0, 2, 1)],
            listed(DevToolListing::Absent, None),
        ),
        (
            "a path client spelled as the id is not the bundle's row",
            vec![db_row(ATERM, 1, 2, 1)],
            listed(DevToolListing::Absent, None),
        ),
        (
            "a longer id that starts with ours",
            vec![db_row("com.aterm.aterm.dev", 0, 2, 1)],
            listed(DevToolListing::Absent, None),
        ),
        (
            "a client that only contains ours",
            vec![db_row("x|0|2|9\ncom.aterm.aterm", 0, 2, 1)],
            listed(DevToolListing::Absent, None),
        ),
        (
            "ours in another case is another id",
            vec![db_row(&ATERM.to_ascii_uppercase(), 0, 2, 1)],
            listed(DevToolListing::Absent, None),
        ),
        (
            "rows are counted whoever they name",
            vec![db_row("a.b", 0, 0, 1), db_row("/bin/x", 1, 2, 1)],
            listed_of(DevToolListing::Absent, None, 2),
        ),
        (
            "two rows for the subject",
            vec![db_row(ATERM, 0, 2, 1), db_row(ATERM, 0, 0, 2)],
            unrecognized,
        ),
        (
            "an auth value that is neither on nor off",
            vec![db_row(ATERM, 0, 3, 1)],
            unrecognized,
        ),
        ("unknown", vec![db_row(ATERM, 0, 1, 1)], unrecognized),
    ] {
        assert_eq!(
            classify_devtool_db_bytes(&db_of(&rows), None, ATERM),
            Some(want),
            "{name}"
        );
    }
    let ours = db_row(ATERM, 0, 2, 1);
    for (why, bad) in [
        (
            "null client",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Null,
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Int(1),
            ),
        ),
        (
            "integer client",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Int(7),
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Int(1),
            ),
        ),
        (
            "negative client_type",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(-1),
                SqlValue::Int(2),
                SqlValue::Int(1),
            ),
        ),
        (
            "client_type overflow",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(4_294_967_296),
                SqlValue::Int(2),
                SqlValue::Int(1),
            ),
        ),
        (
            "negative auth_value",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(0),
                SqlValue::Int(-2),
                SqlValue::Int(1),
            ),
        ),
        (
            "auth_value overflow",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(0),
                SqlValue::Int(4_294_967_296),
                SqlValue::Int(1),
            ),
        ),
        (
            "float last_modified",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Float(1.5),
            ),
        ),
        (
            "negative last_modified",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Int(-1),
            ),
        ),
        (
            "last_modified >= 10^18",
            db_row_values(
                DEVTOOL_SERVICE,
                SqlValue::Text("a.b".into()),
                SqlValue::Int(0),
                SqlValue::Int(2),
                SqlValue::Int(1_000_000_000_000_000_000),
            ),
        ),
    ] {
        let mixed = db_of(&[ours.clone(), bad]);
        assert_eq!(
            classify_devtool_db_bytes(&mixed, None, ATERM),
            Some(unrecognized),
            "{why}"
        );
    }
    assert_eq!(
        classify_devtool_db_bytes(b"not sqlite", None, ATERM),
        None,
        "corrupt header"
    );
    let no_access = sqlite_db_with_table("other", "CREATE TABLE other (id INTEGER)", &[]);
    assert_eq!(
        classify_devtool_db_bytes(&no_access, None, ATERM),
        None,
        "missing table access"
    );
    let missing_col = sqlite_db_with_table(
        "access",
        "CREATE TABLE access (service TEXT, client TEXT, client_type INTEGER, auth_value INTEGER)",
        &[],
    );
    assert_eq!(
        classify_devtool_db_bytes(&missing_col, None, ATERM),
        None,
        "missing last_modified column"
    );
}

#[test]
fn the_list_probe_refuses_without_opening_or_spawning() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let tcc = Path::new(SYSTEM_TCC_DB);
    let never_open = |_: &Path| -> ProbeOutcome { panic!("a refusal opens nothing") };
    let never_inspect = |_: &[u8], _: Option<&[u8]>| -> Option<DevToolList> {
        panic!("a refusal must inspect nothing")
    };
    let off = ProbeGate {
        enabled: true,
        check: false,
    };
    let refused = |label| DevToolList::refused(label);
    assert_eq!(
        probe_devtool_list_with(off, Some(&bundled), ATERM, tcc, never_open, never_inspect),
        refused(DevToolListLabel::RefusedDisabled)
    );
    assert_eq!(
        probe_devtool_list_with(ProbeGate::on(), None, ATERM, tcc, never_open, never_inspect),
        refused(DevToolListLabel::RefusedNoExe)
    );
    assert_eq!(
        probe_devtool_list_with(
            ProbeGate::on(),
            Some(Path::new(
                "/private/tmp/x/target/debug/deps/aterm_containment-abc"
            )),
            ATERM,
            tcc,
            never_open,
            never_inspect
        ),
        refused(DevToolListLabel::RefusedOutOfBundle)
    );
    assert_eq!(
        probe_devtool_list_with(
            ProbeGate::on(),
            Some(&bundled),
            "com.aterm.aterm|0|2",
            tcc,
            never_open,
            never_inspect
        ),
        refused(DevToolListLabel::RefusedNoSubject)
    );
    for (errno, label) in [
        (ERRNO_EPERM, DevToolListLabel::NoFullDiskAccess),
        (ERRNO_ENOENT, DevToolListLabel::OpenFailed),
        (13, DevToolListLabel::OpenFailed),
    ] {
        assert_eq!(
            probe_devtool_list_with(
                ProbeGate::on(),
                Some(&bundled),
                ATERM,
                tcc,
                |_| ProbeOutcome::Errno(errno),
                never_inspect
            ),
            refused(label),
            "errno {errno}"
        );
    }
}

#[test]
fn the_list_probe_opens_once_and_reads_in_process_without_spawning() {
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let subject = "com.example.subject";
    let dir = crate::test_scratch_dir("list-open");
    let db = dir.path().join("TCC.db");
    std::fs::write(&db, db_of(&[db_row(subject, 0, 0, 7)])).expect("db");
    let mut opened: Vec<PathBuf> = Vec::new();
    let mut reads = 0usize;
    let got = probe_devtool_list_with(
        ProbeGate::on(),
        Some(&bundled),
        subject,
        &db,
        |db| {
            opened.push(db.to_path_buf());
            ProbeOutcome::Ok
        },
        |main, wal| {
            reads += 1;
            assert!(wal.is_none(), "no -wal beside a plain db");
            classify_devtool_db_bytes(main, wal, subject)
        },
    );
    assert_eq!(got, listed(DevToolListing::Off, Some(7)));
    assert_eq!(opened, vec![db.clone()]);
    assert_eq!(reads, 1, "one in-memory snapshot read");
    assert_eq!(
        SYSTEM_TCC_DB,
        "/Library/Application Support/com.apple.TCC/TCC.db"
    );

    #[cfg(target_os = "macos")]
    {
        use std::io::{BufRead as _, Write as _};
        let wal_db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("TCC-wal.db");
        let mut holder = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&wal_db)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("sqlite3 holder");
        let mut stdin = holder.stdin.take().expect("stdin");
        writeln!(
            stdin,
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; {TCC_ACCESS_DDL}; \
             INSERT INTO access VALUES ('kTCCServiceDeveloperTool', '{subject}', 0, 2, NULL, 42); \
             SELECT 'wal-ready';"
        )
        .expect("write wal");
        stdin.flush().expect("flush wal");
        let mut lines = std::io::BufReader::new(holder.stdout.take().expect("stdout")).lines();
        while let Some(Ok(line)) = lines.next() {
            if line == "wal-ready" {
                break;
            }
        }
        let mut saw_wal = false;
        let wal_got = probe_devtool_list_with(
            ProbeGate::on(),
            Some(&bundled),
            subject,
            &wal_db,
            |_| ProbeOutcome::Ok,
            |main, wal| {
                saw_wal = wal.is_some();
                classify_devtool_db_bytes(main, wal, subject)
            },
        );
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(saw_wal, "uncheckpointed -wal bytes read in memory");
        assert_eq!(wal_got, listed(DevToolListing::On, Some(42)));
    }
}

#[test]
fn a_list_read_that_meets_a_write_is_read_again() {
    let dir = crate::test_scratch_dir("list-retry");
    let db = dir.path().join("TCC.db");
    let subject = "com.example.subject";
    std::fs::write(&db, db_of(&[db_row(subject, 0, 2, 9)])).expect("db");
    let write = || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&db)
            .expect("open");
        std::io::Write::write_all(&mut f, b"x").expect("write");
    };
    let mut runs = 0;
    let got = read_devtool_db(&db, |_, _| {
        runs += 1;
        write();
        Some(listed(DevToolListing::On, Some(9)))
    });
    assert_eq!(got, DevToolList::refused(DevToolListLabel::ToolFailed));
    assert_eq!(
        runs, DEVTOOL_DB_READS,
        "read again while each read met a write"
    );
    let mut runs = 0;
    let got = read_devtool_db(&db, |_, _| {
        runs += 1;
        if runs == 1 {
            write();
        }
        Some(listed(DevToolListing::On, Some(9)))
    });
    assert_eq!(got, listed(DevToolListing::On, Some(9)));
    assert_eq!(runs, 2);
}

#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn the_list_read_never_locks_out_a_writer_with_no_busy_timeout() {
    let dir = crate::test_scratch_dir("list-nolock");
    let db = std::fs::canonicalize(dir.path())
        .expect("canonical")
        .join("TCC.db");
    let subject = "com.example.subject";
    let sqlite = |sql: &str| {
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(sql)
            .output()
            .expect("sqlite3 runs")
    };
    let made = sqlite(
        "CREATE TABLE access (service TEXT NOT NULL, client TEXT NOT NULL, \
         client_type INTEGER NOT NULL, auth_value INTEGER NOT NULL, csreq BLOB, \
         last_modified INTEGER NOT NULL DEFAULT 0); \
         INSERT INTO access VALUES ('kTCCServiceDeveloperTool', 'com.example.subject', \
         0, 0, NULL, 7); \
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000) \
         INSERT INTO access SELECT 'kTCCServiceAccessibility', 'com.example.app' || i, \
         0, 2, randomblob(1000), 0 FROM n;",
    );
    assert!(made.status.success(), "{made:?}");
    let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
    let switch_on = "BEGIN; UPDATE access SET auth_value = 2, last_modified = 8 \
                     WHERE service = 'kTCCServiceDeveloperTool'; COMMIT;";
    let (read, caught) = aterm_uds::foreign_db::witness::while_read(
        &db,
        0x4000_0000,
        512,
        || {
            probe_devtool_list_with(
                ProbeGate::on(),
                Some(&bundled),
                subject,
                &db,
                |_| ProbeOutcome::Ok,
                |main, wal| classify_devtool_db_bytes(main, wal, subject),
            )
        },
        || sqlite(switch_on),
    );
    let caught = caught.map(|(pid, out)| {
        (
            pid,
            out.status,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    });
    assert_eq!(caught, None, "the read held a lock a writer met");
    assert_eq!(read, listed(DevToolListing::Off, Some(7)));
    let wrote = sqlite(switch_on);
    assert!(wrote.status.success(), "{wrote:?}");
    let after = probe_devtool_list_with(
        ProbeGate::on(),
        Some(&bundled),
        subject,
        &db,
        |_| ProbeOutcome::Ok,
        |main, wal| classify_devtool_db_bytes(main, wal, subject),
    );
    assert_eq!(after, listed(DevToolListing::On, Some(8)));
}

#[test]
fn the_grant_joins_the_switch_and_tccds_answer() {
    use DevToolGrant::{Granted, NotGranted, Unknown};
    let tccd = |state, at_ms: i64| DevToolProbe {
        state,
        label: DevToolProbeLabel::Observed,
        requests: Some(3),
        not_allowed: Some(u32::from(state != DevToolState::Allowed) * 3),
        answered_at_ms: Some(at_ms),
    };
    let quiet = DevToolProbe {
        requests: Some(0),
        not_allowed: Some(0),
        ..DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
    };
    let no_log = DevToolProbe::refused(DevToolProbeLabel::LogUnavailable);
    let unread = DevToolList::refused(DevToolListLabel::NoFullDiskAccess);
    let on_at_100 = listed(DevToolListing::On, Some(100));
    let off_at_100 = listed(DevToolListing::Off, Some(100));
    let absent = listed(DevToolListing::Absent, None);
    for (name, list, log, want) in [
        ("on, nothing from tccd", on_at_100, quiet, Granted),
        ("on, tccd unreadable", on_at_100, no_log, Granted),
        ("off", off_at_100, quiet, NotGranted),
        ("absent", absent, quiet, NotGranted),
        (
            "on after tccd's last unlisted answer",
            on_at_100,
            tccd(DevToolState::Unlisted, 99_000),
            Granted,
        ),
        (
            "on, but tccd answered unlisted since: another copy's row",
            on_at_100,
            tccd(DevToolState::Unlisted, 100_001),
            NotGranted,
        ),
        (
            "on, tccd allowed since",
            on_at_100,
            tccd(DevToolState::Allowed, 100_001),
            Granted,
        ),
        (
            "off, tccd allowed since (a policy outranks the switch)",
            off_at_100,
            tccd(DevToolState::Allowed, 100_001),
            Granted,
        ),
        (
            "off after tccd's last allowed answer",
            off_at_100,
            tccd(DevToolState::Allowed, 99_000),
            NotGranted,
        ),
        (
            "the same millisecond: the switch",
            off_at_100,
            tccd(DevToolState::Allowed, 100_000),
            NotGranted,
        ),
        (
            "absent though tccd allowed (just removed)",
            absent,
            tccd(DevToolState::Allowed, i64::MAX),
            NotGranted,
        ),
        (
            "unread, tccd unlisted",
            unread,
            tccd(DevToolState::Unlisted, 1),
            NotGranted,
        ),
        (
            "unread, tccd denied",
            unread,
            tccd(DevToolState::Denied, 1),
            NotGranted,
        ),
        (
            "unread, tccd allowed",
            unread,
            tccd(DevToolState::Allowed, 1),
            Granted,
        ),
        ("unread, tccd quiet", unread, quiet, Unknown),
        ("unread, no log", unread, no_log, Unknown),
        (
            "a list in an unknown shape",
            DevToolList::refused(DevToolListLabel::FormatUnrecognized),
            no_log,
            Unknown,
        ),
        (
            "pending",
            DevToolList::pending(),
            DevToolProbe::pending(),
            Unknown,
        ),
        (
            "a row not read, whatever its listing",
            DevToolList {
                listing: DevToolListing::On,
                ..DevToolList::refused(DevToolListLabel::ToolFailed)
            },
            quiet,
            Unknown,
        ),
        (
            "an absent listing not read",
            DevToolList {
                listing: DevToolListing::Absent,
                ..DevToolList::refused(DevToolListLabel::ToolFailed)
            },
            tccd(DevToolState::Allowed, 1),
            Granted,
        ),
        (
            "an answer not observed, whatever its state",
            unread,
            DevToolProbe {
                state: DevToolState::Allowed,
                ..quiet
            },
            Unknown,
        ),
        (
            "absent, though stamped before tccd allowed",
            listed(DevToolListing::Absent, Some(100)),
            tccd(DevToolState::Allowed, 100_001),
            NotGranted,
        ),
    ] {
        assert_eq!(devtool_grant(&list, &log), want, "{name}");
    }
}

#[test]
fn the_bundle_id_is_read_off_the_bundle_and_never_under_a_protected_root() {
    let dir = crate::test_scratch_dir("bundle-id");
    let root = dir.path().join("x.app");
    std::fs::create_dir_all(root.join("Contents")).expect("bundle");
    std::fs::write(
        root.join("Contents").join("Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleIdentifier</key><string>com.example.x</string></dict></plist>\n",
    )
    .expect("plist");
    assert_eq!(bundle_id_of(&root, &[]), Some("com.example.x".to_string()));
    assert_eq!(bundle_id_of(&root, &[dir.path().to_path_buf()]), None);
    assert_eq!(bundle_id_of(&dir.path().join("missing.app"), &[]), None);
}

#[cfg(target_os = "macos")]
#[test]
fn a_bounded_run_keeps_its_outcome_apart() {
    let sh = |script: &str, ceiling_ms: u64| {
        run_bounded_exit(
            "/bin/sh",
            &["-c".as_ref(), script.as_ref()],
            std::time::Duration::from_millis(ceiling_ms),
            TOOL_OUTPUT_MAX_BYTES,
        )
    };
    assert_eq!(
        sh("printf out; printf err >&2; exit 3", 60_000),
        ToolRun::Exited {
            code: Some(3),
            stdout: "out".to_string(),
            stderr: "err".to_string(),
        }
    );
    assert!(matches!(
        sh("kill -KILL $$", 60_000),
        ToolRun::Exited { code: None, .. }
    ));
    assert_eq!(sh("sleep 5", 200), ToolRun::TimedOut);
    assert_eq!(sh("printf '\\377'", 60_000), ToolRun::Unreadable);
    assert_eq!(
        sh(
            &format!("head -c {} /dev/zero", TOOL_OUTPUT_MAX_BYTES + 1),
            60_000
        ),
        ToolRun::Unreadable
    );
    match sh(
        &format!("head -c {TOOL_OUTPUT_MAX_BYTES} /dev/zero"),
        60_000,
    ) {
        ToolRun::Exited {
            code: Some(0),
            stdout,
            ..
        } => assert_eq!(
            u64::try_from(stdout.len()).ok(),
            Some(TOOL_OUTPUT_MAX_BYTES)
        ),
        other => panic!("a report at the cap: {other:?}"),
    }
    let over = TOOL_OUTPUT_MAX_BYTES + 1;
    let script = format!("head -c {over} /dev/zero");
    match run_bounded_exit(
        "/bin/sh",
        &["-c".as_ref(), script.as_ref()],
        std::time::Duration::from_secs(60),
        DEVTOOL_OUTPUT_MAX_BYTES,
    ) {
        ToolRun::Exited {
            code: Some(0),
            stdout,
            ..
        } => assert_eq!(u64::try_from(stdout.len()).ok(), Some(over)),
        other => panic!("read whole under the larger cap: {other:?}"),
    }
    let dir = crate::test_scratch_dir("no-such-tool");
    let missing = dir.path().join("aterm-no-such-tool");
    assert_eq!(
        run_bounded_exit(
            missing.to_str().unwrap(),
            &[],
            std::time::Duration::from_secs(1),
            TOOL_OUTPUT_MAX_BYTES,
        ),
        ToolRun::Absent
    );
    assert_eq!(
        run_bounded(
            "/bin/sh",
            &["-c".as_ref(), "exit 3".as_ref()],
            HANG_DETECTOR
        ),
        Some((false, String::new(), String::new()))
    );

    let gate = fifo_gate(dir.path());
    let _detector = gate_hang_detector(&gate);
    let armed = gate.clone();
    READERS_PENDING.with(|acts| {
        acts.borrow_mut()
            .push_back(Box::new(move |_| release_gate(&armed).unwrap()));
    });
    let run = run_bounded_exit(
        "/bin/sh",
        &[
            "-c".as_ref(),
            GATED_GRANDCHILD.as_ref(),
            "sh".as_ref(),
            gate.as_os_str(),
        ],
        HANG_DETECTOR,
        TOOL_OUTPUT_MAX_BYTES,
    );
    let _ = release_gate(&gate);
    assert_eq!(
        run,
        ToolRun::Exited {
            code: Some(0),
            stdout: "earlylate".to_string(),
            stderr: String::new(),
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_test_binary_is_not_hardened() {
    assert_eq!(self_hardening(), SelfHardening::NotHardened);
}

#[cfg(target_os = "macos")]
#[test]
fn the_membership_query_is_total() {
    let _ = debugger_membership();
    assert_eq!(imp::in_group("aterm-no-such-group-for-tests"), None);
    assert_eq!(
        imp::in_group("_www"),
        Some(false),
        "no account is the web server's"
    );
    // SAFETY: `getgid` has no preconditions.
    if unsafe { libc::getgid() } == 20 {
        assert_eq!(
            imp::in_group("staff"),
            Some(true),
            "the account's own group"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_unregistered_id_is_an_empty_answer_and_a_malformed_one_none() {
    assert_eq!(
        imp::registered_bundles("com.aterm.test.no-such-app"),
        Some(Vec::new())
    );
    assert_eq!(imp::registered_bundles(""), None);
}

#[cfg(target_os = "macos")]
#[test]
fn errno_constants_match_the_platform() {
    assert_eq!(ERRNO_EPERM, libc::EPERM);
    assert_eq!(ERRNO_ESRCH, libc::ESRCH);
}

#[cfg(target_os = "macos")]
#[test]
fn the_real_probes_refuse_from_a_test_binary() {
    let fda = probe_fda(ProbeGate::on());
    assert_eq!(
        (fda.state, fda.label),
        (FdaState::Unknown, ProbeLabel::RefusedOutOfBundle)
    );
    assert_eq!(
        probe_debugger(ProbeGate::on()),
        DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle)
    );
    assert_eq!(
        probe_devtool(ProbeGate::on(), ATERM),
        DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle)
    );
    assert_eq!(
        probe_devtool_list(ProbeGate::on(), ATERM),
        DevToolList::refused(DevToolListLabel::RefusedOutOfBundle)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_real_tccutil_presence_reads_the_mode_of_a_real_file() {
    let got = tccutil_presence();
    assert_eq!(
        got,
        TccutilPresence::Executable,
        "/usr/bin/tccutil is part of macOS; {got:?} means the mode read is wrong"
    );
    assert!(got.can_run());
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("tccutil-mode");
    let plain = scratch.path().join("tccutil");
    std::fs::write(&plain, b"").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let group_x = scratch.path().join("group-x");
    std::fs::write(&group_x, b"").unwrap();
    std::fs::set_permissions(&group_x, std::fs::Permissions::from_mode(0o610)).unwrap();
    for (path, want) in [
        (plain.clone(), TccutilPresence::NotExecutable),
        (group_x, TccutilPresence::Executable),
        (scratch.path().join("gone"), TccutilPresence::Missing),
        (plain.join("x"), TccutilPresence::Unknown),
    ] {
        assert_eq!(tccutil_presence_at(&path), want, "{}", path.display());
    }
    assert_eq!(
        parse_open_probe_line("errno 1\n"),
        Some(ProbeOutcome::Errno(1))
    );
    assert_eq!(parse_open_probe_line("errno +1\n"), None);
}

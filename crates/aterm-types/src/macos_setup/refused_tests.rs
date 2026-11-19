// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::macos_setup::spotlight_scope::Entry;
use std::collections::BTreeMap;
use std::io;

/// A Mac's record files held in memory: each path's bytes and the uid that owns it.
#[derive(Default)]
struct Files(BTreeMap<PathBuf, (u32, Vec<u8>)>);

impl Files {
    fn with(mut self, path: &str, uid: u32, bytes: &str) -> Self {
        self.0
            .insert(PathBuf::from(path), (uid, bytes.as_bytes().to_vec()));
        self
    }
}

impl Reads for Files {
    fn macos(&self) -> bool {
        true
    }

    fn euid(&self) -> u32 {
        501
    }

    fn home(&self) -> Option<PathBuf> {
        None
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        let (uid, _) = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(Entry {
            uid: *uid,
            dir: false,
            symlink: false,
            id: None,
            mode: 0o644,
        })
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_owned())
    }

    fn names(&self, _: &Path) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        match self.0.get(path) {
            Some((0, bytes)) if bytes.len() <= max => Ok(bytes.clone()),
            Some(_) => Err(io::ErrorKind::PermissionDenied.into()),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        Err("not read".into())
    }
}

fn key(key: Option<&str>) -> impl FnOnce() -> Option<String> {
    let key = key.map(str::to_owned);
    move || key
}

/// A record stands for a key root's own file lists, and for every key once it lists [`KEYS`].
/// NEGATIVE CONTROLS: no record, a record listing other keys, a key that is only part of a
/// listed line, a file root does not own, and inputs that could not be read (no key) each
/// stand for nothing, so the step is asked for.
#[test]
fn a_record_stands_only_as_roots_file_listing_this_key() {
    let record = |uid: u32, text: &str| Files::default().with(DEVELOPER_MODE, uid, text);
    let no_boot = || -> Option<String> { panic!("no stalled line: the boot is not read") };
    assert!(stands(
        &record(0, "off\n"),
        DEVELOPER_MODE,
        key(Some("off")),
        no_boot
    ));
    assert!(stands(
        &record(0, "member 502\noff\n"),
        DEVELOPER_MODE,
        key(Some("off")),
        no_boot
    ));
    for (why, files, listed) in [
        ("no record", Files::default(), Some("off")),
        ("another key", record(0, "member 501\n"), Some("off")),
        (
            "part of a line",
            record(0, "member 5011\n"),
            Some("member 501"),
        ),
        ("not root's file", record(501, "off\n"), Some("off")),
        ("no key", record(0, "off\n"), None),
        (
            "another step's record",
            Files::default().with(LAN_BROKER, 0, "off\n"),
            Some("off"),
        ),
    ] {
        assert!(
            !stands(&files, DEVELOPER_MODE, key(listed), no_boot),
            "{why}"
        );
    }
    // No record: the step's inputs are not read at all.
    assert!(!stands(
        &Files::default(),
        LAN_BROKER,
        || unreachable!("a key read with no record"),
        no_boot
    ));

    let keys = |count: usize| -> String { (0..count).map(|n| format!("build {n}\n")).collect() };
    let full = Files::default().with(LAN_BROKER, 0, &keys(KEYS));
    assert!(stands(
        &full,
        LAN_BROKER,
        key(Some("a build never tried")),
        no_boot
    ));
    assert!(stands(&full, LAN_BROKER, key(None), no_boot));
    let short = Files::default().with(LAN_BROKER, 0, &keys(KEYS - 1));
    assert!(!stands(
        &short,
        LAN_BROKER,
        key(Some("a build never tried")),
        no_boot
    ));
    assert!(stands(&short, LAN_BROKER, key(Some("build 0")), no_boot));

    // A pending or stalled line is no key: a reader that matches whole lines, as a build from
    // before them does, finds no `off` in it, and a pending line stands in no boot.
    for line in ["no-verdict 1000 b1 off", "stalled b1 off"] {
        assert!(!BTreeSet::from([PathBuf::from(line)]).contains(Path::new("off")));
    }
    let pending = record(0, "no-verdict 1000 b1 off\n");
    for boot in ["b1", "b2"] {
        assert!(
            !stands(&pending, DEVELOPER_MODE, key(Some("off")), key(Some(boot))),
            "{boot}"
        );
    }
}

/// The record as `noted` leaves `lines` (each line as written), or `None` where it is left.
fn note(
    lines: &[&str],
    key: &str,
    failure: Failure,
    now: u64,
    boot: Option<&str>,
) -> Option<Vec<String>> {
    let lines: BTreeSet<PathBuf> = lines.iter().map(PathBuf::from).collect();
    noted(&lines, key, failure, now, boot).map(|lines| {
        lines
            .into_iter()
            .map(|line| line.to_str().unwrap().to_owned())
            .collect()
    })
}

/// Whether the record of `lines` stands for `key` for a reader in `boot`.
fn stands_in(lines: &[String], key: &str, boot: &str) -> bool {
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    stands(
        &Files::default().with(LAN_BROKER, 0, &text),
        LAN_BROKER,
        || Some(key.to_owned()),
        || Some(boot.to_owned()),
    )
}

/// A no-verdict stands for nothing until another comes [`APART`] later in the same boot, which
/// stalls the step for that boot alone; one that comes after a reboot stands for the key. The
/// time rule, not a count: every no-verdict within [`APART`] of the first leaves the line
/// pending. NEGATIVE CONTROLS (run): an `APART` guard removed stalls at the second no-verdict;
/// promotion removed leaves the line pending at [`APART`]; a no-verdict routed to a definite
/// failure stands at once; `stands` that ignores the boot stands for another boot.
#[test]
fn a_no_verdict_stalls_only_apart_in_one_boot() {
    const K: &str = "k";
    let first = note(&[], K, Failure::NoVerdict, 1000, Some("b1")).unwrap();
    assert_eq!(first, ["no-verdict 1000 b1 k"]);
    assert!(!stands_in(&first, K, "b1"));
    let lines: Vec<&str> = first.iter().map(String::as_str).collect();

    // Inside APART: the line stays as it is, its first second kept.
    assert_eq!(note(&lines, K, Failure::NoVerdict, 1299, Some("b1")), None);
    assert_eq!(note(&lines, K, Failure::NoVerdict, 1000, Some("b1")), None);

    // APART after the first: stalled for this boot.
    let stalled = note(&lines, K, Failure::NoVerdict, 1300, Some("b1")).unwrap();
    assert_eq!(stalled, ["stalled b1 k"]);
    assert!(stands_in(&stalled, K, "b1"));
    assert!(!stands_in(&stalled, K, "b2"), "another boot asks again");
    assert!(
        !stands_in(&stalled, "other", "b1"),
        "another key asks again"
    );
    let lines: Vec<&str> = stalled.iter().map(String::as_str).collect();
    assert_eq!(note(&lines, K, Failure::NoVerdict, 9999, Some("b1")), None);

    // After a reboot: a pending or a stalled line of the earlier boot makes it the key's.
    for earlier in ["no-verdict 1000 b1 k", "stalled b1 k"] {
        let after = note(&[earlier], K, Failure::NoVerdict, 1001, Some("b2")).unwrap();
        assert_eq!(after, [K], "{earlier}");
        assert!(stands_in(&after, K, "b2"));
        assert!(stands_in(&after, K, "b3"));
    }

    // A definite failure stands at once, over a pending line too.
    assert_eq!(
        note(&[], K, Failure::Definite, 1000, Some("b1")).unwrap(),
        [K]
    );
    assert_eq!(
        note(
            &["no-verdict 1000 b1 k"],
            K,
            Failure::Definite,
            1001,
            Some("b1")
        )
        .unwrap(),
        [K]
    );
    assert_eq!(note(&[], K, Failure::Definite, 1000, None).unwrap(), [K]);
    assert_eq!(note(&[K], K, Failure::NoVerdict, 1000, Some("b1")), None);
    assert_eq!(note(&[K], K, Failure::Definite, 1000, Some("b1")), None);

    // Without the boot a no-verdict lists nothing, as before any record.
    assert_eq!(note(&[], K, Failure::NoVerdict, 1000, None), None);
}

/// A clock set back before a pending line dates the line again and stalls nothing; the next
/// no-verdict [`APART`] after the new date stalls it. NEGATIVE CONTROL (run): without the
/// `min`, the line keeps its later date and the second no-verdict after it never stalls.
#[test]
fn a_clock_set_back_dates_the_line_again_and_stalls_nothing() {
    let back = note(
        &["no-verdict 5000 b1 k"],
        "k",
        Failure::NoVerdict,
        1000,
        Some("b1"),
    )
    .unwrap();
    assert_eq!(back, ["no-verdict 1000 b1 k"]);
    assert_eq!(
        note(
            &["no-verdict 1000 b1 k"],
            "k",
            Failure::NoVerdict,
            1300,
            Some("b1")
        )
        .unwrap(),
        ["stalled b1 k"]
    );
}

/// Each key keeps its own pending line: two builds that give no verdict in turn each stall on
/// their own time. NEGATIVE CONTROL (run): a record that keeps one pending line in all loses
/// the first build's line to the second's, so neither ever stalls.
#[test]
fn alternating_keys_each_stall_on_their_own() {
    let mut lines: Vec<String> = Vec::new();
    let mut step = |key: &str, now: u64| {
        let current: Vec<&str> = lines.iter().map(String::as_str).collect();
        if let Some(next) = note(&current, key, Failure::NoVerdict, now, Some("b1")) {
            lines = next;
        }
        lines.clone()
    };
    step("k1", 1000);
    step("k2", 1100);
    step("k1", 1200);
    let after = step("k2", 1300);
    assert!(
        after.contains(&"no-verdict 1000 b1 k1".to_owned()),
        "{after:?}"
    );
    assert!(
        after.contains(&"no-verdict 1100 b1 k2".to_owned()),
        "{after:?}"
    );
    let after = step("k1", 1300);
    assert!(after.contains(&"stalled b1 k1".to_owned()), "{after:?}");
    let after = step("k2", 1400);
    assert!(after.contains(&"stalled b1 k2".to_owned()), "{after:?}");
    assert_eq!(after.len(), 2, "{after:?}");
}

/// A pending line never brings a record to [`KEYS`] while another line that is not a plain key
/// can give it room: the oldest pending line of another key goes first. Beside [`KEYS`] - 1
/// plain keys it is the last line, and the record stands for no key it does not list; only
/// [`KEYS`] plain keys make it stand for every key. A stalled line of another boot is dropped.
/// NEGATIVE CONTROL (run): without the eviction a pending line fills the record.
#[test]
fn a_pending_line_never_fills_the_record() {
    let mut seven: Vec<String> = (0..5).map(|n| format!("build {n}")).collect();
    seven.push("no-verdict 900 b1 old".to_owned());
    seven.push("no-verdict 950 b1 newer".to_owned());
    let lines: Vec<&str> = seven.iter().map(String::as_str).collect();
    assert_eq!(lines.len(), KEYS - 1);
    let after = note(&lines, "k", Failure::NoVerdict, 1000, Some("b1")).unwrap();
    assert_eq!(after.len(), KEYS - 1, "{after:?}");
    assert!(
        !after.contains(&"no-verdict 900 b1 old".to_owned()),
        "{after:?}"
    );
    assert!(
        after.contains(&"no-verdict 950 b1 newer".to_owned()),
        "{after:?}"
    );
    assert!(
        after.contains(&"no-verdict 1000 b1 k".to_owned()),
        "{after:?}"
    );
    assert!(!stands_in(&after, "a build never tried", "b1"));

    let keys: Vec<String> = (0..KEYS - 1).map(|n| format!("build {n}")).collect();
    let lines: Vec<&str> = keys.iter().map(String::as_str).collect();
    let last = note(&lines, "k", Failure::NoVerdict, 1000, Some("b1")).unwrap();
    assert_eq!(last.len(), KEYS, "{last:?}");
    assert!(
        last.contains(&"no-verdict 1000 b1 k".to_owned()),
        "{last:?}"
    );
    assert!(!stands_in(&last, "a build never tried", "b1"));
    let last: Vec<&str> = last.iter().map(String::as_str).collect();
    let next = note(&last, "k2", Failure::NoVerdict, 1100, Some("b1")).unwrap();
    assert!(
        next.contains(&"no-verdict 1100 b1 k2".to_owned()) && next.len() == KEYS,
        "the earlier pending line gave its room: {next:?}"
    );
    let full = note(&lines, "k", Failure::Definite, 1000, Some("b1")).unwrap();
    assert_eq!(full.len(), KEYS);
    assert!(stands_in(&full, "a build never tried", "b1"));
    let full: Vec<&str> = full.iter().map(String::as_str).collect();
    assert_eq!(
        note(&full, "other", Failure::Definite, 1000, Some("b1")),
        None
    );
    assert_eq!(
        note(&full, "other", Failure::NoVerdict, 1000, Some("b1")),
        None
    );

    let dropped = note(
        &["stalled b0 other", "stalled b1 mine", "off"],
        "k",
        Failure::Definite,
        1000,
        Some("b1"),
    )
    .unwrap();
    assert_eq!(dropped, ["k", "off", "stalled b1 mine"]);
    // Without the boot no stalled line can be judged another boot's: all are kept.
    let kept = note(&["stalled b0 other"], "k", Failure::Definite, 1000, None).unwrap();
    assert_eq!(kept, ["k", "stalled b0 other"]);
}

/// Superseded builds' stalled lines of this boot never keep a later build's no-verdict out of
/// a record: they give it room as pending lines do, so the later build stalls on its own time,
/// whatever mix of plain keys and stalled lines came before it. NEGATIVE CONTROL (run): an
/// eviction that takes pending lines alone lists nothing for the later build, which is then
/// asked for with no end in that boot.
#[test]
fn superseded_builds_stalled_lines_never_keep_a_later_build_out() {
    let stalled: Vec<String> = (0..KEYS - 1).map(|n| format!("stalled b1 s{n}")).collect();
    let mut mixed: Vec<String> = (0..3).map(|n| format!("build {n}")).collect();
    mixed.extend((0..4).map(|n| format!("stalled b1 s{n}")));
    for before in [stalled, mixed] {
        let lines: Vec<&str> = before.iter().map(String::as_str).collect();
        let first = note(&lines, "k", Failure::NoVerdict, 1000, Some("b1"))
            .unwrap_or_else(|| panic!("nothing listed beside {before:?}"));
        assert!(
            first.contains(&"no-verdict 1000 b1 k".to_owned()),
            "{first:?}"
        );
        assert_eq!(first.len(), KEYS - 1, "{first:?}");
        let first: Vec<&str> = first.iter().map(String::as_str).collect();
        let stalled = note(&first, "k", Failure::NoVerdict, 1300, Some("b1")).unwrap();
        assert!(stands_in(&stalled, "k", "b1"), "{stalled:?}");
        assert!(!stands_in(&stalled, "a build never tried", "b1"));
    }
}

/// A plain key brings a record to [`KEYS`] lines only beside [`KEYS`] - 1 other plain keys: a
/// definite failure on top of pending and stalled lines takes the room of the oldest of them,
/// so the record never stands for every key, to this build or to one before it, for a
/// passing state. NEGATIVE CONTROL (run): a definite failure that skips the eviction brings the
/// record to [`KEYS`] lines.
#[test]
fn only_plain_keys_fill_the_record() {
    let mut passing: Vec<String> = (0..4)
        .map(|n| format!("no-verdict {} b1 p{n}", 900 + n))
        .collect();
    passing.extend((0..3).map(|n| format!("stalled b1 s{n}")));
    let lines: Vec<&str> = passing.iter().map(String::as_str).collect();
    let after = note(&lines, "k", Failure::Definite, 1000, Some("b1")).unwrap();
    assert_eq!(after.len(), KEYS - 1, "{after:?}");
    assert!(after.contains(&"k".to_owned()), "{after:?}");
    assert!(
        !after.contains(&"no-verdict 900 b1 p0".to_owned()),
        "the oldest pending line gave its room: {after:?}"
    );
    assert!(stands_in(&after, "k", "b2"));
    assert!(!stands_in(&after, "a build never tried", "b1"));
}

/// A full record is judged after a reboot's dead stalled lines are dropped: once the boot they
/// stalled in is over they take no room, and the next no-verdict is listed. NEGATIVE CONTROL
/// (run): judging the record full before it is pruned lists nothing, after every reboot.
#[test]
fn a_reboot_frees_the_room_of_its_stalled_lines() {
    let mut full: Vec<String> = (0..4).map(|n| format!("build {n}")).collect();
    full.extend((0..4).map(|n| format!("stalled b0 s{n}")));
    let lines: Vec<&str> = full.iter().map(String::as_str).collect();
    assert_eq!(lines.len(), KEYS);
    let after = note(&lines, "k", Failure::NoVerdict, 1000, Some("b1")).unwrap();
    assert_eq!(
        after,
        [
            "build 0",
            "build 1",
            "build 2",
            "build 3",
            "no-verdict 1000 b1 k"
        ]
    );
}

/// A record an older writer brought to [`KEYS`] lines, its plain key beside pending and stalled
/// lines, stands for the keys it lists alone. NEGATIVE CONTROL (run): counting every line
/// toward [`KEYS`] makes it stand for every key, a build never tried among them.
#[test]
fn a_record_of_mixed_lines_stands_only_for_what_it_lists() {
    let mut mixed: Vec<String> = (0..KEYS - 1)
        .map(|n| format!("no-verdict {n} b1 p{n}"))
        .collect();
    mixed.push("an older build".to_owned());
    assert_eq!(mixed.len(), KEYS);
    assert!(stands_in(&mixed, "an older build", "b1"));
    assert!(!stands_in(&mixed, "a build never tried", "b1"));
}

/// Keys and boots a line cannot hold are refused, and lines of another form are kept as they
/// stand and match nothing.
#[test]
fn a_line_holds_only_what_it_can_say() {
    for bad in ["", "a\nb", "no-verdict 1 b k", "stalled b k"] {
        assert_eq!(
            note(&[], bad, Failure::Definite, 1, Some("b1")),
            None,
            "{bad:?}"
        );
    }
    for boot in ["", "b 1", "b\n"] {
        assert_eq!(
            note(&[], "k", Failure::NoVerdict, 1, Some(boot)),
            None,
            "{boot:?}"
        );
    }
    let odd = note(
        &["no-verdict x b1 k", "stalled b1"],
        "k",
        Failure::NoVerdict,
        1,
        Some("b1"),
    )
    .unwrap();
    assert_eq!(
        odd,
        ["no-verdict 1 b1 k", "no-verdict x b1 k", "stalled b1"]
    );
    assert!(!stands_in(&["no-verdict x b1 k".to_owned()], "k", "b1"));
}

/// A broker that serves takes out every line its own key names and keeps the rest.
/// NEGATIVE CONTROL: a key no line names leaves the record.
#[test]
fn a_serving_broker_takes_out_its_own_key_alone() {
    let lines: BTreeSet<PathBuf> = [
        "k",
        "no-verdict 1 b1 k",
        "stalled b1 k",
        "k2",
        "stalled b1 k2",
        "kk",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    let kept = without_key(&lines, "k").unwrap();
    let kept: Vec<&str> = kept.iter().map(|line| line.to_str().unwrap()).collect();
    assert_eq!(kept, ["k2", "kk", "stalled b1 k2"]);
    assert_eq!(without_key(&lines, "k3"), None);
}

/// A boot reads in one form whichever way it was read: root's `sysctl` (the UUID) and the
/// window's (32 hex digits). NEGATIVE CONTROLS: other lengths, misplaced dashes and non-hex digits
/// are none.
#[test]
fn a_boot_reads_in_one_form() {
    let word = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        boot_word("01234567-89AB-CDEF-0123-456789ABCDEF\n").as_deref(),
        Some(word)
    );
    assert_eq!(boot_word(word).as_deref(), Some(word));
    for bad in [
        "",
        "0123456789abcdef0123456789abcde",
        "0123456-789AB-CDEF-0123-456789ABCDEF",
        "01234567-89AB-CDEF-0123-456789ABCDEG",
        "0123456789abcdef0123456789abcdeg",
    ] {
        assert_eq!(boot_word(bad), None, "{bad:?}");
    }
}

/// Root's reading of this boot (`sysctl -n kern.bootsessionuuid`, which the root half runs) and
/// the window's ([`boot_here`]) name the same boot, so a stalled line root writes stands for the
/// window's reader.
#[cfg(target_os = "macos")]
#[test]
fn root_and_the_window_read_this_boot_alike() {
    let mut sysctl = aterm_uds::spawnfd::command("/usr/sbin/sysctl");
    sysctl.args(["-n", "kern.bootsessionuuid"]);
    let out = aterm_uds::deadline::output_while_process_progressing(
        &mut sysctl,
        aterm_uds::deadline::ECHO_HANG,
        "sysctl -n kern.bootsessionuuid",
    )
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    let root = boot_word(std::str::from_utf8(&out.stdout).unwrap());
    assert!(root.is_some(), "{out:?}");
    assert_eq!(root, boot_here());
}

/// The wait that stalls a step is the broker adoption's longest assumed stall.
#[cfg(unix)]
#[test]
fn apart_is_the_adoptions_stall() {
    assert_eq!(APART, aterm_uds::lan::adoption::STALL);
}

/// Developer mode's key names the reading that failed: `off` for any account, or the account
/// whose authorization is missing.
#[test]
fn the_developer_mode_key_names_the_reading() {
    assert_eq!(developer_mode_key(false, 501), "off");
    assert_eq!(developer_mode_key(false, 502), "off");
    assert_eq!(developer_mode_key(true, 501), "member 501");
    assert_ne!(developer_mode_key(true, 501), developer_mode_key(true, 502));
}

/// The broker's key names the build and the bundle it was installed from.
/// NEGATIVE CONTROLS: one changed code byte at the same length, one more byte, another CPU
/// type, the same bytes split over two directories, and another bundle path each give another
/// key; a path with a line break gives none.
#[cfg(unix)]
#[test]
fn the_broker_key_names_the_build_and_the_bundle() {
    use aterm_uds::lan::install::{CodeDirectories, test_image};
    let code = |cpu: u32, directories: &[&[u8]]| {
        CodeDirectories::of_bytes(&test_image(cpu, directories)).unwrap()
    };
    let arm = 0x0100_000c;
    let app = Path::new("/Applications/aterm.app");
    let key = broker_key(app, &code(arm, &[b"this build"])).unwrap();
    assert_eq!(
        broker_key(app, &code(arm, &[b"this build"])),
        Some(key.clone())
    );
    assert!(key.ends_with(" /Applications/aterm.app"), "{key}");
    assert_eq!(key.len(), 32 + " /Applications/aterm.app".len(), "{key}");
    for (why, other) in [
        (
            "one changed byte",
            broker_key(app, &code(arm, &[b"that build"])),
        ),
        (
            "one more byte",
            broker_key(app, &code(arm, &[b"this build."])),
        ),
        (
            "another CPU",
            broker_key(app, &code(0x0100_0007, &[b"this build"])),
        ),
        (
            "two directories",
            broker_key(app, &code(arm, &[b"this ", b"build"])),
        ),
        (
            "another bundle",
            broker_key(
                Path::new("/Users//a/Applications/aterm.app"),
                &code(arm, &[b"this build"]),
            ),
        ),
    ] {
        assert!(other.is_some_and(|other| other != key), "{why}");
    }
    assert_eq!(
        broker_key(
            Path::new("/Users//a\n/aterm.app"),
            &code(arm, &[b"this build"])
        ),
        None
    );
}

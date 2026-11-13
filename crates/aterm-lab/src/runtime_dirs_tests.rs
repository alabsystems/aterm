// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

const ROOT: &str = "/private/tmp/tbh-501-rt/muse";
const UID: u32 = 501;
const UP: i64 = 1_000;

/// A filesystem of entries and contents.
#[derive(Default)]
struct Fake {
    at: BTreeMap<String, (Stat, String)>,
    dirs: BTreeSet<String>,
    stat_errors: BTreeSet<String>,
    dir_errors: BTreeSet<String>,
    entry_errors: BTreeSet<String>,
}

impl Fake {
    fn put(&mut self, path: &str, ino: u64, kind: Kind, text: &str) {
        let st = Stat {
            dev: 1,
            ino,
            kind,
            uid: UID,
            born: UP + 5,
        };
        self.at.insert(path.to_string(), (st, text.to_string()));
    }
}

impl View for Fake {
    fn stat(&self, path: &str) -> Option<Stat> {
        self.at.get(path).map(|(s, _)| *s)
    }
    fn read(&self, path: &str) -> Option<String> {
        self.at.get(path).map(|(_, t)| t.clone())
    }
    fn inspect(&self, path: &str) -> std::io::Result<Option<Stat>> {
        if self.stat_errors.contains(path) {
            Err(std::io::Error::other("injected unavailable observation"))
        } else {
            Ok(self.stat(path))
        }
    }
    fn children(&self, path: &str) -> std::io::Result<Option<Scan<Vec<String>>>> {
        if self.dir_errors.contains(path) {
            return Err(std::io::Error::other("injected unavailable observation"));
        }
        if !self.dirs.contains(path) {
            return Ok(None);
        }
        let prefix = format!("{path}/");
        Ok(Some(Scan {
            entries: self
                .at
                .keys()
                .filter(|p| {
                    p.strip_prefix(prefix.as_str())
                        .is_some_and(|rest| !rest.contains('/'))
                })
                .cloned()
                .collect(),
            complete: !self.entry_errors.contains(path),
        }))
    }
}

fn held(pid: i32, ino: u64, rel: &str) -> Held {
    Held {
        pid,
        dev: 1,
        ino,
        path: format!("{ROOT}/{rel}"),
    }
}

/// One Muse Code's three entries, as 1.4.10 writes them.
fn muse(fs: &mut Fake, pid: i32, hex: &str, id: &str, base: u64) -> Vec<Held> {
    fs.put(
        &format!("{ROOT}/ms-{hex}.sock.lease"),
        base,
        Kind::File,
        &format!(
            "{{\"schema_version\":1,\"endpoint_hint\":\"ms-{hex}.sock\",\"process_generation_hint\":\"pid={pid}\",\"pid\":{pid}}}"
        ),
    );
    fs.put(&format!("{ROOT}/ms-{hex}.sock"), base + 1, Kind::Socket, "");
    fs.put(
        &format!("{ROOT}/sessions/{id}.json"),
        base + 2,
        Kind::File,
        &format!(
            "{{\"schema_version\":1,\"session_id\":\"{id}\",\"endpoint_hint\":\"ms-{hex}.sock\",\"process_generation_hint\":\"pid={pid}\"}}"
        ),
    );
    vec![
        held(pid, base, &format!("ms-{hex}.sock.lease")),
        held(pid, base + 1, &format!("ms-{hex}.sock")),
        held(pid, base + 2, &format!("sessions/{id}.json")),
    ]
}

fn paths(v: &[Held]) -> Vec<String> {
    v.iter()
        .map(|h| h.path.trim_start_matches(ROOT).to_string())
        .collect()
}

/// The witness's record keeps a line per entry, and only its own shape.
#[test]
fn a_record_line_reads_back_and_the_witness_keeps_what_lies_below_the_root() {
    let h = held(4242, 7, "ms-00ff.sock.lease");
    assert_eq!(Held::parse(h.line().trim_end()), Some(h));
    assert_eq!(
        Held::parse("1\t1\t1\t/x"),
        None,
        "launchd is never a holder"
    );
    assert_eq!(Held::parse("7\t1\t1\trelative"), None);
    let seen = [
        (4242, ROOT.to_string()),
        (4242, format!("{ROOT}/ms-00ff.sock.lease")),
        (4242, format!("{ROOT}/sessions")),
        (17, format!("{ROOT}-other/x")),
        (17, format!("{ROOT}/ms-00ff.sock")),
        (1, format!("{ROOT}/ms-00ff.sock")),
    ];
    assert_eq!(
        held_below(&seen, ROOT),
        [
            (4242, format!("{ROOT}/ms-00ff.sock.lease")),
            (4242, format!("{ROOT}/sessions")),
            (17, format!("{ROOT}/ms-00ff.sock")),
        ]
    );
}

/// What a run's processes hold below a root, read in process ([`held_by`]): a child's
/// open file and the listener this process bound there. NEGATIVE CONTROLS: a file below
/// the root that only a process outside the run holds, one nobody holds, and the listener
/// once its binder is not of the run, stay out.
#[test]
fn the_witness_reads_a_members_open_file_and_bound_listener() {
    let root = crate::TestScratch::new("rt-held");
    std::fs::create_dir_all(root.join("sessions")).unwrap();
    let (session, theirs, idle) = (
        root.join("sessions/01a1.json"),
        root.join("ms-bb22.sock.lease"),
        root.join("ms-cc33.sock.lease"),
    );
    for f in [&session, &theirs, &idle] {
        std::fs::write(f, b"{}").unwrap();
    }
    let socket = root.join("ms-aa11.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let sleeper = |file: &Path| {
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(std::fs::File::open(file).unwrap())
            .spawn()
            .expect("spawn a holder")
    };
    let mut member = sleeper(&session);
    let mut outsider = sleeper(&theirs);
    let me = i32::try_from(std::process::id()).unwrap();
    let pid = i32::try_from(member.id()).unwrap();
    let at = root.to_string_lossy().into_owned();
    let mut got = held_below(&held_by(&[pid, me], &at), &at);
    let without_me = held_below(&held_by(&[pid], &at), &at);
    for child in [&mut member, &mut outsider] {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(listener);
    got.sort();
    let session = (pid, session.to_string_lossy().into_owned());
    let mut want = vec![session.clone(), (me, socket.to_string_lossy().into_owned())];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(without_me, [session]);
}

/// THE LAB TAKES BACK ITS OWN MUSE CODE'S LEASE, SOCKET AND SESSION
/// RECORD, AND NOTHING ELSE. NEGATIVE CONTROLS: another process's entries
/// that a process of the jail held (a scan of the registry) and that name
/// that other process, the registry's shared lock and its directories, an
/// entry made before the lab came up, an entry another file replaced since
/// the witness saw it, a link in an entry's place, another user's file,
/// and a socket whose lease is not this run's all stay.
#[test]
fn the_lab_takes_back_its_own_muse_code_entries_and_nothing_else() {
    let mut fs = Fake::default();
    let mut rec = muse(&mut fs, 4242, "aa11", "01a1-mine", 10);
    let theirs = muse(&mut fs, 777, "bb22", "01a1-theirs", 20);
    rec.extend(theirs.iter().map(|h| Held {
        pid: 4242,
        ..h.clone()
    }));
    fs.put(
        &format!("{ROOT}/.session-registry.mutation.lock"),
        30,
        Kind::File,
        "",
    );
    rec.push(held(4242, 30, ".session-registry.mutation.lock"));
    fs.put(&format!("{ROOT}/sessions"), 31, Kind::Other, "");
    rec.push(held(4242, 31, "sessions"));
    fs.put(
        &format!("{ROOT}/ms-cc33.sock.lease"),
        40,
        Kind::File,
        "{\"pid\":4243}",
    );
    fs.at
        .get_mut(&format!("{ROOT}/ms-cc33.sock.lease"))
        .unwrap()
        .0
        .born = UP - 60;
    rec.push(held(4243, 40, "ms-cc33.sock.lease"));
    rec.extend(muse(&mut fs, 5151, "dd44", "01a1-second", 50));
    fs.at
        .get_mut(&format!("{ROOT}/ms-dd44.sock.lease"))
        .unwrap()
        .0
        .ino = 99;
    rec.extend(muse(&mut fs, 6161, "ee55", "01a1-third", 60));
    fs.at
        .get_mut(&format!("{ROOT}/sessions/01a1-third.json"))
        .unwrap()
        .0
        .uid = 0;
    rec.extend(muse(&mut fs, 7171, "ff66", "01a1-fourth", 70));
    fs.at
        .get_mut(&format!("{ROOT}/ms-ff66.sock.lease"))
        .unwrap()
        .0
        .kind = Kind::Other;
    rec.extend(muse(&mut fs, 8181, "a1a1", "01a1-fifth", 80));
    rec.extend(muse(&mut fs, 9191, "b2b2", "01a1-sixth", 90));
    rec.extend(muse(&mut fs, 3131, "c3c3", "01a1-seventh", 100));
    rec.extend(muse(&mut fs, 2121, "d4d4", "01a1-eighth", 110));
    for (hex, edit) in [
        ("a1a1", (|s: &mut Stat| s.ino = 199) as fn(&mut Stat)),
        ("b2b2", |s: &mut Stat| s.uid = 0),
        ("c3c3", |s: &mut Stat| s.kind = Kind::File),
        ("d4d4", |s: &mut Stat| s.born = UP - 60),
    ] {
        edit(&mut fs.at.get_mut(&format!("{ROOT}/ms-{hex}.sock")).unwrap().0);
    }
    let got = claims(&rec, &[ROOT.to_string()], UID, UP, &fs);
    assert_eq!(
        paths(&got),
        [
            "/ms-a1a1.sock.lease",
            "/ms-aa11.sock",
            "/ms-aa11.sock.lease",
            "/ms-b2b2.sock.lease",
            "/ms-c3c3.sock.lease",
            "/ms-d4d4.sock.lease",
            "/ms-ee55.sock",
            "/ms-ee55.sock.lease",
            "/sessions/01a1-eighth.json",
            "/sessions/01a1-fifth.json",
            "/sessions/01a1-fourth.json",
            "/sessions/01a1-mine.json",
            "/sessions/01a1-second.json",
            "/sessions/01a1-seventh.json",
            "/sessions/01a1-sixth.json",
        ]
    );
    assert!(
        got.iter()
            .all(|h| [4242, 5151, 6161, 7171, 8181, 9191, 3131, 2121].contains(&h.pid))
    );
    assert!(claims(&[], &[ROOT.to_string()], UID, UP, &fs).is_empty());
    let outside = Held {
        path: "/private/tmp/other/ms-aa11.sock.lease".into(),
        ..rec[0].clone()
    };
    fs.at.insert(
        outside.path.clone(),
        fs.at[&format!("{ROOT}/ms-aa11.sock.lease")].clone(),
    );
    assert!(claims(&[outside], &[ROOT.to_string()], UID, UP, &fs).is_empty());
}

fn ent(rel: &str, ino: u64) -> Entry {
    Entry {
        path: if rel.is_empty() {
            ROOT.to_string()
        } else {
            format!("{ROOT}/{rel}")
        },
        dev: 1,
        ino,
    }
}

fn rels(v: &[String]) -> Vec<&str> {
    v.iter()
        .map(|p| p.trim_start_matches(ROOT).trim_start_matches('/'))
        .collect()
}

/// A LISTING BEFORE UP AND AFTER DOWN SHOWS WHAT IS LEFT, whatever the
/// witness saw: an entry new since up is left unless it names a live Muse
/// Code outside the jail (a lease's `pid`, a record's
/// `process_generation_hint`, the socket of such a lease) or is the
/// registry's shared structure a first run makes. NEGATIVE CONTROLS: an
/// entry of no known shape, a socket whose lease is gone, entries naming
/// a dead pid, a `sessions` that is no directory and a lock that is no
/// file are left; an entry gone since the listing, an entry outside the
/// root and one unchanged since up are not counted.
#[test]
fn a_listing_before_up_and_after_down_shows_what_is_left() {
    let mut fs = Fake::default();
    let mut before = BTreeSet::new();
    fs.put(ROOT, 1, Kind::Other, "");
    fs.put(&format!("{ROOT}/sessions"), 2, Kind::Other, "");
    fs.put(&format!("{ROOT}/{LOCK}"), 3, Kind::File, "");
    for (rel, ino) in [("", 1), ("sessions", 2), (LOCK, 3)] {
        before.insert(ent(rel, ino));
    }
    let theirs = muse(&mut fs, 777, "bb22", "theirs", 20);
    before.extend(theirs.iter().map(|h| Entry {
        path: h.path.clone(),
        dev: h.dev,
        ino: h.ino,
    }));
    let mut after = before.clone();
    for h in muse(&mut fs, 4242, "aa11", "mine", 10) {
        after.insert(Entry {
            path: h.path,
            dev: h.dev,
            ino: h.ino,
        });
    }
    fs.put(&format!("{ROOT}/ms-aa11.sock.tmp"), 40, Kind::File, "");
    after.insert(ent("ms-aa11.sock.tmp", 40));
    for h in muse(&mut fs, 778, "cc33", "other", 30) {
        after.insert(Entry {
            path: h.path,
            dev: h.dev,
            ino: h.ino,
        });
    }
    fs.at
        .get_mut(&format!("{ROOT}/sessions/theirs.json"))
        .unwrap()
        .0
        .ino = 23;
    after.remove(&ent("sessions/theirs.json", 22));
    after.insert(ent("sessions/theirs.json", 23));
    fs.put(&format!("{ROOT}/ms-ee55.sock"), 50, Kind::Socket, "");
    after.insert(ent("ms-ee55.sock", 50));
    after.insert(ent("ms-ff66.sock.lease", 60));
    let outside = "/private/tmp/tbh-501-rt/muse-other/ms-aa11.sock.lease";
    fs.put(outside, 70, Kind::File, "{\"pid\":4242}");
    after.insert(Entry {
        path: outside.into(),
        dev: 1,
        ino: 70,
    });
    let alive = |p: i32| p == 777 || p == 778;
    let got = new_since(&before, &after, &[ROOT.to_string()], &fs, &alive);
    assert_eq!(
        rels(&got.left),
        [
            "ms-aa11.sock",
            "ms-aa11.sock.lease",
            "ms-aa11.sock.tmp",
            "ms-ee55.sock",
            "sessions/mine.json",
        ]
    );
    assert_eq!(
        rels(&got.theirs),
        [
            "ms-cc33.sock",
            "ms-cc33.sock.lease",
            "sessions/other.json",
            "sessions/theirs.json",
        ]
    );
    assert!(got.shared.is_empty());
    assert_eq!(
        new_since(&before, &before, &[ROOT.to_string()], &fs, &alive),
        Since::default()
    );
    let none = BTreeSet::new();
    let first: BTreeSet<Entry> = [ent("", 1), ent("sessions", 2), ent(LOCK, 3)].into();
    let got = new_since(&none, &first, &[ROOT.to_string()], &fs, &alive);
    assert_eq!(rels(&got.shared), ["", LOCK, "sessions"]);
    assert!(got.left.is_empty() && got.theirs.is_empty());
    fs.at.get_mut(&format!("{ROOT}/sessions")).unwrap().0.kind = Kind::File;
    fs.at.get_mut(&format!("{ROOT}/{LOCK}")).unwrap().0.kind = Kind::Other;
    let got = new_since(&none, &first, &[ROOT.to_string()], &fs, &alive);
    assert_eq!(rels(&got.shared), [""]);
    assert_eq!(rels(&got.left), [LOCK, "sessions"]);
}

/// Only the three shapes Muse Code makes per process are ever taken: a
/// name with a path too many, a dot file or a non-hex endpoint is not.
#[test]
fn only_a_lease_its_socket_and_a_session_record_have_a_shape() {
    assert_eq!(shape("ms-0a1b.sock.lease"), Some(Shape::Lease));
    assert_eq!(shape("ms-0a1b.sock"), Some(Shape::Socket));
    assert_eq!(
        shape("sessions/01a14930-f573-7ce1.json"),
        Some(Shape::Session)
    );
    for not in [
        "",
        "sessions",
        "sessions/",
        "sessions/.json",
        "sessions/a/b.json",
        "sessions/../x.json",
        ".session-registry.mutation.lock",
        "ms-.sock",
        "ms-xyz.sock",
        "ms-0a1b.sock.lease/x",
        "sub/ms-0a1b.sock",
    ] {
        assert_eq!(shape(not), None, "{not}");
    }
}

/// Actual traversal/recheck/post-down verdict: clean and absent are known;
/// stat, directory and claimed-file errors cannot certify an absence.
#[test]
fn a_runtime_absence_needs_every_listing_and_recheck() {
    use crate::codex_proof::Verdict;
    let roots = [ROOT.to_string()];
    let mut fs = Fake::default();
    let absent = listing_with(&roots, &fs);
    assert!(absent.complete && absent.entries.is_empty());
    fs.put(ROOT, 1, Kind::Other, "");
    fs.dirs.insert(ROOT.to_string());
    let clean = listing_with(&roots, &fs);
    let made: Vec<String> = ["ms-aa.sock", "ms-aa.sock.lease", "sessions/mine.json"]
        .iter()
        .map(|p| format!("{ROOT}/{p}"))
        .collect();
    let judge = |before: &Scan<BTreeSet<Entry>>,
                 after: &Scan<BTreeSet<Entry>>,
                 held: &[Held],
                 fs: &Fake| {
        let got = after_down(before, after, held, &roots, fs, &|_| false);
        crate::muse_proof::runtime_verdict(&made, &got.left, got.complete)
    };
    assert_eq!(judge(&absent, &clean, &[], &fs), Verdict::Ok);
    let h = held(7, 2, "ms-aa.sock");
    fs.put(&h.path, h.ino, Kind::Socket, "");
    let dirty = listing_with(&roots, &fs);
    assert_eq!(
        judge(&clean, &dirty, std::slice::from_ref(&h), &fs),
        Verdict::Fail
    );
    let missing = held(7, 3, "ms-bb.sock");
    assert!(standing_with(std::slice::from_ref(&missing), &fs).complete);
    let mut unexpected = Vec::new();
    fs.stat_errors.insert(ROOT.to_string());
    let unknown = listing_with(&roots, &fs);
    if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
        unexpected.push("root stat");
    }
    if judge(&unknown, &clean, &[], &fs) != Verdict::Unproven {
        unexpected.push("before snapshot");
    }
    fs.stat_errors.clear();
    fs.dir_errors.insert(ROOT.to_string());
    let unknown = listing_with(&roots, &fs);
    if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
        unexpected.push("directory entries");
    }
    fs.dir_errors.clear();
    fs.stat_errors.insert(h.path.clone());
    if judge(&clean, &dirty, &[], &fs) != Verdict::Unproven {
        unexpected.push("entry recheck");
    }
    if judge(&clean, &clean, std::slice::from_ref(&h), &fs) != Verdict::Unproven {
        unexpected.push("claimed file");
    }
    fs.stat_errors.clear();
    let saved = fs.at.remove(&h.path).unwrap();
    fs.entry_errors.insert(ROOT.to_string());
    let unknown = listing_with(&roots, &fs);
    if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
        unexpected.push("directory entry");
    }
    fs.at.insert(h.path.clone(), saved);
    let partial = listing_with(&roots, &fs);
    assert_eq!(
        judge(&clean, &partial, std::slice::from_ref(&h), &fs),
        Verdict::Fail
    );
    fs.entry_errors.clear();
    fs.stat_errors.clear();
    fs.stat_errors.insert(missing.path.clone());
    let both = [h, missing];
    assert_eq!(judge(&clean, &dirty, &both, &fs), Verdict::Fail);
    assert!(
        unexpected.is_empty(),
        "LAB-ABSENCE-COMPLETENESS: {unexpected:?}"
    );
}

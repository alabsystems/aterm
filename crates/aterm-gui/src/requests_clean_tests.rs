// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::requests_host::set_dir_for_test;
use aterm_requests::{Born, ClaimSrc, Form, From, Intake, Stage, Touch, TouchWhy, Via, why_of};
use aterm_uds::process::Birth;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
const BY: &str = "fleet:87be5228:tqn5r6e0148";
const TAB: &str = "s-aaaaaaaaaaaaaaaaaaaa";
/// A hang detector for the requests' writer thread.
const MUST: Duration = Duration::from_secs(60);

/// This test thread's requests in a folder of their own, and a home and a work folder beside it,
/// until dropped.
struct Here {
    _root: aterm_tempfile::TempDir,
    dir: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

impl Here {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("a folder");
        // A claim records a folder's path with no link on the way (`/var` is one on macOS), each
        // followed by hand (`requests_footprint::walk`): `canonicalize` would spell a Windows
        // path verbatim (`\\?\`), which no claim or tab's folder is.
        let real = crate::requests_footprint::walk(None, root.path()).expect("its real path");
        let (dir, home, work) = (real.join("requests"), real.join("home"), real.join("work"));
        for folder in [&dir, &home, &work] {
            std::fs::create_dir_all(folder).expect("a folder");
        }
        set_dir_for_test(Some(dir.clone()));
        Self {
            _root: root,
            dir,
            home,
            work,
        }
    }

    fn store(&self) -> aterm_requests::Store {
        aterm_requests::Store::open(&self.dir).expect("the store")
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.dir.join(aterm_requests::JOURNAL)).unwrap_or_default()
    }

    fn places(&self) -> Places {
        Places {
            home: Some(self.home.clone()),
            state: None,
        }
    }

    /// A folder in the work folder, made now.
    fn folder(&self, name: &str) -> String {
        let path = self.work.join(name);
        std::fs::create_dir_all(&path).expect("a folder");
        path.to_str().expect("UTF-8").to_owned()
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        assert!(
            crate::requests_host::flush_within(MUST),
            "the writer flushed"
        );
        crate::requests_host::forget_for_test();
        set_dir_for_test(None);
    }
}

fn now() -> u64 {
    crate::requests_host::now_unix_ms()
}

/// A drive's plain shell request with tab [`TAB`], opened a minute ago.
fn running(store: &mut aterm_requests::Store) -> String {
    let at = now() - 60_000;
    let id = store
        .open_request(
            at,
            Intake {
                via: Via::FleetShell,
                form: Form::Shell,
                from: From {
                    node: NODE.to_owned(),
                    host: "m114".to_owned(),
                    by: BY.to_owned(),
                    program: None,
                    model: None,
                    account: None,
                },
                why: why_of(Via::FleetShell, Some("build it"), None, None, None),
                brief: None,
                goal: None,
                asked: None,
            },
        )
        .expect("opened");
    store.tab(at + 1, &id, TAB, BY, false, None).expect("a tab");
    id
}

/// Its tab left now: the request concluded.
fn conclude(store: &mut aterm_requests::Store) {
    store
        .left(
            now(),
            TAB,
            &aterm_requests::Left {
                reason: "shell-exit".to_owned(),
                code: Some(0),
                task: None,
                by: BY.to_owned(),
                node: Some(NODE.to_owned()),
                spent: None,
            },
        )
        .expect("left");
}

/// Claim `paths` for `id` as folders its own command made alone, each born when the filesystem
/// says (plus `skew` ms).
fn claim(store: &mut aterm_requests::Store, id: &str, paths: &[(&str, i64)]) {
    let found: Vec<Born> = paths
        .iter()
        .map(|(path, skew)| Born {
            id: id.to_owned(),
            path: (*path).to_owned(),
            src: ClaimSrc::Born,
            tab: Some(TAB.to_owned()),
            born_ms: crate::requests_footprint::born_ms(Path::new(path))
                .expect("a birth")
                .saturating_add_signed(*skew),
            alone: Some(true),
        })
        .collect();
    assert_eq!(
        store.born(now(), &found).expect("claimed").len(),
        paths.len()
    );
}

/// What the stand-in kernel holds: each pid's identity now, and the signals sent.
#[derive(Default)]
struct Kernel {
    now: HashMap<i32, Identity>,
    sent: Vec<(i32, i32)>,
    naps: usize,
}

struct FakeEnd(Arc<Mutex<Kernel>>);

impl EndControl for FakeEnd {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        self.0.lock().unwrap().now.get(&pid).copied()
    }

    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        let mut kernel = self.0.lock().unwrap();
        kernel.sent.push((token.pid, sig));
        if sig == 9 {
            kernel.now.remove(&token.pid);
        }
        Ok(())
    }

    fn nap(&mut self, _: Duration) {
        self.0.lock().unwrap().naps += 1;
    }
}

fn identity(pid: i32, birth: u64) -> Identity {
    Identity {
        pid,
        birth: Birth(birth),
        idversion: None,
    }
}

#[test]
fn tree_capture_requires_the_recorded_shell_generation_before_probing_descendants() {
    thread_local! { static CHILD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    let probe = crate::tab_marks::TreeProbe {
        children: |pid| {
            CHILD_READS.with(|reads| reads.set(reads.get() + 1));
            Some(if pid == 100 { vec![101] } else { Vec::new() })
        },
        ..crate::tab_marks::TreeProbe::INERT
    };
    let original = Identity {
        idversion: Some(17),
        ..identity(100, 1)
    };
    let child = Identity {
        idversion: Some(23),
        ..identity(101, 2)
    };
    let kernel = |pid| match pid {
        100 => Some(original),
        101 => Some(child),
        _ => None,
    };
    for birth in [None, Some(99)] {
        CHILD_READS.with(|reads| reads.set(0));
        let shell = aterm_pty::ShellIdentity::from_parts(100, birth, true);
        assert!(tree_with(&shell, &probe, kernel).is_empty());
        assert_eq!(CHILD_READS.with(std::cell::Cell::get), 0);
    }
    let shell = aterm_pty::ShellIdentity::from_parts(100, Some(1), false);
    assert_eq!(tree_with(&shell, &probe, kernel), [original, child]);
    assert_eq!(CHILD_READS.with(std::cell::Cell::get), 2);
}

/// A measure that reads every folder 10 bytes, its repos pushed but in one named `unpushed-repo`.
const MEASURE: MeasureProbe = MeasureProbe {
    walk: |_, _| Some((10, true)),
    repos: |path, _| Some(path.to_string_lossy().ends_with("unpushed-repo")),
};

/// A kit whose close answers `close` (and runs `then` first), its kernel `kernel`.
fn kit(
    here: &Here,
    captured: Vec<Captured>,
    close: Closed,
    then: impl Fn() + Send + 'static,
    kernel: &Arc<Mutex<Kernel>>,
) -> Kit {
    Kit {
        capture: Box::new(move |sid| captured.iter().find(|c| c.sid == sid).cloned()),
        close: Box::new(move |_, _, _| {
            then();
            Ok(close.clone())
        }),
        cwds: Box::new(Vec::new),
        end: Box::new(FakeEnd(Arc::clone(kernel))),
        grace: Duration::ZERO,
        measure: MEASURE,
        places: here.places(),
    }
}

/// `request clean <rest>` by `ctl`, run on this thread with `kit`.
fn clean(rest: &str, kit: Kit) -> String {
    let kit = Mutex::new(kit);
    let start = |job: Job| {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(run(job, &mut kit.lock().unwrap()));
        rx
    };
    cmd_as(rest, ExitActor::Ctl, "ctl", &|_| true, &start, MUST)
}

/// The fields of a one-line `OK …` reply.
#[cfg(unix)]
fn fields(reply: &str) -> HashMap<String, String> {
    reply
        .split_whitespace()
        .filter_map(|word| word.split_once('='))
        .map(|(k, v)| (k.to_owned(), pct::decode(v)))
        .collect()
}

/// The `gone` rows of request `id`, by path: what was done and why.
fn gone(here: &Here, id: &str) -> HashMap<String, (Did, Option<Keep>)> {
    here.store()
        .get(id)
        .expect("the request")
        .gone
        .iter()
        .map(|gone| (gone.path.clone(), (gone.did, gone.why)))
        .collect()
}

/// The archives in the archive's folder, written or being written, by name (its locks apart).
fn archives(here: &Here) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(here.dir.join(ARCHIVE))
        .expect("the archive")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.ends_with(".lock"))
        .collect();
    names.sort();
    names
}

/// The one folder in the archive that is being written.
#[cfg(unix)]
fn writing(here: &Here, id: &str) -> PathBuf {
    std::fs::read_dir(here.dir.join(ARCHIVE))
        .expect("the archive")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{id}.tmp-")))
        })
        .expect("an archive being written")
}

#[cfg(unix)]
#[test]
fn clean_archives_before_any_close() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    claim(&mut store, &id, &[(&scratch, 0)]);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let captured = vec![Captured {
        sid: TAB.to_owned(),
        screen: "$ make\nok\n".to_owned(),
        result: Some(b"# done".to_vec()),
        brief: Some(b"build it".to_vec()),
        tree: Vec::new(),
    }];
    let seen = Arc::new(Mutex::new(None));
    let (dir, at, saw) = (here.dir.clone(), id.clone(), Arc::clone(&seen));
    let then = move || {
        let tmp = std::fs::read_dir(dir.join(ARCHIVE))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains(".tmp-"))
            .expect("the archive is being written when the tabs close");
        let record = std::fs::read_to_string(tmp.join("record")).unwrap();
        let screen = std::fs::read_to_string(tmp.join(format!("screen-{TAB}.txt"))).unwrap();
        *saw.lock().unwrap() = Some((record.contains(&format!("op=clean req={at}")), screen));
    };
    let reply = clean(
        &format!("{id} stop=1"),
        kit(
            &here,
            captured,
            Closed {
                closed: vec![TAB.to_owned()],
                ..Closed::default()
            },
            then,
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!(
            "OK cleaned req={id} tabs=1 deleted=1 kept=0 failed=0 "
        )),
        "{reply}"
    );
    let (cleaned, screen) = seen.lock().unwrap().clone().expect("the closer ran");
    assert!(cleaned, "the record holds the clean");
    assert_eq!(screen, "$ make\nok\n");
    let archive = here.dir.join(ARCHIVE).join(&id);
    assert_eq!(fields(&reply)["archive"], format!("requests/archive/{id}"));
    for file in ["record", "said", "manifest.tsv"] {
        assert!(archive.join(file).is_file(), "{file}");
    }
    assert_eq!(
        std::fs::read(archive.join(format!("result-{TAB}.md"))).unwrap(),
        b"# done"
    );
    assert!(!Path::new(&scratch).exists());
    let request = here.store().get(&id).unwrap().clone();
    assert_eq!(request.stage(), Stage::Cleaned);
    assert_eq!(
        request
            .archives
            .iter()
            .map(|a| a.dir.as_str())
            .collect::<Vec<_>>(),
        [id.as_str()]
    );

    // NEGATIVE: an archive that could not be finished keeps every claim, as unarchived.
    let mut store = here.store();
    let second = running(&mut store);
    let kept = here.folder("kept-scratch");
    claim(&mut store, &second, &[(&kept, 0)]);
    let (dir, at) = (here.dir.clone(), second.clone());
    let lock = move || {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = std::fs::read_dir(dir.join(ARCHIVE))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains(&format!("{at}.tmp-")))
            .unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o500)).unwrap();
    };
    let reply = clean(
        &format!("{second} stop=1"),
        kit(&here, Vec::new(), Closed::default(), lock, &kernel),
    );
    assert!(
        reply.ends_with(" deleted=0 kept=1 failed=0 archive=-\n"),
        "{reply}"
    );
    assert!(Path::new(&kept).is_dir());
    assert_eq!(
        gone(&here, &second)[&kept],
        (Did::Kept, Some(Keep::Unarchived))
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = writing(&here, &second);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn clean_ends_only_captured_identities_that_survive_close() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    {
        let mut k = kernel.lock().unwrap();
        // 101 was reused by a new process; 102 survived the close; 103 was never in the tree.
        k.now.insert(101, identity(101, 99));
        k.now.insert(102, identity(102, 3));
        k.now.insert(103, identity(103, 4));
    }
    let captured = vec![Captured {
        sid: TAB.to_owned(),
        tree: vec![identity(100, 1), identity(101, 2), identity(102, 3)],
        ..Captured::default()
    }];
    let reply = clean(
        &format!("{id} stop=1"),
        kit(
            &here,
            captured,
            Closed {
                closed: vec![TAB.to_owned()],
                ..Closed::default()
            },
            || {},
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!("OK cleaned req={id} tabs=1 ")),
        "{reply}"
    );
    let k = kernel.lock().unwrap();
    assert_eq!(
        k.sent,
        vec![(102, 15), (102, 9)],
        "only the survivor, term then kill"
    );
    assert!(
        k.now.contains_key(&101) && k.now.contains_key(&103),
        "never signalled"
    );
    drop(k);
    // NEGATIVE: a process that leaves within its grace is never killed.
    let end = FakeEnd(Arc::new(Mutex::new(Kernel::default())));
    end.0.lock().unwrap().now.insert(7, identity(7, 1));
    struct Leaves(FakeEnd);
    impl EndControl for Leaves {
        fn identity(&mut self, pid: i32) -> Option<Identity> {
            self.0.identity(pid)
        }
        fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
            self.0.signal(token, sig)?;
            self.0.0.lock().unwrap().now.remove(&token.pid);
            Ok(())
        }
        fn nap(&mut self, pause: Duration) {
            self.0.nap(pause);
        }
    }
    let shared = Arc::clone(&end.0);
    assert_eq!(end_tree(&[identity(7, 1)], &mut Leaves(end), MUST), 1);
    let k = shared.lock().unwrap();
    assert_eq!(k.sent, vec![(7, 15)]);
    assert_eq!(k.naps, 0, "nothing waited on a process already gone");
}

#[test]
fn clean_deletes_born_alone_measured_claims_and_keeps_unpushed_touched_in_use_replaced() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    let repo = here.folder("unpushed-repo");
    let touched = here.folder("touched");
    let used = here.folder("used");
    let moved = here.folder("moved");
    claim(
        &mut store,
        &id,
        &[
            (&scratch, 0),
            (&repo, 0),
            (&touched, 0),
            (&used, 0),
            (&moved, -1),
        ],
    );
    store
        .touched(
            now(),
            &[Touch {
                id: id.clone(),
                path: touched.clone(),
                tab: "tbbbbbbbbbb".to_owned(),
                blk: 1,
                why: TouchWhy::PersonTab,
            }],
        )
        .unwrap();
    conclude(&mut store);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let reply = clean(
        &id,
        kit(
            &here,
            Vec::new(),
            Closed {
                cwds: vec![format!("{used}/deeper")],
                ..Closed::default()
            },
            || {},
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!(
            "OK cleaned req={id} tabs=0 deleted=1 kept=4 failed=0 "
        )),
        "{reply}"
    );
    assert!(!Path::new(&scratch).exists());
    for kept in [&repo, &touched, &used, &moved] {
        assert!(Path::new(kept).is_dir(), "{kept}");
    }
    let gone = gone(&here, &id);
    assert_eq!(gone[&scratch], (Did::Deleted, None));
    assert_eq!(gone[&repo], (Did::Kept, Some(Keep::Unpushed)));
    assert_eq!(gone[&touched], (Did::Kept, Some(Keep::Touched)));
    assert_eq!(gone[&used], (Did::Kept, Some(Keep::InUse)));
    assert_eq!(gone[&moved], (Did::Kept, Some(Keep::Replaced)));
    // The archive lists only the folder it deleted.
    let manifest =
        std::fs::read_to_string(here.dir.join(ARCHIVE).join(&id).join("manifest.tsv")).unwrap();
    assert!(manifest.contains(&pct::encode(&scratch)), "{manifest}");
    assert!(!manifest.contains(&pct::encode(&repo)), "{manifest}");
    // A second clean is already done, and keeps what it kept.
    assert_eq!(
        clean(
            &id,
            kit(&here, Vec::new(), Closed::default(), || {}, &kernel)
        ),
        format!("OK already req={id}\n")
    );
}

#[cfg(unix)]
#[test]
fn manifest_lists_every_file_keeps_small_text_and_marks_a_capped_walk_unlisted() {
    use std::os::unix::fs::PermissionsExt as _;
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    let base = Path::new(&scratch);
    std::fs::create_dir_all(base.join("out/deep")).unwrap();
    std::fs::write(base.join("receipt.json"), b"{\"ok\":true}").unwrap();
    std::fs::write(base.join("out/deep/build.bin"), [0_u8, 159, 146, 150]).unwrap();
    std::fs::write(base.join("big.log"), vec![b'x'; 300 * 1024]).unwrap();
    let outside = here.folder("outside");
    std::fs::write(Path::new(&outside).join("keep.txt"), b"mine").unwrap();
    std::os::unix::fs::symlink(&outside, base.join("link")).unwrap();
    let locked = here.folder("locked");
    std::fs::create_dir_all(Path::new(&locked).join("inner")).unwrap();
    claim(&mut store, &id, &[(&scratch, 0), (&locked, 0)]);
    conclude(&mut store);
    std::fs::set_permissions(
        Path::new(&locked).join("inner"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let reply = clean(
        &id,
        kit(&here, Vec::new(), Closed::default(), || {}, &kernel),
    );
    std::fs::set_permissions(
        Path::new(&locked).join("inner"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(
        reply.starts_with(&format!("OK cleaned req={id} tabs=0 deleted=1 kept=1 ")),
        "{reply}"
    );
    let archive = here.dir.join(ARCHIVE).join(&id);
    let manifest = std::fs::read_to_string(archive.join("manifest.tsv")).unwrap();
    let sha = |bytes: &[u8]| aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes));
    for row in [
        format!("file\treceipt.json\t11\t{}", sha(b"{\"ok\":true}")),
        format!("file\tout/deep/build.bin\t4\t{}", sha(&[0, 159, 146, 150])),
        "dir\tout\t-\t-".to_owned(),
        "link\tlink\t-\t-".to_owned(),
    ] {
        assert!(
            manifest.lines().any(|line| line == row),
            "{row} in {manifest}"
        );
    }
    assert!(manifest.contains("# whole=1"), "{manifest}");
    // Small text is kept whole; a big log and a binary are only listed.
    assert_eq!(
        std::fs::read(archive.join("files/1/receipt.json")).unwrap(),
        b"{\"ok\":true}"
    );
    assert!(!archive.join("files/1/big.log").exists());
    assert!(!archive.join("files/1/out/deep/build.bin").exists());
    // The link's target, never followed, stands.
    assert!(!base.exists());
    assert_eq!(
        std::fs::read(Path::new(&outside).join("keep.txt")).unwrap(),
        b"mine"
    );
    // NEGATIVE: a folder the listing could not read whole is kept, unlisted.
    assert!(Path::new(&locked).join("inner").is_dir());
    assert_eq!(gone(&here, &id)[&locked], (Did::Kept, Some(Keep::Unlisted)));
    let request = here.store().get(&id).unwrap().clone();
    assert!(!request.archives.last().unwrap().whole);
}

#[test]
fn a_busy_tab_without_stop_writes_uncleaned_and_deletes_nothing() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    claim(&mut store, &id, &[(&scratch, 0)]);
    conclude(&mut store);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let reply = clean(
        &id,
        kit(
            &here,
            Vec::new(),
            Closed {
                busy: vec![TAB.to_owned()],
                ..Closed::default()
            },
            || {},
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!(
            "ERR request clean: req={id} was not cleaned (why=busy)"
        )),
        "{reply}"
    );
    assert!(Path::new(&scratch).is_dir());
    let request = here.store().get(&id).unwrap().clone();
    assert_eq!(request.stage(), Stage::Reviewed);
    assert!(request.gone.is_empty());
    assert!(
        here.journal()
            .contains(&format!("op=uncleaned req={id} why=busy"))
    );
    // The archive it began is dropped with the cleanup.
    assert_eq!(archives(&here), Vec::<String>::new(), "no archive is left");
    // NEGATIVE: a running request is refused without stop=1, and nothing is written.
    let mut store = here.store();
    let live = {
        let id = running(&mut store);
        drop(store);
        id
    };
    let before = here.journal();
    let reply = clean(
        &live,
        kit(&here, Vec::new(), Closed::default(), || {}, &kernel),
    );
    assert!(
        reply.starts_with("ERR request clean: the request has not concluded"),
        "{reply}"
    );
    assert_eq!(here.journal(), before);
}

#[cfg(unix)]
#[test]
fn a_symlinked_claim_is_never_followed_on_delete() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    claim(&mut store, &id, &[(&scratch, 0)]);
    conclude(&mut store);
    let target = here.folder("target");
    std::fs::write(Path::new(&target).join("work.txt"), b"a person's").unwrap();
    // The claimed folder becomes a link to another while its tabs close.
    let (from, to) = (scratch.clone(), target.clone());
    let swap = move || {
        std::fs::remove_dir(&from).unwrap();
        std::os::unix::fs::symlink(&to, &from).unwrap();
    };
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let reply = clean(
        &id,
        kit(&here, Vec::new(), Closed::default(), swap, &kernel),
    );
    assert!(reply.contains(" deleted=0 kept=1 "), "{reply}");
    assert!(
        std::fs::symlink_metadata(&scratch)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(Path::new(&target).join("work.txt")).unwrap(),
        b"a person's"
    );
    assert_eq!(
        gone(&here, &id)[&scratch],
        (Did::Kept, Some(Keep::Replaced))
    );
}

#[test]
fn a_cut_clean_resumes_and_skips_what_it_deleted() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let done = here.folder("done");
    let left = here.folder("left");
    claim(&mut store, &id, &[(&done, 0), (&left, 0)]);
    conclude(&mut store);
    // A cleanup cut after it deleted one folder and said so.
    let begun = store
        .begin_clean(now(), &id, "ctl", false, &|_| true)
        .unwrap();
    assert!(matches!(begun, CleanStart::Plan(_)));
    std::fs::remove_dir(&done).unwrap();
    store
        .gone(
            &id,
            &GoneClaim {
                at_ms: now(),
                path: done.clone(),
                bytes: Some(10),
                why: None,
                did: Did::Deleted,
            },
        )
        .unwrap();
    drop(store);
    assert_eq!(here.store().get(&id).unwrap().stage(), Stage::Cleaning);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    let reply = clean(
        &id,
        kit(&here, Vec::new(), Closed::default(), || {}, &kernel),
    );
    // Both deleted folders are counted: the one the cut cleanup deleted, and the one it left.
    assert!(
        reply.starts_with(&format!(
            "OK cleaned req={id} tabs=0 deleted=2 kept=0 failed=0 "
        )),
        "{reply}"
    );
    assert!(!Path::new(&left).exists());
    let manifest =
        std::fs::read_to_string(here.dir.join(ARCHIVE).join(&id).join("manifest.tsv")).unwrap();
    assert!(!manifest.contains(&pct::encode(&done)), "{manifest}");
    assert_eq!(
        here.journal().matches(&format!("op=gone req={id}")).count(),
        2,
        "one row per folder"
    );
}

#[test]
fn close_request_tabs_closes_the_asked_and_reports_remaining_cwds() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));
    app.push_stub_tab(wid, crate::stub_session(2));
    // A folder this host spells absolute (Windows' names a drive), as the tab reports it and as a
    // claim records it.
    let (reported, stays) = if cfg!(windows) {
        ("file:///C:/Users//a/work/stays", r"C:\Users\a\work\stays")
    } else {
        ("file:///Users//a/work/stays", "/Users//a/work/stays")
    };
    crate::feed_session_for_test(&app, 2, format!("\x1b]7;{reported}\x07").as_bytes());
    let sid = |app: &crate::App, session: u64| {
        app.store
            .read()
            .unwrap()
            .by_local(session)
            .map(|handle| handle.sid.as_str().to_owned())
            .expect("registered")
    };
    let (one, two) = (sid(&app, 1), sid(&app, 2));
    let closing = app.close_request_tabs(&[(1, one.clone())], &ExitActor::Ctl, false);
    let closed = app.request_tabs_closed(closing);
    assert_eq!(closed.closed, vec![one]);
    assert!(
        closed.busy.is_empty() && closed.failed.is_empty(),
        "{closed:?}"
    );
    assert!(
        closed.cwds.iter().any(|cwd| cwd == stays),
        "{:?}",
        closed.cwds
    );
    // NEGATIVE: the tab not asked for stands.
    assert!(app.store.read().unwrap().by_local(2).is_some());
    assert_eq!(sid(&app, 2), two);
}

/// A cleanup's hold is a lock on a file of the store's, so a second take is refused whether this
/// aterm or another on the same state home asks (each take opens the file anew).
#[test]
fn a_second_flight_of_one_request_is_refused_and_releases_nothing() {
    let root = aterm_tempfile::tempdir().expect("a folder");
    let dir = root.path();
    let take = |id: &str| Flight::take(dir, id).expect("the lock file");
    let first = take("r-1").expect("the first");
    assert!(take("r-1").is_none());
    // NEGATIVE: the refused take left the first's hold standing.
    assert!(take("r-1").is_none());
    assert!(take("r-2").is_some());
    drop(first);
    assert!(take("r-1").is_some());
}

/// A close the main thread did not take in time is cancelled, so it never runs after; one it
/// took is waited for, and one whose reply it dropped after taking reads every tab not closed.
#[test]
fn a_close_the_main_thread_did_not_take_in_time_is_cancelled() {
    let sids = [TAB.to_owned()];
    // Not taken within the wait: cancelled, and the main thread may not take it after.
    let ticket = Ticket::default();
    let (_tx, rx) = mpsc::channel::<Closed>();
    assert!(closed_within(&rx, &ticket, Duration::ZERO, &sids).is_err());
    assert!(!ticket.take(), "the main thread closes nothing after");
    // Taken, then answered.
    let ticket = Ticket::default();
    let (tx, rx) = mpsc::channel();
    assert!(ticket.take());
    let answer = Closed {
        closed: sids.to_vec(),
        ..Closed::default()
    };
    tx.send(answer.clone()).unwrap();
    assert_eq!(closed_within(&rx, &ticket, MUST, &sids), Ok(answer));
    // Taken, then its reply dropped: no tab reads closed.
    let ticket = Ticket::default();
    let (tx, rx) = mpsc::channel::<Closed>();
    assert!(ticket.take());
    drop(tx);
    let closed = closed_within(&rx, &ticket, Duration::ZERO, &sids).unwrap();
    assert!(closed.closed.is_empty());
    assert_eq!(closed.failed.len(), 1);
    // NEGATIVE: a reply dropped before the main thread took it cancels, closing nothing.
    let ticket = Ticket::default();
    let (tx, rx) = mpsc::channel::<Closed>();
    drop(tx);
    assert!(closed_within(&rx, &ticket, MUST, &sids).is_err());
    assert!(!ticket.take());
}

/// A tab whose close failed stands: its tree is never signalled, nothing is deleted and the
/// cleanup ends `uncleaned why=closer`, leaving no archive where no tab closed.
#[test]
fn a_tab_that_did_not_close_keeps_its_processes_and_every_folder() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let scratch = here.folder("scratch");
    claim(&mut store, &id, &[(&scratch, 0)]);
    let kernel = Arc::new(Mutex::new(Kernel::default()));
    kernel.lock().unwrap().now.insert(100, identity(100, 1));
    let captured = vec![Captured {
        sid: TAB.to_owned(),
        tree: vec![identity(100, 1)],
        ..Captured::default()
    }];
    let reply = clean(
        &format!("{id} stop=1"),
        kit(
            &here,
            captured.clone(),
            Closed {
                failed: vec![(TAB.to_owned(), "close deferred".to_owned())],
                ..Closed::default()
            },
            || {},
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!(
            "ERR request clean: req={id} was not cleaned (why=closer): {TAB} (close deferred) \
             did not close"
        )),
        "{reply}"
    );
    assert!(kernel.lock().unwrap().sent.is_empty(), "never signalled");
    assert!(Path::new(&scratch).is_dir());
    let request = here.store().get(&id).unwrap().clone();
    assert!(request.gone.is_empty() && request.cleaned.is_none());
    assert_eq!(archives(&here), Vec::<String>::new());
    // NEGATIVE: the same tree of a tab that closed is ended.
    let reply = clean(
        &format!("{id} stop=1"),
        kit(
            &here,
            captured,
            Closed {
                closed: vec![TAB.to_owned()],
                ..Closed::default()
            },
            || {},
            &kernel,
        ),
    );
    assert!(
        reply.starts_with(&format!("OK cleaned req={id} tabs=1 ")),
        "{reply}"
    );
    assert_eq!(kernel.lock().unwrap().sent, vec![(100, 15), (100, 9)]);
}

/// An archive whose first part could not be written is dropped: no unfinished archive stays to
/// push the next cleanup's name aside.
#[cfg(unix)]
#[test]
fn an_archive_whose_first_part_failed_is_dropped() {
    use std::os::unix::fs::PermissionsExt as _;
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    drop(store);
    let journal = here.dir.join(aterm_requests::JOURNAL);
    let shut = journal.clone();
    // The request's rows cannot be read once its tab's screen is: the first part fails.
    let mut kit = kit(&here, Vec::new(), Closed::default(), || {}, &Arc::default());
    kit.capture = Box::new(move |sid| {
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
        Some(Captured {
            sid: sid.to_owned(),
            ..Captured::default()
        })
    });
    let reply = clean(&format!("{id} stop=1"), kit);
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        reply.starts_with(&format!(
            "ERR request clean: req={id} was not cleaned (why=store)"
        )),
        "{reply}"
    );
    assert_eq!(
        archives(&here),
        Vec::<String>::new(),
        "nothing unfinished stays"
    );
    // NEGATIVE: the next cleanup's archive takes the request's own name.
    let reply = clean(&format!("{id} stop=1"), kit_of(&here, Closed::default()));
    assert!(
        reply.starts_with(&format!("OK cleaned req={id} ")),
        "{reply}"
    );
    assert_eq!(archives(&here), [id]);
}

/// A plain kit whose close answers `close`.
fn kit_of(here: &Here, close: Closed) -> Kit {
    kit(here, Vec::new(), close, || {}, &Arc::default())
}

/// A claim missing when a first cleanup looks was removed by something else: it is kept as
/// missing, never counted deleted. CONTROL: a resumed cleanup counts it deleted (the cut cleanup
/// deleted it without saying so).
#[test]
fn a_claim_missing_at_a_first_cleanup_is_not_recorded_deleted() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let missing = here.folder("missing");
    claim(&mut store, &id, &[(&missing, 0)]);
    conclude(&mut store);
    drop(store);
    std::fs::remove_dir(&missing).unwrap();
    let reply = clean(&id, kit_of(&here, Closed::default()));
    assert!(
        reply.starts_with(&format!(
            "OK cleaned req={id} tabs=0 deleted=0 kept=1 failed=0 "
        )),
        "{reply}"
    );
    assert_eq!(gone(&here, &id)[&missing], (Did::Kept, Some(Keep::Missing)));
    // CONTROL: resumed, it reads deleted.
    let mut store = here.store();
    let second = running(&mut store);
    let cut = here.folder("cut");
    claim(&mut store, &second, &[(&cut, 0)]);
    conclude(&mut store);
    let begun = store
        .begin_clean(now(), &second, "ctl", false, &|_| true)
        .unwrap();
    assert!(matches!(begun, CleanStart::Plan(_)));
    drop(store);
    std::fs::remove_dir(&cut).unwrap();
    let reply = clean(&second, kit_of(&here, Closed::default()));
    assert!(
        reply.starts_with(&format!(
            "OK cleaned req={second} tabs=0 deleted=1 kept=0 failed=0 "
        )),
        "{reply}"
    );
}

/// Folder `path`'s entries' [`times`] alone, in the walk's order: what its shape would tell with
/// nothing counting its entries' changes.
fn times_of(path: &Path, home: Option<&Path>) -> Vec<String> {
    let mut lines = Vec::new();
    let whole = walk_claim(
        path,
        home,
        &mut Budget::new(),
        &mut |rel, seen, entry, _| {
            lines.push(times(rel, seen, &entry.meta));
            true
        },
    );
    assert!(whole, "walked whole");
    lines
}

/// Turn off the last-write-time updates the writes through `file` make (`SetFileTime` with
/// 0xFFFFFFFF_FFFFFFFF as the last write): a write through it leaves that time as it was, as a
/// write in the clock step of the write before it does, and the call itself changes nothing of
/// the file.
#[cfg(windows)]
fn keep_write_time(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle as _;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// Each time a `FILETIME` (two `DWORD`s, low first: one little-endian `u64`), or null to
        /// leave it as it is.
        fn SetFileTime(
            file: *mut core::ffi::c_void,
            creation: *const u64,
            access: *const u64,
            write: *const u64,
        ) -> i32;
    }
    let off = u64::MAX;
    // SAFETY: `file` keeps its handle open across the call; the creation and access times are
    // null (left as they are), and `off` is a readable `FILETIME`'s eight bytes that outlive the
    // call.
    let ok = unsafe {
        SetFileTime(
            file.as_raw_handle(),
            std::ptr::null(),
            std::ptr::null(),
            &raw const off,
        )
    };
    assert_ne!(ok, 0, "SetFileTime: {}", std::io::Error::last_os_error());
}

/// A folder's shape reads every change a write, a new entry or a removal makes after its
/// listing, where the entries' times do not tell it among them, by what counts each entry's
/// changes: on Windows its change-journal USN, on unix its status change.
///
/// On Windows, a write of as many bytes that leaves the file's modified time as it was (as a
/// write in the clock step of the write before it does; here through a handle whose time updates
/// are off) reads another shape while that handle is still open and nothing else of the file has
/// changed, by the journal's record of the write alone. CONTROL: the handle opened and its time
/// updates turned off, nothing written, reads the listed shape.
///
/// On every platform, a rewrite of as many bytes whose modified time is then put back where the
/// write before left it (as a copy that keeps a file's times leaves it) reads another shape: on
/// unix by the status change the put-back sets to the clock's now, on Windows by the journal's
/// records of the write and of the put-back. On unix a rewrite in the clock step of the write
/// before it, with no put-back, leaves the status change as it was where the clock steps coarser
/// than the writes (ATERM-KI-1115-47201), so no unix case here stands for one.
///
/// CONTROL throughout: an unchanged folder reads the shape its listing did, and each rewrite
/// leaves every entry's kind, path, size and modified time ([`times`]) as the listing read them,
/// so only the count tells it.
#[test]
fn a_folder_changed_after_its_listing_reads_another_shape() {
    let here = Here::new();
    let scratch = here.folder("scratch");
    let base = Path::new(&scratch);
    std::fs::create_dir_all(base.join("deep")).unwrap();
    let notes = base.join("deep/notes.md");
    std::fs::write(&notes, b"one").unwrap();
    let mut archive = Archiving::begin(&here.dir, "r-shape").unwrap();
    let home = Some(here.home.as_path());
    let listed = archive
        .list(base, home, &mut Budget::new())
        .expect("listed whole");
    let listed_times = times_of(base, home);
    assert_eq!(shape_of(base, home), Some(listed), "unchanged");
    #[cfg(windows)]
    let listed = {
        use std::io::Write as _;
        assert!(
            entry_at(&notes).expect("read").usn.is_some(),
            "the temp volume keeps a change journal"
        );
        let mut held = std::fs::File::options().write(true).open(&notes).unwrap();
        keep_write_time(&held);
        assert_eq!(
            shape_of(base, home),
            Some(listed),
            "opened, its time updates off, nothing written"
        );
        held.write_all(b"two").unwrap();
        assert_eq!(times_of(base, home), listed_times, "the write's times");
        let written = shape_of(base, home).expect("whole");
        assert_ne!(written, listed, "a write the times do not show");
        drop(held);
        shape_of(base, home).expect("whole")
    };
    let modified = std::fs::symlink_metadata(&notes)
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(&notes, b"six").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&notes)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(times_of(base, home), listed_times, "the rewrite's times");
    let written = shape_of(base, home).expect("whole");
    assert_ne!(written, listed, "a rewrite with its time put back");
    std::fs::write(base.join("new.txt"), b"").unwrap();
    let made = shape_of(base, home).expect("whole");
    assert_ne!(made, written, "a new file");
    std::fs::remove_file(base.join("new.txt")).unwrap();
    assert_ne!(shape_of(base, home), Some(made), "a removal");
    archive.discard();
}

thread_local! {
    /// How often this thread's repo probe ran.
    static REPO_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Each folder is judged again as it stands immediately before its removal: a repo with work no
/// remote has since its first measure (a commit during the listing) and a tab opened in it since
/// the tabs closed both keep it.
#[test]
fn a_folder_is_judged_again_on_fresh_facts_before_its_removal() {
    let here = Here::new();
    let mut store = here.store();
    let id = running(&mut store);
    let repo = here.folder("repo");
    claim(&mut store, &id, &[(&repo, 0)]);
    conclude(&mut store);
    drop(store);
    REPO_READS.with(|reads| reads.set(0));
    let mut committed = kit_of(&here, Closed::default());
    // Pushed when first measured; unpushed when read again.
    committed.measure = MeasureProbe {
        walk: |_, _| Some((10, true)),
        repos: |_, _| {
            REPO_READS.with(|reads| {
                reads.set(reads.get() + 1);
                Some(reads.get() > 1)
            })
        },
    };
    let reply = clean(&id, committed);
    assert!(reply.contains(" deleted=0 kept=1 "), "{reply}");
    assert_eq!(gone(&here, &id)[&repo], (Did::Kept, Some(Keep::Unpushed)));
    assert_eq!(REPO_READS.with(std::cell::Cell::get), 2, "read again");
    // A tab opened in the folder since its tabs closed.
    let mut store = here.store();
    let second = running(&mut store);
    let used = here.folder("used");
    claim(&mut store, &second, &[(&used, 0)]);
    conclude(&mut store);
    drop(store);
    let mut opened = kit_of(&here, Closed::default());
    let cwd = format!("{used}/inner");
    opened.cwds = Box::new(move || vec![cwd.clone()]);
    let reply = clean(&second, opened);
    assert!(reply.contains(" deleted=0 kept=1 "), "{reply}");
    assert_eq!(gone(&here, &second)[&used], (Did::Kept, Some(Keep::InUse)));
    // NEGATIVE: with neither, the folder goes.
    let mut store = here.store();
    let third = running(&mut store);
    let plain = here.folder("plain");
    claim(&mut store, &third, &[(&plain, 0)]);
    conclude(&mut store);
    drop(store);
    let reply = clean(&third, kit_of(&here, Closed::default()));
    assert!(reply.contains(" deleted=1 kept=0 "), "{reply}");
    assert!(!Path::new(&plain).exists());
}

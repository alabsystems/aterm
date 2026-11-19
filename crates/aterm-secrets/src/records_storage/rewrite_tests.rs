// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rewrite of a records entry (tracker FL-148D): cut at any step, any flush and any unlink it
//! recovers to exactly the predecessor or exactly the new data; a build that reads only format 1
//! refuses what it wrote, whole; and it replaces what the entry holds, freeing the predecessor.

use super::*;
use crate::crossing::capacity;
use crate::membership_log::RecordBudgets;
use crate::records_storage as storage;
use std::collections::BTreeMap;

/// The records a fixture's entry holds before any rewrite.
const OLD: &[u8] = b"original history";
/// What a fixture's first rewrite wrote, the predecessor of [`Before::Rewritten`].
const FIRST: &[u8] = b"the first rewrite's data";
/// What the rewrite under test writes.
const NEW: &[u8] = b"the data the rewrite under test writes";
/// What a build that knows no rewrite appends between a crash and recovery.
const APPENDED: &[u8] = b", and a record appended since";
/// The head beside a fixture's entry, of its predecessor.
const HEAD: &[u8] = b"a head of the predecessor";
const WHAT: What = What::Checkpoint([0xC4; 32]);
const HELD: usize = 32;

fn held<T>(run: impl FnOnce() -> T) -> T {
    RecordBudgets {
        weighed: 16,
        keyed: 128,
        held: HELD,
    }
    .within(run)
}

/// Every file and folder under a fixture's root, by its path from it, with its bytes: all a
/// rewrite or a recovery may change. The writer's lock file is left out.
type Tree = BTreeMap<String, Vec<u8>>;

fn tree(root: &Path) -> Tree {
    fn walk(root: &Path, dir: &Path, out: &mut Tree) {
        for item in std::fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if path.is_dir() {
                out.insert(format!("{name}/"), Vec::new());
                walk(root, &path, out);
            } else if !name.ends_with(".writing") {
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = Tree::new();
    walk(root, root, &mut out);
    out
}

/// How `have` differs from `want`, by name.
fn differs(want: &Tree, have: &Tree) -> String {
    let mut out = Vec::new();
    for name in want.keys().filter(|name| !have.contains_key(*name)) {
        out.push(format!("{name} is gone"));
    }
    for (name, bytes) in have {
        match want.get(name) {
            None => out.push(format!("{name} is left")),
            Some(wanted) if wanted != bytes => out.push(format!("{name} is changed")),
            Some(_) => {}
        }
    }
    out.join(", ")
}

/// What a fixture's entry is before the rewrite under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Before {
    /// A records file made a format-1 layout, never rewritten: its original kept beside it.
    Layout,
    /// A layout rewritten once: generation 1.
    Rewritten,
}

struct Fixture {
    kind: Before,
    root: aterm_tempfile::TempDir,
    entry: PathBuf,
    /// Everything under the root before the rewrite under test.
    before: Tree,
    /// The predecessor's data.
    old: &'static [u8],
    /// The predecessor's generation.
    generation: u64,
}

fn fixture(before: Before) -> Fixture {
    let root = aterm_tempfile::tempdir().unwrap();
    let entry = root.path().join("records");
    fsx::write_atomic(&entry, OLD, 0o600).unwrap();
    drop(storage::write_path(&entry, true).unwrap());
    let (old, generation) = match before {
        Before::Layout => (OLD, 0),
        Before::Rewritten => {
            let shed = What::Shed {
                records: 2,
                digest: [7; 32],
            };
            let done = rewrite(&entry, Predecessor::of(OLD), shed, &|out| {
                out.write_all(FIRST)
            })
            .unwrap();
            assert_eq!(done.generation, 1);
            (FIRST, 1)
        }
    };
    fsx::write_atomic(&storage::head_path(&entry).unwrap(), HEAD, 0o600).unwrap();
    let kind = before;
    let before = tree(root.path());
    Fixture {
        kind,
        root,
        entry,
        before,
        old,
        generation,
    }
}

/// Where the rewrite under test is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Crash {
    /// Once this step is durable.
    After(Step),
    /// Inside a step ([`Cut`]).
    At(Cut),
}

/// What happens between the crash and recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    Nothing,
    /// The intent loses its second half.
    TornIntent,
    /// Every file of the new generation, which the pin does not select yet, loses its second
    /// half.
    TornUnpinned,
    /// A build that knows no rewrite appends to the data file the pin selects.
    Appended,
    /// Recovery itself dies once it has unlinked this many files, then runs again.
    RecoveryCut(usize),
}

/// What a scenario did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ran {
    /// The crash came: the rewrite did not return its result.
    crashed: bool,
    /// The recovery's own crash came.
    recovery_crashed: bool,
    /// The entry is the new data, not the predecessor.
    new: bool,
}

/// The nonce of the original records file kept beside the layout in `tree`, where one is.
fn nonce_of(tree: &Tree) -> Option<String> {
    tree.keys()
        .find_map(|name| name.strip_prefix(".aterm-records-"))
        .map(str::to_owned)
}

/// `text` with the nonce of `tree`'s kept original, where it has one, written `<nonce>`.
fn unnamed(text: &str, tree: &Tree) -> String {
    match nonce_of(tree) {
        Some(nonce) => text.replace(&nonce, "<nonce>"),
        None => text.to_owned(),
    }
}

/// The tree an uninterrupted rewrite under test leaves, for each [`Before`].
struct Reference {
    /// Of a fixture never rewritten, whose kept original's nonce its new header names.
    layout: (Tree, String),
    rewritten: Tree,
}

impl Reference {
    fn new() -> Self {
        let of = |before| {
            let fx = fixture(before);
            let done = Context::new(&fx.entry)
                .unwrap()
                .rewrite_with(
                    Predecessor::of(fx.old),
                    WHAT,
                    &|out| out.write_all(NEW),
                    &|_| Ok(()),
                )
                .unwrap();
            assert_eq!(done.generation, fx.generation + 1);
            (tree(fx.root.path()), nonce_of(&fx.before))
        };
        let (layout, nonce) = of(Before::Layout);
        Self {
            layout: (layout, nonce.expect("the fixture kept its original")),
            rewritten: of(Before::Rewritten).0,
        }
    }

    /// The tree as the reference's own fixture left it.
    fn raw(&self, before: Before) -> &Tree {
        match before {
            Before::Layout => &self.layout.0,
            Before::Rewritten => &self.rewritten,
        }
    }

    /// The tree an uninterrupted rewrite of `fx` leaves: the reference's, its new header
    /// naming `fx`'s own kept original and its pin that header.
    fn of(&self, fx: &Fixture) -> Tree {
        let mut tree = self.raw(fx.kind).clone();
        if fx.kind == Before::Layout {
            let nonce = nonce_of(&fx.before).expect("the fixture kept its original");
            let header = String::from_utf8(tree["records/layout.1"].clone())
                .unwrap()
                .replace(&self.layout.1, &nonce)
                .into_bytes();
            let pin = Generation::decode(&header).unwrap().pin(HELD);
            tree.insert("records/layout.1".to_owned(), header);
            tree.insert("records.layout".to_owned(), pin);
        }
        tree
    }
}

fn halve(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let len = file.metadata().unwrap().len();
        file.set_len(len / 2).unwrap();
    }
}

/// What a reader of format 2 selects at `entry`, read whole.
fn selects(entry: &Path) -> std::result::Result<(u64, Vec<u8>), String> {
    let custody = data_path(entry).map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&custody.path).map_err(|error| error.to_string())?;
    Ok((custody.generation, bytes))
}

/// One rewrite of a fixture, cut at `crash`, then `then`, then recovery, the rewrite and the
/// recovery run as `mutant`: an `Err` says how what stands is neither exactly the predecessor
/// nor exactly the new data, as the crash's place says it must be.
fn survives(
    reference: &Reference,
    before: Before,
    crash: Crash,
    then: Then,
    mutant: Option<Mutant>,
) -> std::result::Result<Ran, String> {
    let fx = fixture(before);
    let context = Context::new(&fx.entry).unwrap();
    MUTANT.set(mutant);
    if let Crash::At(cut) = crash {
        CUT.set(Some(cut));
    }
    let result = context.rewrite_with(
        Predecessor::of(fx.old),
        WHAT,
        &|out| out.write_all(NEW),
        &|step| {
            if crash == Crash::After(step) {
                Err(refused("a test's crash came after a step"))
            } else {
                Ok(())
            }
        },
    );
    CUT.set(None);
    let crashed = result.is_err();
    // Where the crash's place puts the pin: nothing before the pin's rename moves it.
    let new = !crashed
        || matches!(
            crash,
            Crash::After(Step::Pinned | Step::Exchanged)
                | Crash::At(Cut::Unlinked(_) | Cut::Unflushed)
        );
    let next = fx.generation + 1;
    let mut old = fx.old.to_vec();
    let mut want = if new {
        reference.of(&fx)
    } else {
        fx.before.clone()
    };
    let mut recovery_crashed = false;
    match then {
        Then::Nothing => {}
        Then::TornIntent => halve(&fx.entry.with_file_name("records.rewriting")),
        Then::TornUnpinned => {
            assert!(
                !new,
                "the predecessor's files were durable long before the rewrite"
            );
            for name in [data_name(next), header_name(next)] {
                halve(&fx.entry.join(&name));
                halve(&fx.entry.join(format!("{name}.building")));
            }
        }
        Then::Appended => {
            let pinned = if new { next } else { fx.generation };
            let name = data_name(pinned);
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(fx.entry.join(&name))
                .map_err(|error| format!("the pinned data file cannot be appended to: {error}"))?;
            file.write_all(APPENDED).unwrap();
            want.get_mut(&format!("records/{name}"))
                .unwrap()
                .extend_from_slice(APPENDED);
            old.extend_from_slice(APPENDED);
        }
        Then::RecoveryCut(unlinks) => {
            CUT.set(Some(Cut::Unlinked(unlinks)));
            recovery_crashed = recover(&fx.entry).is_err();
            CUT.set(None);
        }
    }
    let finish = || -> std::result::Result<(), String> {
        let (generation, data) = if new {
            let mut data = NEW.to_vec();
            if then == Then::Appended {
                data.extend_from_slice(APPENDED);
            }
            (next, data)
        } else {
            (fx.generation, old.clone())
        };
        let which = if new {
            "the new data"
        } else {
            "the predecessor"
        };
        // Before recovery a reader already selects one generation, whole.
        match selects(&fx.entry) {
            Ok(selected) if selected == (generation, data.clone()) => {}
            Ok((selected, bytes)) if selected == generation => {
                return Err(format!(
                    "before recovery a reader reads {} bytes as {which}, which is {} bytes",
                    bytes.len(),
                    data.len()
                ));
            }
            Ok((selected, _)) => {
                return Err(format!(
                    "before recovery a reader selects generation {selected}, not {which}"
                ));
            }
            Err(error) => {
                return Err(format!(
                    "before recovery no reader selects {which}: {error}"
                ));
            }
        }
        recover(&fx.entry).map_err(|error| format!("recovery is refused: {error}"))?;
        let after = tree(fx.root.path());
        if after != want {
            return Err(format!(
                "after recovery the entry is not exactly {which}: {}",
                differs(&want, &after)
            ));
        }
        recover(&fx.entry).map_err(|error| format!("a second recovery is refused: {error}"))?;
        let again = tree(fx.root.path());
        if again != after {
            return Err(format!(
                "a second recovery changed the entry: {}",
                differs(&after, &again)
            ));
        }
        if selects(&fx.entry) != Ok((generation, data.clone())) {
            return Err(format!("after recovery no reader selects {which} whole"));
        }
        if generation == 0 {
            // A reader of format 1 reads a predecessor never rewritten as it always did.
            let path = storage::data_path(&fx.entry).map_err(|error| {
                format!("a format-1 reader is refused the predecessor: {error}")
            })?;
            if std::fs::read(path).unwrap() != data {
                return Err("a format-1 reader reads other bytes than the predecessor's".to_owned());
            }
        }
        Ok(())
    };
    let finished = finish();
    MUTANT.set(None);
    finished?;
    if then == Then::Nothing {
        // What stands is an entry the next rewrite replaces.
        let generation = if new { next } else { fx.generation };
        let (_, standing) = selects(&fx.entry)?;
        let done = rewrite(&fx.entry, Predecessor::of(&standing), WHAT, &|out| {
            out.write_all(b"the next rewrite")
        })
        .map_err(|error| format!("the recovered entry takes no rewrite: {error}"))?;
        if done.generation != generation + 1
            || selects(&fx.entry) != Ok((generation + 1, b"the next rewrite".to_vec()))
        {
            return Err("the recovered entry's next rewrite is not selected".to_owned());
        }
    }
    Ok(Ran {
        crashed,
        recovery_crashed,
        new,
    })
}

const PIECES: [Piece; 4] = [Piece::Intent, Piece::Data, Piece::Header, Piece::Pin];
const STEPS: [Step; 5] = [
    Step::Intent,
    Step::Copied,
    Step::Prepared,
    Step::Pinned,
    Step::Exchanged,
];

/// A rewrite cut at every step, at every flush (a scratch file half written, and whole but not
/// yet renamed, for the intent, the data, the header and the pin; and the pin renamed with the
/// flush of its rename failed) and after every unlink recovers to exactly the predecessor where the pin had not moved and exactly the new data
/// where it had: every file under the entry's folder, byte for byte, with nothing left of the
/// other. It does so with the intent torn, with every file of the new generation torn, with
/// a record appended to the pinned data since the crash (kept), and where the recovery itself
/// dies after any of its unlinks; a second recovery changes nothing; before recovery a reader
/// already selects one generation whole; and the entry then takes its next rewrite. Both for an
/// entry never rewritten (a format-1 layout, its original kept beside it) and for one rewritten
/// before. A scratch file the disk cut short without an error is refused before its rename.
///
/// NEGATIVE CONTROLS (2026-11-17): each [`Mutant`] run over the whole matrix (every `survives`
/// of the loop below run as it) and seen to fail first with the message quoted; each is also
/// run below on one scenario and its message asserted, so it stays seen.
/// - `UnlinkFirst` (the predecessor unlinked before the pin moves): ``Layout, At(Torn(Pin)),
///   Nothing: before recovery no reader selects the predecessor: membership records storage: a
///   layout file is missing``.
/// - `PinFirst` (the pin moved as soon as the intent stands): ``Layout, After(Copied), Nothing:
///   before recovery no reader selects the predecessor: membership records storage: a layout
///   file is missing``.
/// - `NoRecovery` (recovery removes nothing): ``Layout, After(Intent), Nothing: after recovery
///   the entry is not exactly the predecessor: records.rewriting is left``.
/// - `Unverified` (no scratch file read back): ``Layout, At(Lost(Intent))`` at the assertion
///   that a lost half is refused; on the data, asserted below, ``before recovery a reader reads
///   19 bytes as the new data, which is 38 bytes``.
/// - `ForwardBeforePin` (a rewrite cut before its pin is finished where its files are whole):
///   ``Layout, After(Prepared), Nothing: after recovery the entry is not exactly the
///   predecessor: .aterm-records-<nonce> is gone, records.head is gone, records.preparing is
///   gone, records/current is gone, records/layout is gone, records.layout is changed,
///   records/current.1 is left, records/layout.1 is left``; asserted below with a record
///   appended to the predecessor since the crash, which goes with it.
#[test]
fn vault_checkpoint_a_crash_at_every_step_recovers() {
    held(|| {
        let reference = Reference::new();
        for before in [Before::Layout, Before::Rewritten] {
            // An uninterrupted rewrite leaves the new data and its header alone in the layout,
            // the pin beside it, and neither the original, the head nor either intent.
            let next = if before == Before::Layout { 1 } else { 2 };
            let names: Vec<_> = reference.raw(before).keys().cloned().collect();
            assert_eq!(
                names,
                [
                    "records.layout".to_owned(),
                    "records/".to_owned(),
                    format!("records/current.{next}"),
                    format!("records/layout.{next}"),
                ],
                "{before:?}"
            );
            assert_eq!(
                reference.raw(before)[&format!("records/current.{next}")],
                NEW
            );

            let mut crashes: Vec<Crash> = STEPS.into_iter().map(Crash::After).collect();
            for piece in PIECES {
                crashes.push(Crash::At(Cut::Torn(piece)));
                crashes.push(Crash::At(Cut::Flushed(piece)));
                crashes.push(Crash::At(Cut::Lost(piece)));
            }
            crashes.push(Crash::At(Cut::Unflushed));
            crashes.extend((1..=7).map(|unlinks| Crash::At(Cut::Unlinked(unlinks))));
            let mut unlink_crashes = 0;
            let (mut back, mut forward) = (0, 0);
            let (mut back_recovery_crashes, mut forward_recovery_crashes) = (0, 0);
            for crash in crashes {
                let run = |then| {
                    survives(&reference, before, crash, then, None)
                        .unwrap_or_else(|error| panic!("{before:?}, {crash:?}, {then:?}: {error}"))
                };
                let ran = run(Then::Nothing);
                match crash {
                    Crash::After(step) => {
                        assert!(ran.crashed, "{before:?}, {crash:?}");
                        assert_eq!(
                            ran.new,
                            matches!(step, Step::Pinned | Step::Exchanged),
                            "{before:?}, {crash:?}"
                        );
                    }
                    Crash::At(Cut::Unlinked(_)) => {
                        assert!(ran.new, "{before:?}, {crash:?}");
                        unlink_crashes += usize::from(ran.crashed);
                    }
                    // The pin was renamed and its flush failed: recovery, whose flush works,
                    // finishes the rewrite.
                    Crash::At(Cut::Unflushed) => {
                        assert!(ran.crashed && ran.new, "{before:?}, {crash:?}");
                    }
                    // A lost half is refused by the read back, and a crash is a crash.
                    Crash::At(_) => assert!(ran.crashed && !ran.new, "{before:?}, {crash:?}"),
                }
                if !ran.crashed {
                    continue;
                }
                if ran.new {
                    forward += 1;
                } else {
                    back += 1;
                }
                for then in [Then::TornIntent, Then::TornUnpinned, Then::Appended] {
                    if then == Then::TornUnpinned && ran.new {
                        continue;
                    }
                    assert_eq!(run(then).new, ran.new, "{before:?}, {crash:?}, {then:?}");
                }
                for unlinks in 1..=8 {
                    let again = run(Then::RecoveryCut(unlinks));
                    if !again.recovery_crashed {
                        break;
                    }
                    if ran.new {
                        forward_recovery_crashes += 1;
                    } else {
                        back_recovery_crashes += 1;
                    }
                }
            }
            // Every unlink of the rewrite was a crash's place: the predecessor's data and header,
            // the head and the intent, and for an entry never rewritten the original and the
            // first transition's intent.
            assert_eq!(
                unlink_crashes,
                if before == Before::Layout { 6 } else { 4 },
                "{before:?}"
            );
            assert!(
                back >= 15 && forward >= 7,
                "{before:?}: {back} back, {forward} forward"
            );
            assert!(
                back_recovery_crashes >= 30 && forward_recovery_crashes >= 10,
                "{before:?}: {back_recovery_crashes} and {forward_recovery_crashes} recovery crashes"
            );
        }

        let fails = |mutant, before, crash, then| {
            survives(&reference, before, crash, then, Some(mutant))
                .expect_err("a mutant's rewrite recovers")
        };
        assert_eq!(
            fails(
                Mutant::UnlinkFirst,
                Before::Layout,
                Crash::At(Cut::Flushed(Piece::Pin)),
                Then::Nothing
            ),
            "before recovery no reader selects the predecessor: membership records storage: a layout file is missing"
        );
        assert_eq!(
            fails(
                Mutant::PinFirst,
                Before::Layout,
                Crash::At(Cut::Torn(Piece::Data)),
                Then::Nothing
            ),
            "before recovery no reader selects the predecessor: membership records storage: a layout file is missing"
        );
        assert_eq!(
            fails(
                Mutant::NoRecovery,
                Before::Layout,
                Crash::After(Step::Prepared),
                Then::Nothing
            ),
            "after recovery the entry is not exactly the predecessor: records.rewriting is left, records/current.1 is left, records/layout.1 is left"
        );
        assert_eq!(
            fails(
                Mutant::Unverified,
                Before::Layout,
                Crash::At(Cut::Lost(Piece::Data)),
                Then::Nothing
            ),
            "before recovery a reader reads 19 bytes as the new data, which is 38 bytes"
        );
        let forward = fails(
            Mutant::ForwardBeforePin,
            Before::Layout,
            Crash::After(Step::Prepared),
            Then::Appended,
        );
        assert!(
            forward.starts_with("after recovery the entry is not exactly the predecessor: .aterm-records-")
                && forward.ends_with(
                    " is gone, records.head is gone, records.preparing is gone, records/current is gone, records/layout is gone, records.layout is changed, records/current.1 is left, records/layout.1 is left"
                ),
            "{forward}"
        );
    });
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A build that reads only format 1, as `origin/main` read a records entry before this module
/// (a0c29a8018b): written here from the format, not from this build's reader, so a change to
/// that reader changes nothing here. It answers the records it reads, and names in `opened`
/// every file it opened.
fn older_reader(
    entry: &Path,
    held: usize,
    opened: &mut Vec<String>,
) -> std::result::Result<Vec<u8>, String> {
    let mut read = |path: PathBuf| -> std::result::Result<Vec<u8>, String> {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let bytes = std::fs::read(&path).map_err(|error| format!("{name}: {error}"))?;
        opened.push(name);
        Ok(bytes)
    };
    let pin_path = entry.with_file_name("records.layout");
    if !pin_path.exists() {
        return read(entry.to_path_buf());
    }
    let pin = read(pin_path)?;
    let header = read(entry.join("layout"))?;
    let text = std::str::from_utf8(&header).map_err(|_| "invalid layout text".to_owned())?;
    let rows: Vec<_> = text.lines().collect();
    if rows.len() != 6 || rows[0] != "aterm-records-layout/1" {
        return Err("invalid layout header".to_owned());
    }
    if rows[1] != format!("entry {}", hex_of(&Sha256::digest(b"records"))) {
        return Err("the layout belongs to another records entry".to_owned());
    }
    let capacity: usize = rows[5]
        .strip_prefix("capacity ")
        .and_then(|text| text.parse().ok())
        .ok_or("invalid capacity")?;
    let pinned: usize = std::str::from_utf8(&pin)
        .ok()
        .and_then(|text| text.lines().nth(2))
        .and_then(|row| row.strip_prefix("capacity "))
        .and_then(|text| text.parse().ok())
        .filter(|pinned| *pinned >= capacity)
        .ok_or("invalid pinned capacity")?;
    let expected = format!(
        "aterm-records-pin/1\n{}\ncapacity {pinned}\n",
        hex_of(&Sha256::digest(&header))
    );
    if expected.as_bytes() != pin {
        return Err("the layout pin does not match".to_owned());
    }
    if pinned > held {
        return Err("this custody requires more capacity".to_owned());
    }
    read(entry.join("current"))
}

/// Whether every reader of format 1 refuses the entry of `root`, whole: the older build
/// ([`older_reader`]), which opens no data file, and this build's own format-1 paths, with no
/// file under the root changed by any of them. An `Err` says which read or changed something.
fn refused_by_format_one(root: &Path, entry: &Path) -> std::result::Result<(), String> {
    let before = tree(root);
    let mut opened = Vec::new();
    if let Ok(bytes) = older_reader(entry, HELD, &mut opened) {
        return Err(format!(
            "a build that reads only format 1 read {} bytes of a rewritten entry from {}",
            bytes.len(),
            opened.last().unwrap()
        ));
    }
    if let Some(name) = opened.iter().find(|name| name.starts_with("current")) {
        return Err(format!("a build that reads only format 1 opened {name}"));
    }
    if std::fs::read(entry).is_ok() {
        return Err("a build that reads a records file kept whole read the entry".to_owned());
    }
    let own: [(&str, Box<dyn Fn() -> bool>); 7] = [
        ("data_path", Box::new(|| storage::data_path(entry).is_ok())),
        ("selected", Box::new(|| storage::selected(entry).is_ok())),
        (
            "reader_path",
            Box::new(|| storage::reader_path(entry).is_ok()),
        ),
        (
            "write_path",
            Box::new(|| storage::write_path(entry, false).is_ok()),
        ),
        (
            "a fencing write_path",
            Box::new(|| storage::write_path(entry, true).is_ok()),
        ),
        (
            "write_path_if_unchanged",
            Box::new(|| storage::write_path_if_unchanged(entry, true, None).is_ok()),
        ),
        (
            "recover_pending",
            Box::new(|| storage::recover_pending(entry).is_ok()),
        ),
    ];
    for (name, takes) in own {
        if takes() {
            return Err(format!(
                "this build's format-1 {name} takes a rewritten entry"
            ));
        }
    }
    let after = tree(root);
    if after != before {
        return Err(format!(
            "a format-1 reader changed a rewritten entry: {}",
            differs(&before, &after)
        ));
    }
    Ok(())
}

/// A build that reads only format 1 refuses a rewritten layout, whole: it opens no data file
/// and changes nothing, so it reads no part of a file that holds a root's stubs and loses no
/// data. So do this build's own format-1 readers and writers, one of which had judged the
/// layout before the rewrite. It does so from the moment the pin moves, while the
/// predecessor's files still stand beside the new ones, and at every later generation; and until
/// the pin moves it reads the predecessor, whole, as it always did, a rewrite's files beside it
/// or not. Format 1 itself is what it was: the pin of a layout never rewritten is the format-1
/// pin, byte for byte.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::InPlace` (the rewrite writes its data over
/// `current`, under the format-1 header and pin), seen to fail with ``a build that reads only
/// format 1 read 38 bytes of a rewritten entry from current`` (asserted below).
#[test]
fn vault_checkpoint_an_older_pin_reader_refuses_a_rewritten_layout() {
    held(|| {
        let fx = fixture(Before::Layout);
        let (root, entry) = (fx.root.path(), fx.entry.as_path());
        // The positive control: the older build reads a format-1 layout, as this build does.
        let mut opened = Vec::new();
        assert_eq!(older_reader(entry, HELD, &mut opened).unwrap(), OLD);
        assert_eq!(opened, ["records.layout", "layout", "current"]);
        assert_eq!(
            std::fs::read(storage::data_path(entry).unwrap()).unwrap(),
            OLD
        );
        assert_eq!(
            storage::selected(entry).unwrap().path(),
            entry.join("current")
        );
        assert_eq!(
            fx.before["records.layout"],
            format!(
                "aterm-records-pin/1\n{}\ncapacity {HELD}\n",
                hex_of(&Sha256::digest(&fx.before["records/layout"]))
            )
            .into_bytes()
        );
        assert_eq!(generation(entry).unwrap().number, 0);
        assert!(
            refused_by_format_one(root, entry).is_err(),
            "a format-1 layout is read"
        );

        // Cut before the pin: the older build reads the predecessor, whole.
        let context = Context::new(entry).unwrap();
        let cut_at = |cut: Step| {
            context
                .rewrite_with(
                    Predecessor::of(OLD),
                    WHAT,
                    &|out| out.write_all(NEW),
                    &|step| {
                        if step == cut {
                            Err(refused("a test's crash came after a step"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();
        };
        cut_at(Step::Prepared);
        let mut opened = Vec::new();
        assert_eq!(older_reader(entry, HELD, &mut opened).unwrap(), OLD);
        assert_eq!(opened, ["records.layout", "layout", "current"]);
        assert_eq!(
            std::fs::read(storage::data_path(entry).unwrap()).unwrap(),
            OLD
        );
        // It appends there too, under the writer's lock, and recovery keeps what it wrote.
        let target = storage::write_path(entry, true).unwrap();
        assert_eq!(&*target, entry.join("current"));
        drop(target);
        recover(entry).unwrap();
        assert_eq!(tree(root), fx.before);

        // Cut once the pin moved, the predecessor's files still beside the new ones.
        cut_at(Step::Pinned);
        assert!(entry.join("current").exists() && entry.join("layout").exists());
        refused_by_format_one(root, entry).unwrap();
        recover(entry).unwrap();
        refused_by_format_one(root, entry).unwrap();
        assert_eq!(selects(entry), Ok((1, NEW.to_vec())));
        assert_eq!(generation(entry).unwrap().number, 1);

        // And at the next generation.
        rewrite(entry, Predecessor::of(NEW), WHAT, &|out| {
            out.write_all(b"again")
        })
        .unwrap();
        refused_by_format_one(root, entry).unwrap();
        assert_eq!(selects(entry), Ok((2, b"again".to_vec())));

        let fx = fixture(Before::Layout);
        MUTANT.set(Some(Mutant::InPlace));
        Context::new(&fx.entry)
            .unwrap()
            .rewrite_with(
                Predecessor::of(OLD),
                WHAT,
                &|out| out.write_all(NEW),
                &|_| Ok(()),
            )
            .unwrap();
        MUTANT.set(None);
        assert_eq!(
            refused_by_format_one(fx.root.path(), &fx.entry).unwrap_err(),
            "a build that reads only format 1 read 38 bytes of a rewritten entry from current"
        );
    });
}

/// A rewrite replaces what the entry holds and frees the predecessor: a records file kept
/// whole becomes a layout and is rewritten in one call, leaving the new data, its header and
/// the pin alone (no original beside it, no head); the generation a consumer reads
/// (`Paths::records_generation`) counts each rewrite from 0;
/// an entry with no records is refused; and a rewrite whose data is not the data it measured,
/// or is more than a format-2 layout holds, is refused with the entry exactly as it was. So is
/// a rewrite computed from other data than the entry holds under the writer's lock: a record
/// appended since its caller read the records stands.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::Unread` (the rewrite replaces whatever the entry
/// holds), run from the test's first line and seen to fail with ``assertion failed:
/// stale(OLD).is_err()``; run again at the test's end, where it replaces data its rewrite was
/// not computed from.
#[test]
fn vault_checkpoint_a_rewrite_replaces_what_the_entry_holds() {
    held(|| {
        // A scratch home's vault folder, where a consumer reads the generation.
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = crate::client::Paths::locate_for_home(home.path());
        std::fs::create_dir_all(&paths.dir).unwrap();
        let (root, entry) = (paths.dir.as_path(), paths.records_file());
        assert!(
            rewrite(&entry, Predecessor::of(b""), WHAT, &|out| out
                .write_all(NEW))
            .is_err()
        );
        assert!(tree(root).is_empty(), "an absent entry is left absent");
        assert_eq!(paths.records_generation().unwrap().number, 0);

        fsx::write_atomic(&entry, OLD, 0o600).unwrap();
        fsx::write_atomic(&storage::head_path(&entry).unwrap(), HEAD, 0o600).unwrap();
        assert_eq!(selects(&entry), Ok((0, OLD.to_vec())));
        let done = rewrite(&entry, Predecessor::of(OLD), WHAT, &|out| {
            out.write_all(NEW)
        })
        .unwrap();
        assert_eq!(
            done,
            Rewritten {
                generation: 1,
                path: entry.join("current.1"),
                len: NEW.len(),
                digest: Sha256::digest(NEW),
            }
        );
        let after = tree(root);
        assert_eq!(
            after.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "records.layout",
                "records/",
                "records/current.1",
                "records/layout.1"
            ]
        );
        assert_eq!(after["records/current.1"], NEW);
        let header = Generation::decode(&after["records/layout.1"]).unwrap();
        assert_eq!(header.from, (OLD.len(), Sha256::digest(OLD)));
        assert_eq!(header.data, (NEW.len(), Sha256::digest(NEW)));
        assert_eq!(
            (header.number, header.what, header.capacity),
            (1, WHAT, HELD)
        );
        assert_eq!(after["records.layout"], header.pin(HELD));
        assert_eq!(paths.records_generation().unwrap().number, 1);

        // Other bytes the second time, more than was measured, and more than the format holds.
        let calls = std::cell::Cell::new(0);
        let refused: [Produce<'_>; 3] = [
            &|out| {
                calls.set(calls.get() + 1);
                out.write_all(if calls.get() % 2 == 1 {
                    b"measured"
                } else {
                    b"MEASURED"
                })
            },
            &|out| {
                calls.set(calls.get() + 1);
                out.write_all(if calls.get() % 2 == 1 {
                    b"short"
                } else {
                    b"longer"
                })
            },
            &|out| {
                let chunk = vec![0u8; 1 << 20];
                for _ in 0..=capacity(FORMAT_TWO) >> 20 {
                    out.write_all(&chunk)?;
                }
                Ok(())
            },
        ];
        for produce in refused {
            assert!(rewrite(&entry, Predecessor::of(NEW), WHAT, produce).is_err());
            assert_eq!(tree(root), after, "a refused rewrite leaves the entry");
        }

        // A rewrite computed from other data than the entry holds: from the data before the
        // last rewrite, and from the data as it stood before a record was appended to it.
        let stale = |from: &[u8]| {
            rewrite(&entry, Predecessor::of(from), WHAT, &|out| {
                out.write_all(b"stale")
            })
        };
        assert!(stale(OLD).is_err());
        assert_eq!(tree(root), after);
        let mut data = std::fs::OpenOptions::new()
            .append(true)
            .open(entry.join("current.1"))
            .unwrap();
        data.write_all(APPENDED).unwrap();
        drop(data);
        let appended = tree(root);
        assert_eq!(
            stale(NEW).unwrap_err().to_string(),
            "membership records storage: the records changed since their rewrite was computed; read them again"
        );
        assert_eq!(tree(root), appended, "the appended record stands");
        let whole = [NEW, APPENDED].concat();

        let shed = What::Shed {
            records: 3,
            digest: [9; 32],
        };
        let done = rewrite(&entry, Predecessor::of(&whole), shed, &|out| {
            out.write_all(b"shed")
        })
        .unwrap();
        assert_eq!(done.generation, 2);
        assert_eq!(paths.records_generation().unwrap().number, 2);
        assert_eq!(selects(&entry), Ok((2, b"shed".to_vec())));
        let header = Generation::decode(&std::fs::read(entry.join("layout.2")).unwrap()).unwrap();
        assert_eq!(header.what, shed);
        assert_eq!(header.from, (whole.len(), Sha256::digest(&whole)));

        MUTANT.set(Some(Mutant::Unread));
        let taken = stale(NEW);
        MUTANT.set(None);
        assert!(
            taken.is_ok(),
            "Unread replaces data its rewrite was not computed from"
        );
    });
}

/// A rewritten layout is read exactly or not at all: a header or a pin with a byte changed, a
/// row added or its rows reordered is refused; so is a pin that names another header, another
/// generation or a capacity below the header's, a layout that requires more capacity than its
/// reader holds, and one whose data file is gone. Recovery refuses each too and removes
/// nothing: an active pin never falls back to another generation.
#[test]
fn vault_checkpoint_a_rewritten_layout_reads_exactly() {
    held(|| {
        let fx = fixture(Before::Rewritten);
        let (root, entry) = (fx.root.path(), fx.entry.as_path());
        let header = std::fs::read(entry.join("layout.1")).unwrap();
        let pin = std::fs::read(entry.with_file_name("records.layout")).unwrap();
        let decoded = Generation::decode(&header).unwrap();
        assert_eq!(decoded.encode(), header);
        assert_eq!(rewritten_pin(&pin).unwrap().unwrap().2, 1);
        assert!(
            rewritten_pin(&fx.before["records.layout"])
                .unwrap()
                .is_some()
        );
        assert!(
            rewritten_pin(b"aterm-records-pin/1\nabc\ncapacity 32\n")
                .unwrap()
                .is_none(),
            "a format-1 pin is not a rewrite's"
        );

        let text = String::from_utf8(header.clone()).unwrap();
        let kept = format!(
            "kept {}",
            decoded.kept.clone().expect("the fixture kept its original")
        );
        assert!(text.contains(&format!("\n{kept}\n")));
        assert!(
            Generation::decode(text.replace(&kept, "kept -").as_bytes()).is_ok(),
            "a first rewrite of an entry that kept no original"
        );
        let forged_headers = [
            text.replace("generation 1", "generation 01"),
            text.replace("generation 1", "generation 0"),
            text.replace("layout/2", "layout/3"),
            text.replace("capacity 32", "capacity 0"),
            text.replace("rewrite shed 2 ", "rewrite shed two "),
            format!("{text}more\n"),
            text.trim_end().to_owned(),
            text.replacen("from ", "data ", 1),
            // Only an entry's first rewrite names a kept original, by its 32 hex digits.
            text.replace("generation 1", "generation 2"),
            text.replace(&kept, &kept.to_uppercase()),
            text.replace(&kept, &kept[..kept.len() - 1]),
            text.replace(&kept, "kept"),
        ];
        for forged in &forged_headers {
            assert!(Generation::decode(forged.as_bytes()).is_err(), "{forged}");
        }
        let pin_text = String::from_utf8(pin.clone()).unwrap();
        let other = Generation {
            capacity: decoded.capacity + 1,
            ..decoded.clone()
        };
        let forged_pins = [
            pin_text.replace("generation 1", "generation 2"),
            pin_text.replace("capacity 32", "capacity 31"),
            pin_text.replace("capacity 32", "capacity 032"),
            format!("{pin_text}more\n"),
            pin_text.trim_end().to_owned(),
            String::from_utf8(other.pin(other.capacity)).unwrap(),
            pin_text[..pin_text.len() / 2].to_owned(),
        ];
        let pin_file = entry.with_file_name("records.layout");
        for forged in &forged_pins {
            fsx::write_atomic(&pin_file, forged.as_bytes(), 0o600).unwrap();
            let standing = tree(root);
            assert!(data_path(entry).is_err(), "{forged}");
            assert!(recover(entry).is_err(), "{forged}");
            assert!(
                rewrite(entry, Predecessor::of(FIRST), WHAT, &|out| out
                    .write_all(NEW))
                .is_err(),
                "{forged}"
            );
            assert_eq!(tree(root), standing, "{forged}: nothing is removed");
        }
        fsx::write_atomic(&pin_file, &pin, 0o600).unwrap();
        assert_eq!(selects(entry), Ok((1, FIRST.to_vec())));

        // A reader that holds fewer records than the pin requires.
        let fewer = RecordBudgets {
            weighed: 16,
            keyed: 128,
            held: HELD - 1,
        };
        assert!(fewer.within(|| data_path(entry)).is_err());
        // A pin raised past its header's capacity is one the header's reader takes.
        fsx::write_atomic(&pin_file, &decoded.pin(HELD + 8), 0o600).unwrap();
        assert!(
            data_path(entry).is_err(),
            "it requires more than this reader holds"
        );
        let more = RecordBudgets {
            weighed: 16,
            keyed: 128,
            held: HELD + 8,
        };
        assert_eq!(more.within(|| selects(entry)), Ok((1, FIRST.to_vec())));
        fsx::write_atomic(&pin_file, &pin, 0o600).unwrap();

        // An intent beside a pin that is not read: recovery removes nothing.
        std::fs::remove_file(entry.join("current.1")).unwrap();
        fsx::write_atomic(&entry.with_file_name("records.rewriting"), &header, 0o600).unwrap();
        let standing = tree(root);
        assert!(data_path(entry).is_err());
        assert!(recover(entry).is_err());
        assert_eq!(tree(root), standing);
    });
}

/// A rewrite of `before`'s fixture cut once `cut` is durable, run as `mutant`.
fn cut_after(before: Before, cut: Step, mutant: Option<Mutant>) -> Fixture {
    let fx = fixture(before);
    MUTANT.set(mutant);
    Context::new(&fx.entry)
        .unwrap()
        .rewrite_with(
            Predecessor::of(fx.old),
            WHAT,
            &|out| out.write_all(NEW),
            &|step| {
                if step == cut {
                    Err(refused("a test's crash came after a step"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
    MUTANT.set(None);
    fx
}

/// Run `act` as `mutant`.
fn run_as<T>(mutant: Option<Mutant>, act: impl FnOnce() -> T) -> T {
    MUTANT.set(mutant);
    let out = act();
    MUTANT.set(None);
    out
}

/// A rewrite whose flush of the pin's rename fails, and the recovery after it while the flush
/// still fails, then the power cut that takes the rename back: an `Err` says how the entry is
/// not exactly the predecessor then.
fn pin_unflushed(before: Before, mutant: Option<Mutant>) -> std::result::Result<(), String> {
    let fx = fixture(before);
    CUT.set(Some(Cut::Unflushed));
    let (result, again) = run_as(mutant, || {
        let result = rewrite(&fx.entry, Predecessor::of(fx.old), WHAT, &|out| {
            out.write_all(NEW)
        });
        (result, recover(&fx.entry))
    });
    CUT.set(None);
    let standing = tree(fx.root.path());
    for (name, bytes) in &fx.before {
        if name != "records.layout" && standing.get(name) != Some(bytes) {
            return Err(format!(
                "{name} of the predecessor is gone under a pin that is not durable"
            ));
        }
    }
    if result.is_ok() || again.is_ok() {
        return Err(
            "a rewrite or its recovery finished under a pin that is not durable".to_owned(),
        );
    }
    // The power cut: the disk kept the pin as it was before the rename.
    fsx::write_atomic(
        &fx.entry.with_file_name("records.layout"),
        &fx.before["records.layout"],
        0o600,
    )
    .unwrap();
    if selects(&fx.entry) != Ok((fx.generation, fx.old.to_vec())) {
        return Err("after the power cut no reader selects the predecessor whole".to_owned());
    }
    recover(&fx.entry).map_err(|error| format!("recovery is refused: {error}"))?;
    let after = tree(fx.root.path());
    if after != fx.before {
        return Err(format!(
            "after the power cut the entry is not exactly the predecessor: {}",
            differs(&fx.before, &after)
        ));
    }
    Ok(())
}

/// No file of a predecessor is unlinked under a pin that may not be durable. A rewrite whose
/// flush of the pin's rename fails answers an error and leaves every file of the predecessor;
/// so does recovery while the flush still fails; a power cut that then takes the rename back
/// leaves exactly the predecessor; and once the flush works recovery finishes the rewrite.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::RetireUnflushed` (recovery goes forward though the
/// flush failed, as it did before this law), seen to fail with ``records.head of the
/// predecessor is gone under a pin that is not durable`` (asserted below).
#[test]
fn vault_checkpoint_the_predecessor_stands_until_the_pin_is_durable() {
    held(|| {
        let reference = Reference::new();
        for before in [Before::Layout, Before::Rewritten] {
            pin_unflushed(before, None).unwrap_or_else(|error| panic!("{before:?}: {error}"));

            let fx = fixture(before);
            CUT.set(Some(Cut::Unflushed));
            rewrite(&fx.entry, Predecessor::of(fx.old), WHAT, &|out| {
                out.write_all(NEW)
            })
            .unwrap_err();
            CUT.set(None);
            assert_eq!(selects(&fx.entry), Ok((fx.generation + 1, NEW.to_vec())));
            recover(&fx.entry).unwrap();
            assert_eq!(tree(fx.root.path()), reference.of(&fx), "{before:?}");
        }
        assert_eq!(
            pin_unflushed(Before::Rewritten, Some(Mutant::RetireUnflushed)).unwrap_err(),
            "records.head of the predecessor is gone under a pin that is not durable"
        );
    });
}

/// Recovery after the pin, run as `mutant`, of an entry never rewritten before whose
/// predecessor's own header `damage` changed since the crash: an `Err` says how what stands is
/// not exactly the new data.
fn recovers_without(
    reference: &Reference,
    damage: impl FnOnce(&Path),
    mutant: Option<Mutant>,
) -> std::result::Result<(), String> {
    let fx = cut_after(Before::Layout, Step::Pinned, mutant);
    damage(&fx.entry.join("layout"));
    run_as(mutant, || recover(&fx.entry))
        .map_err(|error| format!("recovery is refused: {error}"))?;
    let after = tree(fx.root.path());
    let want = reference.of(&fx);
    if after != want {
        return Err(format!(
            "after recovery the entry is not exactly the new data: {}",
            unnamed(&differs(&want, &after), &after)
        ));
    }
    Ok(())
}

/// The unlinks and flushes of an uninterrupted rewrite of an entry never rewritten, run as
/// `mutant`, from its pin on, the kept original's nonce written `<nonce>`.
fn retire_trace(mutant: Option<Mutant>) -> String {
    let fx = fixture(Before::Layout);
    TRACE.with_borrow_mut(Vec::clear);
    run_as(mutant, || {
        Context::new(&fx.entry)
            .unwrap()
            .rewrite_with(
                Predecessor::of(OLD),
                WHAT,
                &|out| out.write_all(NEW),
                &|_| Ok(()),
            )
            .unwrap();
    });
    unnamed(&TRACE.with_borrow(|trace| trace.join(", ")), &fx.before)
}

/// Recovery after the pin needs no file of the predecessor to read or to stand: the header the
/// pin names carries the name of the original records file kept beside the predecessor, so
/// recovery finishes, and unlinks that original, where the predecessor's own header is cut
/// short and where it is already gone. And the predecessor's files are unlinked in an order a
/// power cut cannot turn into a head or an original that outlives them: those two first, and
/// the parent flushed, before the data and the header.
///
/// NEGATIVE CONTROLS (2026-11-17), each asserted below:
/// - `Mutant::KeptUnnamed` (the name is read from the predecessor's header, as it was before
///   this law): with that header cut short, ``recovery is refused: membership records storage:
///   invalid layout header``; with it gone, ``after recovery the entry is not exactly the new
///   data: .aterm-records-<nonce> is left``.
/// - `Mutant::RetireUnordered` (no flush between the two groups): the trace reads ``unlink
///   records.head, unlink .aterm-records-<nonce>, unlink current, unlink layout, flush layout,
///   unlink records.preparing, flush parent, unlink records.rewriting``.
#[test]
fn vault_checkpoint_recovery_needs_no_file_of_the_predecessor() {
    held(|| {
        let reference = Reference::new();
        let gone = |header: &Path| std::fs::remove_file(header).unwrap();
        recovers_without(&reference, halve, None).unwrap();
        recovers_without(&reference, gone, None).unwrap();
        assert_eq!(
            recovers_without(&reference, halve, Some(Mutant::KeptUnnamed)).unwrap_err(),
            "recovery is refused: membership records storage: invalid layout header"
        );
        assert_eq!(
            recovers_without(&reference, gone, Some(Mutant::KeptUnnamed)).unwrap_err(),
            "after recovery the entry is not exactly the new data: .aterm-records-<nonce> is left"
        );

        assert_eq!(
            retire_trace(None),
            "unlink records.head, unlink .aterm-records-<nonce>, flush parent, unlink current, \
             unlink layout, flush layout, unlink records.preparing, flush parent, \
             unlink records.rewriting"
        );
        assert_eq!(
            retire_trace(Some(Mutant::RetireUnordered)),
            "unlink records.head, unlink .aterm-records-<nonce>, unlink current, unlink layout, \
             flush layout, unlink records.preparing, flush parent, unlink records.rewriting"
        );
    });
}

/// Recovery, run as `mutant`, of a rewrite of `before`'s fixture cut once its pin moved, whose
/// new data `damage` changed since: an `Err` says how recovery took data that is not what its
/// header declares, or removed something.
fn refuses_new_data(
    before: Before,
    damage: impl FnOnce(&Path),
    mutant: Option<Mutant>,
) -> std::result::Result<(), String> {
    let fx = cut_after(before, Step::Pinned, None);
    damage(&data_of(&fx));
    let standing = tree(fx.root.path());
    let recovered = run_as(mutant, || recover(&fx.entry));
    let after = tree(fx.root.path());
    match recovered {
        Ok(()) => Err(format!(
            "recovery took data that is not what its header declares: {}",
            differs(&standing, &after)
        )),
        Err(error) if after != standing => Err(format!(
            "recovery was refused ({error}) and changed the entry: {}",
            differs(&standing, &after)
        )),
        Err(error) => {
            assert_eq!(
                error.to_string(),
                "membership records storage: the rewritten data is not what its header declares; its predecessor is kept"
            );
            Ok(())
        }
    }
}

/// The predecessor goes only once the new data reads, from the disk, as its header declares:
/// recovery after the pin is refused, and removes nothing, where the pinned data file is cut
/// short or holds another byte, for an entry never rewritten and for one rewritten before. A
/// record appended to it since is past what the header declares and is kept; and once the
/// predecessor's data is gone there is nothing left to keep, so a data file a later writer
/// replaced whole is left as that writer wrote it.
///
/// NEGATIVE CONTROL (2026-11-17), for nothing being removed: with recovery removing the files
/// of the generation after the pinned one before it read the new data, as it first did, seen
/// to fail with ``Layout: recovery was refused (membership records storage: the rewritten data
/// is not what its header declares; its predecessor is kept) and changed the entry:
/// records/current.2 is gone``.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::ForwardUnread` (recovery unlinks the predecessor
/// unread, as it did before this law), seen to fail with ``recovery took data that is not what
/// its header declares: records.head is gone, records.rewriting is gone, records/current.1 is
/// gone, records/layout.1 is gone`` (asserted below).
#[test]
fn vault_checkpoint_recovery_reads_the_new_data_before_the_predecessor_goes() {
    held(|| {
        let reference = Reference::new();
        let flip = |data: &Path| {
            let mut bytes = std::fs::read(data).unwrap();
            bytes[3] ^= 1;
            std::fs::write(data, bytes).unwrap();
        };
        for before in [Before::Layout, Before::Rewritten] {
            refuses_new_data(before, halve, None)
                .unwrap_or_else(|error| panic!("{before:?}: {error}"));
            refuses_new_data(before, flip, None)
                .unwrap_or_else(|error| panic!("{before:?}: {error}"));
            // Nothing is removed: not even a file at the generation after the pinned one, which
            // a recovery that is not refused removes.
            let planted = |data: &Path| {
                halve(data);
                let after = if before == Before::Layout { 2 } else { 3 };
                fsx::write_atomic(&data.with_file_name(data_name(after)), b"planted", 0o600)
                    .unwrap();
            };
            refuses_new_data(before, planted, None)
                .unwrap_or_else(|error| panic!("{before:?}: {error}"));

            // A record appended since the crash is kept.
            let fx = cut_after(before, Step::Pinned, None);
            let whole = [NEW, APPENDED].concat();
            std::fs::write(data_of(&fx), &whole).unwrap();
            recover(&fx.entry).unwrap();
            let mut want = reference.of(&fx);
            let name = format!("records/{}", data_name(fx.generation + 1));
            want.insert(name.clone(), whole);
            assert_eq!(tree(fx.root.path()), want, "{before:?}");

            // The predecessor already gone: a later writer's whole file stands.
            let fx = cut_after(before, Step::Exchanged, None);
            std::fs::write(data_of(&fx), b"a later store").unwrap();
            recover(&fx.entry).unwrap();
            let mut want = reference.of(&fx);
            want.insert(name, b"a later store".to_vec());
            assert_eq!(tree(fx.root.path()), want, "{before:?}");
        }
        assert_eq!(
            refuses_new_data(Before::Rewritten, halve, Some(Mutant::ForwardUnread)).unwrap_err(),
            "recovery took data that is not what its header declares: records.head is gone, \
             records.rewriting is gone, records/current.1 is gone, records/layout.1 is gone"
        );
    });
}

/// The data file of the generation the rewrite under test makes.
fn data_of(fx: &Fixture) -> PathBuf {
    fx.entry.join(data_name(fx.generation + 1))
}

/// Recovery, run as `mutant`, of a rewrite cut once it had unlinked the predecessor, beside
/// which a reader has since written the head of the new data: an `Err` says what recovery
/// changed beside removing the intent.
fn keeps_new_head(before: Before, mutant: Option<Mutant>) -> std::result::Result<(), String> {
    let fx = cut_after(before, Step::Exchanged, None);
    fsx::write_atomic(
        &storage::head_path(&fx.entry).unwrap(),
        b"a head of the new data",
        0o600,
    )
    .unwrap();
    let mut want = tree(fx.root.path());
    assert!(want.remove("records.rewriting").is_some());
    run_as(mutant, || recover(&fx.entry))
        .map_err(|error| format!("recovery is refused: {error}"))?;
    let after = tree(fx.root.path());
    if after != want {
        return Err(differs(&want, &after));
    }
    // An intent that names the pinned generation itself, beside a settled entry.
    fsx::write_atomic(
        &fx.entry.with_file_name("records.rewriting"),
        &std::fs::read(fx.entry.join(header_name(fx.generation + 1))).unwrap(),
        0o600,
    )
    .unwrap();
    run_as(mutant, || recover(&fx.entry))
        .map_err(|error| format!("recovery is refused: {error}"))?;
    let after = tree(fx.root.path());
    if after != want {
        return Err(differs(&want, &after));
    }
    Ok(())
}

/// Recovery removes the head only while a file of the predecessor stands: the head a reader
/// wrote for the new data, once the rewrite had unlinked the predecessor and before its intent
/// was removed, is kept, as it is beside a settled entry whose intent names the pinned
/// generation itself. (That a predecessor's head goes with it, and that a head stands through
/// a rewrite undone before its pin, the crash matrix holds: its fixtures have a head.)
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::HeadAlways` (the head removed whenever recovery
/// goes forward, as it was wherever the intent named the pinned generation before this law),
/// seen to fail with ``records.head is gone`` (asserted below).
#[test]
fn vault_checkpoint_recovery_keeps_the_head_of_the_new_data() {
    held(|| {
        for before in [Before::Layout, Before::Rewritten] {
            keeps_new_head(before, None).unwrap_or_else(|error| panic!("{before:?}: {error}"));
        }
        assert_eq!(
            keeps_new_head(Before::Rewritten, Some(Mutant::HeadAlways)).unwrap_err(),
            "records.head is gone"
        );
    });
}

/// An entry whose intent's name, or whose next generation's data name, holds what `plant`
/// puts there, recovered and then rewritten as `mutant`: an `Err` says how either was not
/// refused by name, or what either changed.
fn refuses_what_is_no_file(
    plant: impl FnOnce(&Fixture),
    mutant: Option<Mutant>,
) -> std::result::Result<(), String> {
    const NAMED: &str =
        "membership records storage: something that is no file stands at a name a rewrite writes";
    let fx = fixture(Before::Layout);
    plant(&fx);
    let standing = tree(fx.root.path());
    let acts: [(&str, Box<dyn Fn() -> Result<()>>); 2] = [
        ("recovery", Box::new(|| recover(&fx.entry))),
        (
            "a rewrite",
            Box::new(|| {
                rewrite(&fx.entry, Predecessor::of(OLD), WHAT, &|out| {
                    out.write_all(NEW)
                })
                .map(|_| ())
            }),
        ),
    ];
    for (name, act) in acts {
        let done = run_as(mutant, act);
        let after = tree(fx.root.path());
        if after != standing {
            return Err(format!(
                "{name} changed the entry: {}",
                differs(&standing, &after)
            ));
        }
        match done {
            Err(error) if error.to_string() == NAMED => {}
            // Recovery has nothing to do where no intent stands.
            Ok(())
                if name == "recovery"
                    && !standing.contains_key("records.rewriting/")
                    && !standing.contains_key("records.rewriting") => {}
            Ok(()) => return Err(format!("{name} is taken")),
            Err(_) => return Err(format!("{name} is refused, not by name")),
        }
    }
    if selects(&fx.entry) != Ok((0, OLD.to_vec())) {
        return Err("no reader selects the predecessor whole".to_owned());
    }
    Ok(())
}

/// An intent no rewrite of this entry declared, and names that hold what is no file. An
/// intent beside a records file kept whole, which has no pin, declares nothing: recovery
/// removes it and nothing else, and the file then takes its rewrite. A folder at the intent's
/// name, at the next generation's data name, or at the pin's scratch name beside the entry, is
/// not a rewrite's to remove: recovery and a rewrite are refused by name before either changes
/// anything, the rewrite leaving no intent of its own, and every reader still selects the
/// predecessor.
///
/// NEGATIVE CONTROLS (2026-11-17), each asserted below:
/// - `Mutant::StaleIntentKept` (the intent held to a pin, as before this law): ``membership
///   records storage: a layout file is missing``.
/// - `Mutant::Unlooked` (nothing looked at, as before this law): with a folder at the intent's
///   name and a file at the next generation's, ``recovery changed the entry:
///   records/current.1 is gone``; with a folder at the next generation's data name, ``a
///   rewrite changed the entry: records.rewriting is left``.
/// - The pin's scratch (`records.layout.building`) not looked at, as the rewrite and its
///   recovery were before the slice B review, each seen to fail so and then asserted of
///   `Mutant::Unlooked`: with a folder there, ``a rewrite changed the entry: records.rewriting
///   is left`` (it declared its intent, wrote its data and header, and then could not write its
///   pin); with one beside a rewrite cut short once its header stood, ``recovery changed the
///   entry: records/current.1 is gone, records/layout.1 is gone`` (it removed what the rewrite
///   wrote and then could not unlink the folder, so its intent stood for good).
#[test]
fn vault_checkpoint_an_intent_no_rewrite_declared_is_no_wedge() {
    held(|| {
        let stale = |mutant| {
            let root = aterm_tempfile::tempdir().unwrap();
            let entry = root.path().join("records");
            fsx::write_atomic(&entry, OLD, 0o600).unwrap();
            let before = tree(root.path());
            for intent in [
                &b"not an intent"[..],
                &cut_after(Before::Layout, Step::Intent, None).before["records/layout"],
            ] {
                fsx::write_atomic(&entry.with_file_name("records.rewriting"), intent, 0o600)
                    .unwrap();
                run_as(mutant, || recover(&entry)).map_err(|error| error.to_string())?;
                assert_eq!(tree(root.path()), before);
            }
            fsx::write_atomic(&entry.with_file_name("records.rewriting"), b"stale", 0o600).unwrap();
            let done = rewrite(&entry, Predecessor::of(OLD), WHAT, &|out| {
                out.write_all(NEW)
            })
            .map_err(|error| error.to_string())?;
            assert_eq!(done.generation, 1);
            assert_eq!(selects(&entry), Ok((1, NEW.to_vec())));
            Ok::<(), String>(())
        };
        stale(None).unwrap();
        assert_eq!(
            stale(Some(Mutant::StaleIntentKept)).unwrap_err(),
            "membership records storage: a layout file is missing"
        );

        let intent_folder = |fx: &Fixture| {
            std::fs::create_dir(fx.entry.with_file_name("records.rewriting")).unwrap();
            fsx::write_atomic(&fx.entry.join("current.1"), b"planted", 0o600).unwrap();
        };
        let data_folder = |fx: &Fixture| std::fs::create_dir(fx.entry.join("current.1")).unwrap();
        let scratch_folder =
            |fx: &Fixture| std::fs::create_dir(fx.entry.join("layout.1.building")).unwrap();
        // The pin's scratch, beside the entry: with no intent standing a rewrite meets it, and
        // with one (a rewrite cut short once its data and header stood) recovery does.
        let pin_scratch = |fx: &Fixture| {
            std::fs::create_dir(fx.entry.with_file_name("records.layout.building")).unwrap();
        };
        let pin_scratch_cut = |fx: &Fixture| {
            Context::new(&fx.entry)
                .unwrap()
                .rewrite_with(
                    Predecessor::of(OLD),
                    WHAT,
                    &|out| out.write_all(NEW),
                    &|step| match step {
                        Step::Prepared => Err(refused("a test's crash came after a step")),
                        _ => Ok(()),
                    },
                )
                .unwrap_err();
            pin_scratch(fx);
        };
        refuses_what_is_no_file(intent_folder, None).unwrap();
        refuses_what_is_no_file(data_folder, None).unwrap();
        refuses_what_is_no_file(scratch_folder, None).unwrap();
        refuses_what_is_no_file(pin_scratch, None).unwrap();
        refuses_what_is_no_file(pin_scratch_cut, None).unwrap();
        assert_eq!(
            refuses_what_is_no_file(intent_folder, Some(Mutant::Unlooked)).unwrap_err(),
            "recovery changed the entry: records/current.1 is gone"
        );
        assert_eq!(
            refuses_what_is_no_file(data_folder, Some(Mutant::Unlooked)).unwrap_err(),
            "a rewrite changed the entry: records.rewriting is left"
        );
        assert_eq!(
            refuses_what_is_no_file(pin_scratch, Some(Mutant::Unlooked)).unwrap_err(),
            "a rewrite changed the entry: records.rewriting is left"
        );
        assert_eq!(
            refuses_what_is_no_file(pin_scratch_cut, Some(Mutant::Unlooked)).unwrap_err(),
            "recovery changed the entry: records/current.1 is gone, records/layout.1 is gone"
        );
    });
}

/// A rewrite of a records file kept whole, run as `mutant`, that is refused for `from` or for
/// what `produce` measures: an `Err` says what it changed.
fn leaves_a_file(
    from: &[u8],
    produce: Produce<'_>,
    mutant: Option<Mutant>,
) -> std::result::Result<(), String> {
    leaves_a_file_beside(&|_| {}, from, produce, mutant)
}

/// [`leaves_a_file`] where `plant` put something beside the file first.
fn leaves_a_file_beside(
    plant: &dyn Fn(&Path),
    from: &[u8],
    produce: Produce<'_>,
    mutant: Option<Mutant>,
) -> std::result::Result<(), String> {
    let root = aterm_tempfile::tempdir().unwrap();
    let entry = root.path().join("records");
    fsx::write_atomic(&entry, OLD, 0o600).unwrap();
    plant(&entry);
    let before = tree(root.path());
    let done = run_as(mutant, || {
        rewrite(&entry, Predecessor::of(from), WHAT, produce)
    });
    assert!(done.is_err(), "the rewrite is refused");
    let after = tree(root.path());
    if after != before {
        return Err(unnamed(&differs(&before, &after), &after));
    }
    Ok(())
}

/// A rewrite of a records file kept whole that is refused, because it was computed from other
/// data than the file holds or would write more than a format-2 layout holds, leaves the file
/// a file: no layout, no pin, no intent.
///
/// So does one refused because a folder stands at the pin's scratch name beside the file: it
/// is refused by that look before the file becomes a layout, whose own pin the first
/// transition could not then write.
///
/// NEGATIVE CONTROLS (2026-11-17), each asserted below:
/// - `Mutant::PrepareFirst` (the file made a layout before its rewrite is held to it, as it
///   was before this law), seen to fail with ``records is gone, .aterm-records-<nonce> is left,
///   records.layout is left, records.preparing is left, records/ is left, records/current is
///   left, records/layout is left``.
/// - `Mutant::Unlooked` with the folder at the pin's scratch: ``records is gone,
///   .aterm-records-<nonce> is left, records.preparing is left, records/ is left,
///   records/current is left, records/layout is left`` (the first transition was begun and
///   could not pin).
#[test]
fn vault_checkpoint_a_refused_rewrite_leaves_a_records_file_a_file() {
    held(|| {
        let small: Produce<'_> = &|out| out.write_all(NEW);
        let large: Produce<'_> = &|out| {
            let chunk = vec![0u8; 1 << 20];
            for _ in 0..=capacity(FORMAT_TWO) >> 20 {
                out.write_all(&chunk)?;
            }
            Ok(())
        };
        leaves_a_file(b"other history", small, None).unwrap();
        leaves_a_file(OLD, large, None).unwrap();
        let pin_scratch: &dyn Fn(&Path) = &|entry| {
            std::fs::create_dir(entry.with_file_name("records.layout.building")).unwrap();
        };
        leaves_a_file_beside(pin_scratch, OLD, small, None).unwrap();
        assert_eq!(
            leaves_a_file_beside(pin_scratch, OLD, small, Some(Mutant::Unlooked)).unwrap_err(),
            "records is gone, .aterm-records-<nonce> is left, records.preparing is left, \
             records/ is left, records/current is left, records/layout is left"
        );
        assert_eq!(
            leaves_a_file(b"other history", small, Some(Mutant::PrepareFirst)).unwrap_err(),
            "records is gone, .aterm-records-<nonce> is left, records.layout is left, \
             records.preparing is left, records/ is left, records/current is left, \
             records/layout is left"
        );
    });
}

/// A layout pinned at the last generation a pin can name: recovery beside an intent removes
/// the intent alone, and a rewrite is refused, the generation spent, with nothing changed.
/// Neither adds one to it.
///
/// NEGATIVE CONTROL (2026-11-17): with the unchecked `+ 1`s these replaced put back, seen to
/// fail with ``panicked at crates/aterm-secrets/src/records_storage/rewrite.rs:670:41: attempt
/// to add with overflow``, in the rewrite.
#[test]
fn vault_checkpoint_the_last_generation_is_not_counted_past() {
    held(|| {
        let fx = fixture(Before::Rewritten);
        let (root, entry) = (fx.root.path(), fx.entry.as_path());
        let last = Generation {
            number: u64::MAX,
            kept: None,
            ..Generation::decode(&fx.before["records/layout.1"]).unwrap()
        };
        fsx::write_atomic(&entry.join(data_name(u64::MAX)), FIRST, 0o600).unwrap();
        fsx::write_atomic(&entry.join(header_name(u64::MAX)), &last.encode(), 0o600).unwrap();
        fsx::write_atomic(
            &entry.with_file_name("records.layout"),
            &last.pin(HELD),
            0o600,
        )
        .unwrap();
        let standing = tree(root);
        assert_eq!(generation(entry).unwrap().number, u64::MAX);
        assert_eq!(selects(entry), Ok((u64::MAX, FIRST.to_vec())));
        assert_eq!(
            rewrite(entry, Predecessor::of(FIRST), WHAT, &|out| out
                .write_all(NEW))
            .unwrap_err()
            .to_string(),
            "membership records storage: the rewrite generation is spent"
        );
        assert_eq!(tree(root), standing);
        fsx::write_atomic(
            &entry.with_file_name("records.rewriting"),
            &last.encode(),
            0o600,
        )
        .unwrap();
        recover(entry).unwrap();
        // The generation before it is nothing this layout holds; the fixture's own is left.
        assert_eq!(tree(root), standing);
    });
}

/// The generation of the entry of a fixture whose rewrite under test wrote `new`, read as
/// `mutant`.
fn generation_after(new: &'static [u8], mutant: Option<Mutant>) -> (RecordsGeneration, Vec<u8>) {
    let fx = fixture(Before::Layout);
    assert_eq!(generation(&fx.entry).unwrap(), RecordsGeneration::default());
    rewrite(&fx.entry, Predecessor::of(OLD), WHAT, &|out| {
        out.write_all(new)
    })
    .unwrap();
    let header = std::fs::read(fx.entry.join("layout.1")).unwrap();
    (run_as(mutant, || generation(&fx.entry)).unwrap(), header)
}

/// A generation names the rewrite that made it, not only its number ([`RecordsGeneration`]):
/// an entry never rewritten is at 0 with none named, a rewritten one at its number with the
/// SHA-256 of the header its pin names, and two first rewrites of entries that held the same
/// records, which wrote other data, are two generations of one number. So a place kept under
/// one is not read under the other: a rewrite made again after a power cut took its pin back,
/// and an entry removed and made again, reach a number a second time.
///
/// NEGATIVE CONTROL (2026-11-17): `Mutant::NumberAlone` (the generation its number alone, as
/// it was before the slice B review), seen to fail with ``assertion `left != right` failed``,
/// both sides `RecordsGeneration { number: 1, rewrite: None }` (asserted below).
#[test]
fn vault_checkpoint_a_generation_names_its_rewrite() {
    held(|| {
        let differ = |mutant| {
            let (one, header) = generation_after(NEW, mutant);
            let (other, _) = generation_after(FIRST, mutant);
            assert_eq!((one.number, other.number), (1, 1));
            if mutant.is_none() {
                assert_eq!(one.rewrite, Some(Sha256::digest(&header)));
            }
            (one, other)
        };
        let (one, other) = differ(None);
        assert_ne!(one, other);
        let (one, other) = differ(Some(Mutant::NumberAlone));
        assert_eq!(one, other);
        assert_eq!(one.rewrite, None);
    });
}

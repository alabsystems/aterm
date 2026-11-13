// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

use super::*;

/// HIToolbox as a list and a selection; each call is logged, and `refuse`
/// names a call that fails.
#[derive(Default)]
struct Fake {
    selected: RefCell<String>,
    enabled: RefCell<Vec<String>>,
    log: RefCell<Vec<String>>,
    refuse: RefCell<Option<String>>,
    /// The record file a call reads as it runs, and what it held then.
    watch: Option<PathBuf>,
    seen: RefCell<Vec<Option<String>>>,
    /// HIToolbox's own bookkeeping: a selection writes the history.
    prefs: RefCell<Vec<Pref>>,
}

const HISTORY: &str = "AppleInputSourceHistory";

impl Fake {
    fn new(selected: &str, enabled: &[&str]) -> Fake {
        Fake {
            selected: RefCell::new(selected.into()),
            enabled: RefCell::new(enabled.iter().map(|s| (*s).to_string()).collect()),
            prefs: RefCell::new(vec![(HISTORY.into(), None)]),
            ..Fake::default()
        }
    }
    fn call(&self, what: String) -> Result<(), String> {
        if let Some(p) = &self.watch {
            self.seen.borrow_mut().push(std::fs::read_to_string(p).ok());
        }
        let refused = self.refuse.borrow().as_deref() == Some(what.as_str());
        self.log.borrow_mut().push(what.clone());
        if refused {
            Err(format!("{what}: OSStatus -50"))
        } else {
            Ok(())
        }
    }
}

impl Sources for Fake {
    fn selected(&self) -> Option<String> {
        Some(self.selected.borrow().clone())
    }
    fn enabled(&self) -> Vec<String> {
        self.enabled.borrow().clone()
    }
    fn enable(&self, id: &str) -> Result<(), String> {
        self.call(format!("enable {id}"))?;
        self.enabled.borrow_mut().push(id.into());
        Ok(())
    }
    fn disable(&self, id: &str) -> Result<(), String> {
        self.call(format!("disable {id}"))?;
        self.enabled.borrow_mut().retain(|e| e != id);
        Ok(())
    }
    fn select(&self, id: &str) -> Result<(), String> {
        self.call(format!("select {id}"))?;
        *self.selected.borrow_mut() = id.into();
        *self.prefs.borrow_mut() = vec![(HISTORY.into(), Some(id.as_bytes().to_vec()))];
        Ok(())
    }
    fn bookkeeping(&self) -> Result<Vec<Pref>, String> {
        Ok(self.prefs.borrow().clone())
    }
    fn put_bookkeeping(&self, prefs: &[Pref]) -> Result<(), String> {
        self.call("put bookkeeping".into())?;
        *self.prefs.borrow_mut() = prefs.to_vec();
        Ok(())
    }
}

const US: &str = "com.apple.keylayout.US";
const DE: &str = "com.apple.keylayout.German";
const FR: &str = "com.apple.keylayout.French";
const PALETTE: &str = "com.apple.CharacterPaletteIM";

fn scratch(name: &str) -> (crate::TestScratch, PathBuf) {
    let dir = crate::TestScratch::new(name);
    let record = dir.join("record");
    (dir, record)
}

/// A run puts back the selection and disables what it enabled, and nothing
/// else: a source enabled before the run (the palette) stays, in its place.
#[test]
fn a_run_puts_back_the_selection_and_disables_only_what_it_enabled() {
    let (_dir, path) = scratch("put-back");
    let hit = Fake::new(US, &[US, PALETTE]);
    {
        let mut run = Switch::begin(&hit, &path).expect("begin");
        run.select(DE).expect("German");
        run.select(US).expect("U.S.");
        run.select(FR).expect("French");
        assert_eq!(hit.selected().as_deref(), Some(FR));
        run.restore().expect("restore");
    }
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US, PALETTE]);
    assert!(!path.exists(), "the record goes once the desk is put back");
    assert_eq!(
        hit.log.borrow().as_slice(),
        [
            format!("enable {DE}"),
            format!("select {DE}"),
            format!("select {US}"),
            format!("enable {FR}"),
            format!("select {FR}"),
            format!("select {US}"),
            format!("disable {FR}"),
            format!("disable {DE}"),
            "put bookkeeping".to_string(),
        ]
    );
    assert_eq!(
        hit.bookkeeping().expect("prefs"),
        [(HISTORY.to_string(), None)],
        "the history HIToolbox wrote is put back as it was: absent"
    );
}

/// An early return, an error or an interrupt drops the switch, and the drop
/// puts the desk back.
#[test]
fn dropping_the_switch_puts_the_desk_back() {
    let (_dir, path) = scratch("drop");
    let hit = Fake::new(US, &[US, PALETTE]);
    let run = || -> Result<(), String> {
        let mut s = Switch::begin(&hit, &path)?;
        s.select(DE)?;
        Err("the lane stopped".into())
    };
    assert!(run().is_err());
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US, PALETTE]);
    assert!(!path.exists());
}

/// The record names a source BEFORE it is enabled, so a run killed between
/// the two still leaves a record that disables it.
#[test]
fn a_source_is_recorded_before_it_is_enabled() {
    let (_dir, path) = scratch("before");
    let hit = Fake {
        watch: Some(path.clone()),
        ..Fake::new(US, &[US])
    };
    let mut run = Switch::begin(&hit, &path).expect("begin");
    run.select(DE).expect("German");
    let seen = hit.seen.borrow().clone();
    let at_enable = seen[0]
        .as_deref()
        .and_then(Record::parse)
        .expect("a record");
    assert_eq!(at_enable.enabled, [DE]);
    assert_eq!(at_enable.selected, US);
    run.restore().expect("restore");
}

/// A run that died (its pid gone) left its record: the next run puts that
/// desk back before it starts, and says so.
#[test]
fn a_dead_runs_record_is_put_back_by_the_next_run() {
    let (_dir, path) = scratch("recover");
    let hit = Fake::new(DE, &[US, PALETTE, DE, FR]);
    let dead = Record {
        pid: i32::MAX,
        start: 1,
        selected: US.into(),
        enabled: vec![DE.into(), FR.into()],
        prefs: vec![(HISTORY.into(), None)],
    };
    write(&path, &dead).expect("record");
    let said = recover(&hit, &path).expect("recover").expect("a record");
    assert!(
        said.contains("selected com.apple.keylayout.US again"),
        "{said}"
    );
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US, PALETTE]);
    assert!(!path.exists());
    assert_eq!(recover(&hit, &path).expect("again"), None);
}

/// A live run's record is not touched: that run puts its own desk back.
#[test]
fn a_live_runs_record_is_left_to_it() {
    let (_dir, path) = scratch("live");
    let hit = Fake::new(DE, &[US, DE]);
    let live = Record::mine(US.into(), Vec::new());
    write(
        &path,
        &Record {
            enabled: vec![DE.into()],
            ..live
        },
    )
    .expect("record");
    let e = recover(&hit, &path).expect_err("a live run holds it");
    assert!(e.contains("holds the input sources"), "{e}");
    assert_eq!(hit.selected().as_deref(), Some(DE));
    assert!(path.exists());
    let r = restore_now(&hit, &path)
        .expect("restore")
        .expect("a record");
    assert_eq!(r.enabled, [DE]);
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US]);
    assert!(!path.exists());
}

/// A refused restore keeps the record, so the next run (or `--restore`)
/// tries again; it is never dropped as done.
#[test]
fn a_refused_restore_keeps_the_record() {
    let (_dir, path) = scratch("refused");
    let hit = Fake::new(US, &[US]);
    let mut run = Switch::begin(&hit, &path).expect("begin");
    run.select(DE).expect("German");
    *hit.refuse.borrow_mut() = Some(format!("select {US}"));
    assert!(run.restore().is_err());
    assert!(path.exists());
    *hit.refuse.borrow_mut() = None;
    drop(run);
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US]);
    assert!(!path.exists());
}

/// A lane over the fake desk: each idle wait logs what is selected and
/// enabled while it waits, each layout's rows log their layout, and a person
/// touches the desk during the rows of `touched_in`.
struct Desk<'a> {
    hit: &'a Fake,
    /// The answers of successive idle waits (idle once they run out).
    idle: std::collections::VecDeque<Result<(), String>>,
    touched_in: Option<&'static str>,
    person: bool,
    said: Vec<String>,
}

impl<'a> Desk<'a> {
    fn new(hit: &'a Fake) -> Desk<'a> {
        Desk {
            hit,
            idle: std::collections::VecDeque::new(),
            touched_in: None,
            person: false,
            said: Vec::new(),
        }
    }
}

impl Lane for Desk<'_> {
    fn await_idle(&mut self) -> Result<Result<(), String>, Stopped> {
        let line = format!(
            "idle wait: {} selected, {} enabled",
            self.hit.selected().unwrap_or_default(),
            self.hit.enabled().join(" ")
        );
        self.hit.log.borrow_mut().push(line);
        Ok(self.idle.pop_front().unwrap_or(Ok(())))
    }
    fn measure(&mut self, id: &str) -> Result<(), Stopped> {
        self.hit.log.borrow_mut().push(format!("rows {id}"));
        self.person |= self.touched_in == Some(id);
        Ok(())
    }
    fn person(&self) -> bool {
        self.person
    }
    fn skipped(&mut self, id: &str, why: &str) {
        self.said.push(format!("skipped {id}: {why}"));
    }
    fn refused(&mut self, id: &str, what: &str, why: &str) {
        self.said.push(format!("refused {id} {what}: {why}"));
    }
}

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_string()).collect()
}

/// Each layout is selected only for its own rows: the person's own layout is
/// back, and nothing the run enabled is left enabled, whenever the lane waits
/// for the desk again.
#[test]
fn each_layout_is_put_back_before_the_lane_waits_again() {
    let (_dir, path) = scratch("in-turn");
    let hit = Fake::new(US, &[US, PALETTE]);
    let mut desk = Desk::new(&hit);
    in_turn(&hit, &path, &ids(&[US, DE, FR]), &mut desk).expect("not interrupted");
    let idle = format!("idle wait: {US} selected, {US} {PALETTE} enabled");
    let put = "put bookkeeping".to_string();
    assert_eq!(
        hit.log.borrow().as_slice(),
        [
            idle.clone(),
            format!("select {US}"),
            format!("rows {US}"),
            put.clone(),
            idle.clone(),
            format!("enable {DE}"),
            format!("select {DE}"),
            format!("rows {DE}"),
            format!("select {US}"),
            format!("disable {DE}"),
            put.clone(),
            idle,
            format!("enable {FR}"),
            format!("select {FR}"),
            format!("rows {FR}"),
            format!("select {US}"),
            format!("disable {FR}"),
            put,
        ]
    );
    assert!(desk.said.is_empty(), "{:?}", desk.said);
    assert!(!path.exists());
}

/// A person who comes back between two layouts finds their own layout while
/// the lane waits for them (never the last one measured), and the layouts
/// after it are skipped, never selected.
#[test]
fn a_busy_desk_between_layouts_finds_the_persons_own_layout() {
    let (_dir, path) = scratch("busy");
    let hit = Fake::new(US, &[US]);
    let mut desk = Desk::new(&hit);
    desk.idle = [Ok(()), Ok(()), Err("a person typed".to_string())].into();
    in_turn(&hit, &path, &ids(&[US, DE, FR]), &mut desk).expect("not interrupted");
    let log = hit.log.borrow().clone();
    let waits: Vec<&String> = log.iter().filter(|l| l.starts_with("idle")).collect();
    assert_eq!(waits.len(), 3, "{log:?}");
    assert!(
        waits
            .iter()
            .all(|w| **w == format!("idle wait: {US} selected, {US} enabled")),
        "{log:?}"
    );
    assert!(
        !log.iter().any(|l| l.contains(FR)),
        "French is never touched: {log:?}"
    );
    assert_eq!(desk.said, [format!("skipped {FR}: a person typed")]);
    assert_eq!(hit.selected().as_deref(), Some(US));
    assert_eq!(hit.enabled(), [US]);
}

/// A touch during a layout's rows puts the person's layout back at once, and
/// no later layout is selected.
#[test]
fn a_touch_puts_the_persons_layout_back_and_ends_the_layouts() {
    let (_dir, path) = scratch("touch");
    let hit = Fake::new(US, &[US]);
    let mut desk = Desk::new(&hit);
    desk.touched_in = Some(DE);
    in_turn(&hit, &path, &ids(&[DE, FR]), &mut desk).expect("not interrupted");
    let log = hit.log.borrow().clone();
    let rows = log
        .iter()
        .position(|l| *l == format!("rows {DE}"))
        .expect("German's rows");
    assert_eq!(
        log[rows + 1..],
        [
            format!("select {US}"),
            format!("disable {DE}"),
            "put bookkeeping".to_string()
        ],
        "{log:?}"
    );
    assert_eq!(
        desk.said,
        [format!("skipped {FR}: a person is at the desk")]
    );
}

/// One run holds the record at a time: a run that finds another's record (a
/// live run's, which passed its start-up check together with this one) takes
/// nothing over, switches nothing and leaves that record as it was.
#[test]
fn a_second_run_cannot_take_over_the_record() {
    let (_dir, path) = scratch("second");
    let hit = Fake::new(DE, &[US, DE]);
    let first = Record {
        enabled: vec![DE.into()],
        ..Record::mine(US.into(), Vec::new())
    };
    write(&path, &first).expect("the first run's record");
    let e = Switch::begin(&hit, &path).err().expect("refused");
    assert!(e.contains("holds another run's record"), "{e}");
    let mut desk = Desk::new(&hit);
    in_turn(&hit, &path, &ids(&[FR, US]), &mut desk).expect("not interrupted");
    assert!(
        !hit.log.borrow().iter().any(|l| !l.starts_with("idle")),
        "nothing switched: {:?}",
        hit.log.borrow()
    );
    assert_eq!(desk.said.len(), 2, "{:?}", desk.said);
    assert!(
        desk.said[0].starts_with(&format!("refused {FR} recorded: ")),
        "{:?}",
        desk.said
    );
    assert_eq!(
        read(&path).expect("read"),
        Some(first),
        "the first run's record stands"
    );
}

#[test]
fn a_record_reads_back_as_written() {
    let r = Record {
        pid: 42,
        start: 1_792_000_000,
        selected: US.into(),
        enabled: vec![DE.into(), FR.into()],
        prefs: vec![
            (HISTORY.into(), Some(vec![0x62, 0x70, 0x00, 0xff])),
            ("AppleSelectedInputSources".into(), None),
        ],
    };
    assert_eq!(Record::parse(&r.render()), Some(r));
    assert_eq!(
        Record::parse("pid 1\nstart 2\nselected x\npref k 6\n"),
        None,
        "odd hex"
    );
    assert_eq!(Record::parse("pid 1\nselected x\n"), None, "no start");
    assert_eq!(Record::parse("garbage"), None);
}

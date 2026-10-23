// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keyboard input source a `meta-keys --window` run selects, and putting the person's back after it.

use std::path::{Path, PathBuf};

use crate::stop::Stopped;

/// One key of HIToolbox's own bookkeeping and its value (a binary property list), `None` where the key is absent.
pub type Pref = (String, Option<Vec<u8>>);

/// The input sources of the login session, as HIToolbox has them.
pub trait Sources {
    /// The selected keyboard input source's ID.
    fn selected(&self) -> Option<String>;
    /// The enabled input sources' IDs, in HIToolbox's order.
    fn enabled(&self) -> Vec<String>;
    /// Enable an installed source.
    /// # Errors
    /// HIToolbox refused.
    fn enable(&self, id: &str) -> Result<(), String>;
    /// Disable a source.
    /// # Errors
    /// HIToolbox refused.
    fn disable(&self, id: &str) -> Result<(), String>;
    /// Select an enabled source.
    /// # Errors
    /// HIToolbox refused.
    fn select(&self, id: &str) -> Result<(), String>;
    /// What HIToolbox writes of its own when a source is selected (`AppleInputSourceHistory` and its kin, [`crate::sys::tis::BOOKKEEPING`]).
    /// # Errors
    /// It cannot be read.
    fn bookkeeping(&self) -> Result<Vec<Pref>, String>;
    /// Write that bookkeeping back as it was.
    /// # Errors
    /// It cannot be written.
    fn put_bookkeeping(&self, prefs: &[Pref]) -> Result<(), String>;
}

/// The login session's own input sources ([`crate::sys::tis`]).
pub struct HiToolbox;

impl Sources for HiToolbox {
    fn selected(&self) -> Option<String> {
        crate::sys::tis::selected()
    }
    fn enabled(&self) -> Vec<String> {
        crate::sys::tis::enabled()
    }
    fn enable(&self, id: &str) -> Result<(), String> {
        crate::sys::tis::enable(id)
    }
    fn disable(&self, id: &str) -> Result<(), String> {
        crate::sys::tis::disable(id)
    }
    fn select(&self, id: &str) -> Result<(), String> {
        crate::sys::tis::select(id)
    }
    fn bookkeeping(&self) -> Result<Vec<Pref>, String> {
        crate::sys::tis::bookkeeping()
    }
    fn put_bookkeeping(&self, prefs: &[Pref]) -> Result<(), String> {
        crate::sys::tis::put_bookkeeping(prefs)
    }
}

/// Where a run keeps its record: beside the desk lock, so a run from any checkout finds what another left.
#[must_use]
pub fn record_path() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library/Caches/aterm/meta-keys-input-sources")
}

/// What a run changed, written before it changes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The run's pid and its start (seconds since the epoch): a pid alone could be reused by the time a later run reads it.
    pub pid: i32,
    pub start: i64,
    /// The source selected before the run.
    pub selected: String,
    /// The sources the run enabled (none was enabled before it), in order.
    pub enabled: Vec<String>,
    /// HIToolbox's own bookkeeping before the run, put back after it.
    pub prefs: Vec<Pref>,
}

/// `bytes` as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes `s` spells in hex; `None` when it does not.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

impl Record {
    /// The record's text: one `key value` line each.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = format!(
            "pid {}\nstart {}\nselected {}\n",
            self.pid, self.start, self.selected
        );
        for id in &self.enabled {
            s.push_str(&format!("enabled {id}\n"));
        }
        for (key, value) in &self.prefs {
            let value = value.as_deref().map_or_else(|| "-".to_string(), hex);
            s.push_str(&format!("pref {key} {value}\n"));
        }
        s
    }

    /// The record `text` holds; `None` when it is not one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Record> {
        let (mut pid, mut start, mut selected) = (None, None, None);
        let (mut enabled, mut prefs) = (Vec::new(), Vec::new());
        for line in text.lines() {
            let (k, v) = line.split_once(' ')?;
            match k {
                "pid" => pid = v.parse().ok(),
                "start" => start = v.parse().ok(),
                "selected" => selected = Some(v.to_string()),
                "enabled" => enabled.push(v.to_string()),
                "pref" => {
                    let (key, value) = v.split_once(' ')?;
                    let value = if value == "-" {
                        None
                    } else {
                        Some(unhex(value)?)
                    };
                    prefs.push((key.to_string(), value));
                }
                _ => return None,
            }
        }
        Some(Record {
            pid: pid?,
            start: start?,
            selected: selected?,
            enabled,
            prefs,
        })
    }

    /// This process's record, `selected` selected now and HIToolbox's bookkeeping
    /// `prefs`.
    fn mine(selected: String, prefs: Vec<Pref>) -> Record {
        let pid = i32::try_from(std::process::id()).unwrap_or(0);
        Record {
            pid,
            start: start_s(pid).unwrap_or(0),
            selected,
            enabled: Vec::new(),
            prefs,
        }
    }

    /// Whether the run that wrote it still runs.
    fn live(&self) -> bool {
        start_s(self.pid) == Some(self.start)
    }
}

/// When `pid` started, in unix seconds ([`aterm_uds::process::read_process_birth`]).
fn start_s(pid: i32) -> Option<i64> {
    aterm_uds::process::read_process_birth(pid).and_then(|b| i64::try_from(b.seconds).ok())
}

fn read(path: &Path) -> Result<Option<Record>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Record::parse(&text)
            .map(Some)
            .ok_or_else(|| format!("{} is not a meta-keys record", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write `r` to `path` whole: a temporary file renamed over it.
fn write(path: &Path, r: &Record) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, r.render())
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Create `path` holding `r`, only where no record is: a temporary file linked to
/// the name (`link(2)` fails on an existing one), so the record is never seen
/// half written and a second run cannot take it over.
fn create(path: &Path, r: &Record) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("new.{}", std::process::id()));
    std::fs::write(&tmp, r.render()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "{} holds another run's record: that run puts its input sources back (or \
             `meta-keys --restore` does)",
            path.display()
        )),
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

/// Put back what `r` says a run changed: its selection, then each source it
/// enabled, disabled, then the bookkeeping HIToolbox wrote meanwhile, as it was.
fn put_back(sources: &impl Sources, path: &Path, r: &Record) -> Result<(), String> {
    if sources.selected().as_deref() != Some(r.selected.as_str()) {
        sources.select(&r.selected)?;
    }
    let now = sources.enabled();
    for id in r.enabled.iter().rev() {
        if now.contains(id) {
            sources.disable(id)?;
        }
    }
    if !r.prefs.is_empty() && sources.bookkeeping()? != r.prefs {
        sources.put_bookkeeping(&r.prefs)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// Before a run: put back what an earlier run left in its record, when that run is gone.
/// # Errors
/// The record is unreadable, a run that wrote it still runs, or HIToolbox refused the restore.
pub fn recover(sources: &impl Sources, path: &Path) -> Result<Option<String>, String> {
    let Some(r) = read(path)? else {
        return Ok(None);
    };
    if r.live() {
        return Err(format!(
            "another meta-keys run (pid {}) holds the input sources ({})",
            r.pid,
            path.display()
        ));
    }
    put_back(sources, path, &r)?;
    Ok(Some(format!(
        "an earlier run (pid {}) left its record: selected {} again{}",
        r.pid,
        r.selected,
        if r.enabled.is_empty() {
            String::new()
        } else {
            format!(", disabled {}", r.enabled.join(", "))
        }
    )))
}

/// `meta-keys --restore`: put back what the record names, whoever wrote it (the hard exit of a run that is still alive asks this of a child).
/// # Errors
/// The record is unreadable, or HIToolbox refused the restore.
pub fn restore_now(sources: &impl Sources, path: &Path) -> Result<Option<Record>, String> {
    let Some(r) = read(path)? else {
        return Ok(None);
    };
    put_back(sources, path, &r)?;
    Ok(Some(r))
}

/// A run's hold on the input sources: [`Switch::restore`] (or the drop) puts back what [`Switch::begin`] recorded.
pub struct Switch<'a, S: Sources> {
    sources: &'a S,
    path: PathBuf,
    record: Record,
    /// Put back already (or nothing to put back).
    done: bool,
}

impl<'a, S: Sources> Switch<'a, S> {
    /// Record the selected source in `path`, which no record may hold yet; nothing is changed yet.
    /// # Errors
    /// No source is selected, a record is there already (another run's, or one left to put back), or the record cannot be written.
    pub fn begin(sources: &'a S, path: &Path) -> Result<Switch<'a, S>, String> {
        let selected = sources
            .selected()
            .ok_or("HIToolbox names no selected keyboard input source")?;
        let record = Record::mine(selected, sources.bookkeeping()?);
        create(path, &record)?;
        Ok(Switch {
            sources,
            path: path.to_path_buf(),
            record,
            done: false,
        })
    }

    /// Select `id` for the run, enabling it first when it is not enabled (and recording that before enabling it).
    /// # Errors
    /// The record cannot be written, or HIToolbox refused.
    pub fn select(&mut self, id: &str) -> Result<(), String> {
        if !self.sources.enabled().iter().any(|e| e == id) {
            self.record.enabled.push(id.to_string());
            write(&self.path, &self.record)?;
            self.sources.enable(id)?;
        }
        self.sources.select(id)
    }

    /// Put the person's input sources back.
    /// # Errors
    /// HIToolbox refused; the record stays for the next run.
    pub fn restore(&mut self) -> Result<(), String> {
        if self.done {
            return Ok(());
        }
        put_back(self.sources, &self.path, &self.record)?;
        self.done = true;
        Ok(())
    }
}

/// What a run does with each layout [`in_turn`] gives it.
pub trait Lane {
    /// Wait for an idle desk: `Ok(Err(why))` when it never came.
    /// # Errors
    /// The run was interrupted.
    fn await_idle(&mut self) -> Result<Result<(), String>, Stopped>;
    /// Measure the rows of layout `id`, which is selected now.
    /// # Errors
    /// The run was interrupted.
    fn measure(&mut self, id: &str) -> Result<(), Stopped>;
    /// Whether a person touched the desk: no layout is selected after that.
    fn person(&self) -> bool;
    /// Layout `id` was not measured, and why.
    fn skipped(&mut self, id: &str, why: &str);
    /// The input sources refused `what` for layout `id` (a finding).
    fn refused(&mut self, id: &str, what: &str, why: &str);
}

/// Measure each of `ids` in turn, each from an idle desk and selected only for its own rows: its [`Switch`] puts the person's input sources back as soon as the rows end (or a person touched the desk), before the lane waits for the desk again.
/// # Errors
/// The run was interrupted; the layout's switch puts the desk back as it drops.
pub fn in_turn<S: Sources>(
    sources: &S,
    path: &Path,
    ids: &[String],
    lane: &mut impl Lane,
) -> Result<(), Stopped> {
    let mut stop: Option<String> = None;
    for id in ids {
        if let Some(why) = &stop {
            lane.skipped(id, why);
            continue;
        }
        if lane.person() {
            lane.skipped(id, "a person is at the desk");
            continue;
        }
        if let Err(why) = lane.await_idle()? {
            lane.skipped(id, &why);
            stop = Some(why);
            continue;
        }
        let mut switch = match Switch::begin(sources, path) {
            Ok(switch) => switch,
            Err(e) => {
                lane.refused(id, "recorded", &e);
                stop = Some("the input sources are not this run's to switch".into());
                continue;
            }
        };
        if let Err(e) = switch.select(id) {
            lane.refused(id, "selected", &e);
        } else {
            lane.measure(id)?;
        }
        if let Err(e) = switch.restore() {
            lane.refused(id, "put back", &e);
            stop = Some("the input sources did not come back".into());
        }
    }
    Ok(())
}

impl<S: Sources> Drop for Switch<'_, S> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        match self.restore() {
            Ok(()) => eprintln!(
                "meta-keys: ended early; the input sources are put back ({} selected again{})",
                self.record.selected,
                if self.record.enabled.is_empty() {
                    String::new()
                } else {
                    format!(", {} disabled", self.record.enabled.join(", "))
                }
            ),
            Err(e) => eprintln!(
                "meta-keys: the input sources were not put back ({e}); `meta-keys --restore` \
                 or the next run puts them back from {}",
                self.path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-lab-input-sources-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("record")
    }

    /// A run puts back the selection and disables what it enabled, and nothing
    /// else: a source enabled before the run (the palette) stays, in its place.
    #[test]
    fn a_run_puts_back_the_selection_and_disables_only_what_it_enabled() {
        let path = scratch("put-back");
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
        let path = scratch("drop");
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
        let path = scratch("before");
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
        let path = scratch("recover");
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
        let path = scratch("live");
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
        let path = scratch("refused");
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
        let path = scratch("in-turn");
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
        let path = scratch("busy");
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
        let path = scratch("touch");
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
        let path = scratch("second");
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
}

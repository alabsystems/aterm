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
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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
                "pid" => {
                    pid = v
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| v.parse().ok())
                        .flatten();
                }
                "start" => {
                    start = v
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| v.parse().ok())
                        .flatten();
                }
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
    match crate::util::read_regular_text(path) {
        Ok(text) => Record::parse(&text)
            .map(Some)
            .ok_or_else(|| format!("{} is not a meta-keys record", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_new_temp(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut f = opts.open(tmp)?;
    f.write_all(bytes)
}

/// Write `r` to `path` whole: a temporary file renamed over it.
fn write(path: &Path, r: &Record) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("new");
    write_new_temp(&tmp, r.render().as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
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
    write_new_temp(&tmp, r.render().as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
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
#[path = "input_sources_tests.rs"]
mod tests;

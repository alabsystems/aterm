// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lock file that names its holder, and a wait on it that names whom it waits for.
//!
//! The holder writes a [`Holder`] record at the file's head (the prefix a [`RecordLock`] leaves
//! readable), stamped with the moment it had the lock, and clears it before it lets go. The record
//! is advisory: one that cannot be written leaves the lock held and its holder unnamed. A taker that finds the lock held answers at once
//! with the holder named ([`Wait::No`]), or blocks on the lock, naming the holder when the wait
//! starts and once a minute while it lasts ([`Wait::Block`]). The holder is a live process whose
//! lock the kernel lets go when it exits, however it ends, so the wait is admission behind it:
//! no deadline, no retry and no signal to anyone (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! LOCK-WAIT). An interrupt ends the wait ([`crate::interrupt`]).

use std::fs::File;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::record_lock::RecordLock;

/// The head of the file the record may fill; the lock's range starts after it.
pub const RECORD: u64 = 4096;

/// How often a wait names the holder again.
pub const SAY_EVERY: Duration = Duration::from_secs(60);

/// How often a wait looks for an interrupt and asks its caller whether to stop. A wait names its
/// holder only once a whole tick has passed without the lock, so a holder on its way out goes
/// unnamed.
pub const TICK: Duration = Duration::from_millis(100);

/// Who holds a lock, as its record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// When it took the lock, unix seconds; 0 when the record does not say.
    pub since: u64,
    /// What it runs (`lab linux proof`); empty when the record does not say.
    pub verb: String,
    /// The checkout it runs from; empty when the record does not say.
    pub checkout: String,
}

impl Holder {
    /// This process, from now, running `verb` from `checkout`; [`take`] stamps [`Holder::since`]
    /// again with the moment it has the lock, however long it waited for it.
    #[must_use]
    pub fn me(verb: &str, checkout: &Path) -> Holder {
        Holder {
            pid: std::process::id(),
            since: now_secs(),
            verb: verb.to_string(),
            checkout: checkout.to_string_lossy().into_owned(),
        }
    }

    /// The record: pid, since, verb and checkout on one tab-separated line, cut short of
    /// [`RECORD`] on a character boundary.
    #[must_use]
    pub fn line(&self) -> String {
        let field = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let mut line = format!(
            "{}\t{}\t{}\t{}",
            self.pid,
            self.since,
            field(&self.verb),
            field(&self.checkout)
        );
        let limit = usize::try_from(RECORD).map_or(0, |r| r.saturating_sub(1));
        if line.len() > limit {
            let mut end = limit;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        line.push('\n');
        line
    }

    /// The holder `text` names: a [`Holder::line`], or a bare pid (the trust lane's run lock wrote
    /// only that before it wrote this record).
    #[must_use]
    pub fn parse(text: &str) -> Option<Holder> {
        let line = text.lines().next()?;
        let mut fields = line.splitn(4, '\t');
        let pid = fields.next()?.trim().parse().ok().filter(|p| *p != 0)?;
        let since = match fields.next() {
            None => 0,
            Some(s) => s.parse().ok()?,
        };
        Some(Holder {
            pid,
            since,
            verb: fields.next().unwrap_or_default().to_string(),
            checkout: fields.next().unwrap_or_default().to_string(),
        })
    }

    /// `pid N (verb, checkout, since HH:MM)`, the clock read `offset` seconds east of UTC; what
    /// the record does not say is left out.
    #[must_use]
    pub fn describe(&self, offset: i64) -> String {
        let mut what: Vec<String> = [&self.verb, &self.checkout]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        if self.since != 0 {
            let day = i64::try_from(self.since)
                .unwrap_or(0)
                .saturating_add(offset)
                .rem_euclid(86_400);
            what.push(format!("since {:02}:{:02}", day / 3600, day % 3600 / 60));
        }
        if what.is_empty() {
            format!("pid {}", self.pid)
        } else {
            format!("pid {} ({})", self.pid, what.join(", "))
        }
    }
}

/// The wall clock in unix seconds; 0 before the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The holder `path`'s record names, if it names one.
#[must_use]
pub fn read(path: &Path) -> Option<Holder> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(RECORD)
        .read_to_end(&mut head)
        .ok()?;
    Holder::parse(&String::from_utf8_lossy(&head))
}

/// Whom a taker names: the holder `holder`'s record names while that pid lives (`alive`), its
/// clock `offset_at` the time it took the lock; else a holder that wrote no record (a run of a
/// build older than this record), with the last record's exited pid when there is one.
#[must_use]
pub fn name(
    holder: Option<&Holder>,
    offset_at: impl Fn(u64) -> i64,
    alive: impl Fn(u32) -> bool,
) -> String {
    match holder {
        Some(h) if alive(h.pid) => h.describe(offset_at(h.since)),
        Some(h) => format!(
            "a run that wrote no record (the last record names {}, which has exited)",
            h.describe(offset_at(h.since))
        ),
        None => "a run that wrote no record".to_string(),
    }
}

/// What [`take`] does when another handle holds the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Block until the holder lets go.
    Block,
    /// Answer at once ([`Refused::Held`]).
    No,
}

/// Why [`take`] came back without the lock.
#[derive(Debug)]
pub enum Refused {
    /// Another handle holds it and the taker would not wait: the holder its record names.
    Held(Option<Holder>),
    /// An interrupt, or the taker's `stop`, ended the wait: the holder its record named then.
    Stopped {
        /// The holder its record named.
        holder: Option<Holder>,
        /// The interrupt that ended it ([`crate::interrupt`]); `None` when `stop` did.
        signal: Option<i32>,
    },
    /// The file could not be opened, locked or written.
    Io(io::Error),
}

impl From<io::Error> for Refused {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A held lock whose file names this process; the record is cleared before the lock goes.
#[derive(Debug)]
pub struct NamedLock {
    lock: RecordLock,
    holder: Holder,
    waited: Duration,
}

impl NamedLock {
    /// The record this lock's file holds: its taker, since the moment the taker had the lock.
    #[must_use]
    pub fn holder(&self) -> &Holder {
        &self.holder
    }

    /// How long the taker waited for the lock: zero when it was free.
    #[must_use]
    pub fn waited(&self) -> Duration {
        self.waited
    }
}

impl std::ops::Deref for NamedLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.lock
    }
}

impl Drop for NamedLock {
    fn drop(&mut self) {
        let _ = self.lock.set_len(0);
    }
}

/// Take `path`'s lock as `me`, making the file (owner-only on Unix) when it is missing.
///
/// Held by another handle: with [`Wait::No`], [`Refused::Held`] at once. With [`Wait::Block`],
/// `say` hears the holder (its record as it reads then) once a [`TICK`] has passed without the
/// lock, and every [`SAY_EVERY`] after with the time waited. SIGINT, SIGTERM or SIGHUP while it
/// waits ([`crate::interrupt::Watch`]), or `stop`, asked every tick, saying `true`, ends the wait
/// ([`Refused::Stopped`]). A stopped wait leaves a thread blocked on the lock with a handle of its
/// own until the holder lets go; the lock it then has goes back at once.
/// While it blocks, the wait is marked where a ceiling over this process's tree reads it
/// ([`crate::waits`]), naming the holder afresh each time `say` does.
///
/// # Errors
/// [`Refused`]: [`Refused::Io`] when the file cannot be opened or locked, never for its record.
pub fn take(
    path: &Path,
    me: &Holder,
    wait: Wait,
    say: &mut dyn FnMut(Option<&Holder>, Duration),
    stop: &dyn Fn() -> bool,
) -> Result<NamedLock, Refused> {
    take_marked(path, me, wait, say, stop, crate::waits::dir().as_deref())
}

/// [`take`], its wait marked in `marks` (none: unmarked).
fn take_marked(
    path: &Path,
    me: &Holder,
    wait: Wait,
    say: &mut dyn FnMut(Option<&Holder>, Duration),
    stop: &dyn Fn() -> bool,
    marks: Option<&Path>,
) -> Result<NamedLock, Refused> {
    match RecordLock::try_lock(open(path)?, RECORD) {
        Ok(lock) => return Ok(named(lock, me, Duration::ZERO)),
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        Err(std::fs::TryLockError::WouldBlock) => {}
    }
    if wait == Wait::No {
        return Err(Refused::Held(read(path)));
    }
    let watch = crate::interrupt::Watch::enter();
    let file = open(path)?;
    let mut mark = marks
        .and_then(|d| crate::waits::Mark::at(d, &waits_for(path, me, read(path).as_ref())).ok());
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("named-lock wait".into())
        .spawn(move || {
            // A lock taken after the waiter stopped goes back by the guard's drop, when the send
            // finds nobody to take it.
            let _ = tx.send(RecordLock::lock(file, RECORD));
        })?;
    let started = Instant::now();
    let mut next = TICK;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(taken) => return Ok(named(taken?, me, started.elapsed())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("the wait on the lock ended with no answer").into());
            }
        }
        let signal = watch.take();
        if signal.is_some() || stop() {
            return Err(Refused::Stopped {
                holder: read(path),
                signal,
            });
        }
        let waited = started.elapsed();
        if waited >= next {
            let holder = read(path);
            say(holder.as_ref(), waited);
            if let Some(mark) = &mut mark {
                mark.say(&waits_for(path, me, holder.as_ref()));
            }
            next = if next == TICK {
                SAY_EVERY
            } else {
                next + SAY_EVERY
            };
        }
    }
}

/// What `me`'s wait on `path` marks ([`crate::waits`]): the holder its record names, with no
/// clock of its own, and how long it has held the lock.
fn waits_for(path: &Path, me: &Holder, holder: Option<&Holder>) -> String {
    let lead = if me.verb.is_empty() {
        String::new()
    } else {
        format!("{}: ", me.verb)
    };
    match holder {
        Some(h) => {
            let named = Holder {
                since: 0,
                ..h.clone()
            }
            .describe(0);
            let held = if h.since == 0 {
                String::new()
            } else {
                format!(" for {} min", now_secs().saturating_sub(h.since) / 60)
            };
            format!(
                "{lead}waiting for {named}, which has held {}{held}",
                path.display()
            )
        }
        None => format!(
            "{lead}waiting for a run that wrote no record, which holds {}",
            path.display()
        ),
    }
}

/// `lock`, had after `waited`, its record now `me`'s since this moment. A record that cannot be
/// written (a full disk) is cleared and the lock kept: a waiter then names a run that wrote no
/// record, which this one is.
fn named(lock: RecordLock, me: &Holder, waited: Duration) -> NamedLock {
    let mut held = NamedLock {
        lock,
        holder: Holder {
            since: now_secs(),
            ..me.clone()
        },
        waited,
    };
    let line = held.holder.line();
    let written = held
        .lock
        .set_len(0)
        .and_then(|()| held.lock.seek(io::SeekFrom::Start(0)))
        .and_then(|_| held.lock.write_all(line.as_bytes()));
    if written.is_err() {
        let _ = held.lock.set_len(0);
    }
    held
}

/// Open (creating) `path` for locking and its record, never truncated: a lock file is a
/// rendezvous, and its record is the holder's to clear.
fn open(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(&format!("atc-named-{label}-"))
            .tempdir()
            .expect("a scratch dir")
    }

    fn holder(verb: &str) -> Holder {
        Holder::me(verb, Path::new("/checkouts/a"))
    }

    fn quiet(_: Option<&Holder>, _: Duration) {}

    #[test]
    fn a_record_reads_back_and_a_bare_pid_is_a_holder() {
        let h = Holder {
            pid: 42,
            since: 1_792_000_000,
            verb: "lab linux\tproof".into(),
            checkout: "/a b/c".into(),
        };
        let back = Holder::parse(&h.line()).expect("a record");
        assert_eq!(back.verb, "lab linux proof", "a tab in a field is a space");
        assert_eq!(
            (back.pid, back.since, back.checkout.as_str()),
            (42, h.since, "/a b/c")
        );
        assert_eq!(
            Holder::parse("4242\n").map(|h| (h.pid, h.since)),
            Some((4242, 0))
        );
        assert_eq!(Holder::parse(""), None);
        assert_eq!(Holder::parse("0\t1\tx\ty"), None, "pid 0 names nobody");
        assert!(
            h.describe(-7 * 3600).ends_with("since 10:46)"),
            "{}",
            h.describe(-7 * 3600)
        );
        assert_eq!(
            Holder::parse("4242").map(|h| h.describe(0)),
            Some("pid 4242".to_string())
        );
    }

    #[test]
    fn a_taken_lock_names_its_holder_until_it_goes() {
        let dir = scratch("names");
        let path = dir.path().join("lane.lock");
        let me = holder("lab linux proof");
        let held = take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free");
        assert_eq!(held.waited(), Duration::ZERO, "a free lock is had at once");
        assert_eq!(
            held.holder(),
            &Holder {
                since: held.holder().since,
                ..me.clone()
            }
        );
        assert_eq!(read(&path).as_ref(), Some(held.holder()));
        match take(
            &path,
            &holder("lab linux up"),
            Wait::No,
            &mut quiet,
            &|| false,
        ) {
            Err(Refused::Held(Some(named))) => assert_eq!(&named, held.holder()),
            other => panic!("a held lock must name its holder: {other:?}"),
        }
        drop(held);
        assert_eq!(read(&path), None, "the record goes with the lock");
        drop(take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free again"));
    }

    /// The waiter names the holder once it has waited a tick, then has the lock as soon as the
    /// holder lets go, and its record says since when it has it, not since it asked: the waiter
    /// asks as of second 1, so a record that kept the asking time would say so. Negative control:
    /// a waiter told not to wait answers Held at once.
    #[test]
    fn a_waiter_names_the_holder_and_takes_the_lock_when_it_goes() {
        let dir = scratch("waits");
        let path = dir.path().join("lane.lock");
        let first = holder("lab linux proof");
        let held = take(&path, &first, Wait::No, &mut quiet, &|| false).expect("free");
        let first = held.holder().clone();
        assert!(matches!(
            take(&path, &holder("w"), Wait::No, &mut quiet, &|| false),
            Err(Refused::Held(_))
        ));
        let (go, gone) = mpsc::channel::<()>();
        let letting_go = std::thread::spawn(move || {
            // The one-minute hang detector: the waiter says whom it waits for well before.
            gone.recv_timeout(Duration::from_secs(60))
                .expect("the waiter named its holder");
            let let_go = now_secs();
            drop(held);
            let_go
        });
        let mut heard = Vec::new();
        let started = Instant::now();
        let second = Holder {
            since: 1,
            ..holder("lab linux exec")
        };
        let taken = take(
            &path,
            &second,
            Wait::Block,
            &mut |h, _| {
                heard.push(h.cloned());
                let _ = go.send(());
            },
            &|| started.elapsed() > Duration::from_secs(60),
        )
        .expect("the lock, once its holder let go");
        let let_go = letting_go.join().expect("the holder's thread");
        assert_eq!(heard.first(), Some(&Some(first)), "{heard:?}");
        assert!(taken.waited() >= TICK, "{:?}", taken.waited());
        let record = read(&path).expect("the waiter's record");
        assert_eq!(&record, taken.holder());
        assert_eq!(
            (record.pid, record.verb.as_str(), record.checkout.as_str()),
            (second.pid, "lab linux exec", "/checkouts/a")
        );
        assert!(
            record.since >= let_go,
            "since {} is before the lock was let go at {let_go}",
            record.since
        );
        drop(taken);
    }

    /// A waiter whose caller says stop comes back without the lock, naming the holder.
    #[test]
    fn a_stopped_wait_names_the_holder() {
        let dir = scratch("stops");
        let path = dir.path().join("lane.lock");
        let held = take(
            &path,
            &holder("ship trust run"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        match take(&path, &holder("w"), Wait::Block, &mut quiet, &|| true) {
            Err(Refused::Stopped {
                holder: Some(named),
                signal: None,
            }) => assert_eq!(&named, held.holder()),
            other => panic!("a stopped wait must name its holder: {other:?}"),
        }
        drop(held);
    }

    /// Set, the test binary is [`standin_waits_for_the_lock`], on the lock at this path.
    #[cfg(unix)]
    const STANDIN: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_STANDIN";

    /// Run by [`an_interrupt_ends_a_wait_for_a_held_lock`] in a process of its own: waits for the
    /// lock [`STANDIN`] names, with SIGINT at its default action as a foreground process has it,
    /// writing `waiting` beside it once it waits, and exits 3, a tool's NOT RUN, naming what
    /// ended the wait and the holder.
    #[cfg(unix)]
    #[test]
    fn standin_waits_for_the_lock() {
        let Some(path) = std::env::var_os(STANDIN).map(std::path::PathBuf::from) else {
            return;
        };
        // SAFETY: SIG_DFL is a disposition, no handler of ours.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        let waiting = path.with_file_name("waiting");
        let started = Instant::now();
        let got = take(
            &path,
            &holder("lab linux exec"),
            Wait::Block,
            &mut |_, _| {
                let _ = std::fs::write(&waiting, std::process::id().to_string());
            },
            &|| started.elapsed() > Duration::from_secs(60),
        );
        match got {
            Err(Refused::Stopped {
                holder: Some(h),
                signal: Some(sig),
            }) => {
                println!(
                    "stopped by {} waiting for {}",
                    crate::interrupt::name(sig),
                    h.describe(0)
                );
                std::process::exit(3);
            }
            other => panic!("the wait ended otherwise: {other:?}"),
        }
    }

    /// A stand-in, killed when it is dropped still running (a test that failed before it
    /// ended), so none outlives its test.
    #[cfg(unix)]
    struct Standin(aterm_uds::spawnfd::Child);

    #[cfg(unix)]
    impl Drop for Standin {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// A process waiting for a held lock ends on SIGTERM, and on SIGINT where it is not
    /// ignored, well inside the one-minute hang detector (the time is reported), naming the
    /// holder and the signal, with nobody signalling the holder, which keeps the lock. Without
    /// the wait's watch its caller's `stop` never says so, and the stand-in dies of the signal
    /// instead (no exit 3).
    #[cfg(unix)]
    #[test]
    fn an_interrupt_ends_a_wait_for_a_held_lock() {
        for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
            let dir = scratch("interrupt");
            let path = dir.path().join("lane.lock");
            let held = take(
                &path,
                &holder("lab linux proof"),
                Wait::No,
                &mut quiet,
                &|| false,
            )
            .expect("free");
            let mut child = Standin(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "named_lock::tests::standin_waits_for_the_lock",
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(STANDIN, &path)
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the stand-in"),
            );
            let waiting = dir.path().join("waiting");
            let pid: i32 = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the stand-in to wait for the lock",
                || std::fs::read_to_string(&waiting).ok()?.trim().parse().ok(),
            );
            let sent = Instant::now();
            // SAFETY: `kill` takes two integers; the pid is this test's own child.
            assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
            let status = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the stand-in's end",
                || child.0.try_wait().ok().flatten(),
            );
            let took = sent.elapsed();
            let mut out = String::new();
            if let Some(mut o) = child.0.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut o, &mut out);
            }
            assert_eq!(status.code(), Some(3), "{word}: {status} {out}");
            assert!(
                out.contains(&format!(
                    "stopped by {word} waiting for pid {}",
                    held.holder().pid
                )),
                "{out}"
            );
            eprintln!("{word} ended the lock wait in {took:?}");
            assert_eq!(
                read(&path).as_ref(),
                Some(held.holder()),
                "the holder keeps it"
            );
            drop(held);
        }
    }

    /// A blocked wait is marked for a ceiling to read, naming the holder, and the mark goes once
    /// the waiter has the lock. Without the mark the look for it hangs past its detector.
    #[test]
    fn a_blocked_wait_is_marked_until_it_has_the_lock() {
        let dir = scratch("marked");
        let path = dir.path().join("lane.lock");
        let marks = dir.path().join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let held = take(
            &path,
            &holder("lab linux proof"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        let pid = held.holder().pid;
        let waiter = {
            let (path, marks) = (path.clone(), marks.clone());
            std::thread::spawn(move || {
                let started = Instant::now();
                take_marked(
                    &path,
                    &holder("lab linux exec"),
                    Wait::Block,
                    &mut quiet,
                    &|| started.elapsed() > Duration::from_secs(60),
                    Some(&marks),
                )
                .map(drop)
            })
        };
        let detector = Instant::now() + Duration::from_secs(60);
        let seen = loop {
            if let Some(w) = crate::waits::waiting(&marks).pop() {
                break w;
            }
            assert!(Instant::now() < detector, "the wait was never marked");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            seen.text.starts_with(&format!(
                "lab linux exec: waiting for pid {pid} (lab linux proof, /checkouts/a), which has held"
            )),
            "{}",
            seen.text
        );
        drop(held);
        assert!(waiter.join().expect("the waiter").is_ok());
        assert!(
            crate::waits::waiting(&marks).is_empty(),
            "the mark outlived the wait"
        );
    }

    #[test]
    fn a_dead_pids_record_is_not_its_holder() {
        let h = holder("lab linux proof");
        let live = name(Some(&h), |_| 0, |_| true);
        assert!(live.starts_with(&format!("pid {}", h.pid)), "{live}");
        let stale = name(Some(&h), |_| 0, |_| false);
        assert!(stale.starts_with("a run that wrote no record"), "{stale}");
        assert!(stale.contains(&format!("pid {}", h.pid)), "{stale}");
        assert!(!name(None, |_| 0, |_| true).contains("pid"));
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A person's cleanup of a request (ATERM-KI-1109-3, step 5b of the requests design, §7):
//! `request clean <id> [stop=1]`.
//!
//! A cleanup writes the request's archive before it touches anything: the request's journal and
//! content-tier rows, each standing tab's last [`ARCHIVE_ROWS`] screen rows, each worker's result
//! and brief. Only then does it close the request's tabs (on the main thread, which first runs the
//! footprint look, so a person's latest work in a folder is recorded), end what the trees of the
//! tabs that closed still run (each process by the identity read before the close, never a pid
//! alone), measure each claimed folder again, list each folder it may delete in the archive's
//! manifest (every file's path, size and SHA-256, small text files kept whole), and delete a
//! folder only where [`aterm_requests::clean_verdict`] finds no reason to keep it, judged again
//! immediately before the removal on fresh facts: its shape against its listing, its repos read
//! again, the standing tabs' folders read again. A folder a person worked in, one holding a repo
//! with work no remote has, one a standing tab works in, one whose birth moved, one reached through
//! a link, one changed since its listing, the home and every folder holding it, a folder macOS
//! guards and the state home are never deleted; nor is anything while a tab of the request that
//! did not close stands.
//!
//! Every step is a journal row ([`aterm_requests::Store::begin_clean`], `archived`, `gone`,
//! `cleaned` or `uncleaned`), each `gone` written as its folder is done, so a cleanup a quit or a
//! crash cut short reads `cleaning` and a second `request clean` resumes it, skipping every folder
//! already deleted. One cleanup of a request runs at a time in every aterm on the state home
//! ([`Flight`]), each on its process's thread `requests-clean`; the control thread waits
//! [`CLEAN_WAIT`] for it and otherwise answers `OK cleaning`.

use crate::requests_footprint::{MeasureProbe, Places};
use crate::session_store::{ExitActor, Store};
use aterm_codec::pct;
use aterm_requests::{
    ActRefusal, Archive, CleanEnd, CleanPlan, CleanStart, Did, GoneClaim, HostFacts, Keep,
    Measured, Request, Unclean, claim, clean_verdict,
};
use aterm_session::SessionId;
use aterm_uds::process::{EndControl, Identity};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long `request clean` waits for its cleanup before it answers `OK cleaning`.
pub(crate) const CLEAN_WAIT: Duration = Duration::from_secs(120);

/// The most screen rows the archive keeps of each standing tab.
pub(crate) const ARCHIVE_ROWS: usize = 2_000;

/// The rows one hold of a tab's terminal lock reads.
const ROW_CHUNK: usize = 200;

/// The most bytes the archive keeps of one tab's screen rows (the newest), and of all of them.
const SCREEN_BYTES: usize = 1024 * 1024;
const SCREENS_ALL: usize = 4 * 1024 * 1024;

/// The most bytes the archive keeps of a worker's result and of its brief.
const RESULT_BYTES: u64 = 256 * 1024;
const BRIEF_BYTES: u64 = 64 * 1024;

/// The most bytes the archive keeps of the request's journal rows, and of its content-tier rows.
const RECORD_BYTES: usize = 2 * 1024 * 1024;

/// The largest text file the archive keeps whole, and the most bytes of them it keeps.
pub(crate) const ARCHIVE_TEXT_EACH: u64 = 256 * 1024;
pub(crate) const ARCHIVE_TEXT_ALL: u64 = 8 * 1024 * 1024;

/// The most entries, manifest bytes and hashed bytes, and the longest time, a cleanup's listing of
/// its claimed folders takes; past any of them a folder's listing is not whole and it is kept
/// ([`Keep::Unlisted`]).
const MANIFEST_FILES_MAX: usize = 100_000;
const MANIFEST_BYTES_MAX: usize = 8 * 1024 * 1024;
const HASH_BYTES_MAX: u64 = 4 * 1024 * 1024 * 1024;
const HASH_BUDGET: Duration = Duration::from_secs(120);

/// The extensions of a text file the archive keeps whole (the JSON receipts and logs of §2).
const TEXT_EXTENSIONS: [&str; 9] = [
    "json", "txt", "log", "md", "toml", "yaml", "yml", "csv", "tsv",
];

/// The archive's folder in the store's.
pub(crate) const ARCHIVE: &str = "archive";

/// How often a cleanup looks again at the processes it asked to end.
const END_POLL: Duration = Duration::from_millis(20);

const USAGE: &str = "ERR usage: request clean <id> [stop=1]\n";

/// What a cleanup read of one standing tab before it closed it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Captured {
    pub(crate) sid: String,
    /// Its last [`ARCHIVE_ROWS`] rows, at most [`SCREEN_BYTES`].
    pub(crate) screen: String,
    /// Its worker's result file, at most [`RESULT_BYTES`].
    pub(crate) result: Option<Vec<u8>>,
    /// Its worker's brief, at most [`BRIEF_BYTES`].
    pub(crate) brief: Option<Vec<u8>>,
    /// Every process of its tree, by identity, its shell first.
    pub(crate) tree: Vec<Identity>,
}

/// What the main thread's close of a request's tabs did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Closed {
    /// The tabs it closed.
    pub(crate) closed: Vec<String>,
    /// The tabs it did not close, each running a command, so it closed none.
    pub(crate) busy: Vec<String>,
    /// The tabs whose close failed, and why.
    pub(crate) failed: Vec<(String, String)>,
    /// The working folder of every tab that still stands, in [`claim::normal`] form.
    pub(crate) cwds: Vec<String>,
}

/// A [`Kit`]'s read of a standing tab.
pub(crate) type Capture = dyn Fn(&str) -> Option<Captured> + Send;

/// A [`Kit`]'s close of a request's tabs: `Err` only where it closed none.
pub(crate) type Close = dyn Fn(&[String], &ExitActor, bool) -> Result<Closed, String> + Send;

/// A [`Kit`]'s read of the working folder of every tab that stands now, in [`claim::normal`] form.
pub(crate) type Cwds = dyn Fn() -> Vec<String> + Send;

/// What a cleanup reaches beyond the store: injected, so a test stands in for each.
pub(crate) struct Kit {
    /// Read a standing tab before it closes; `None` where this process no longer hosts it.
    pub(crate) capture: Box<Capture>,
    /// Close these tabs by this actor, each even where a command runs in it where `true`.
    pub(crate) close: Box<Close>,
    /// The working folders of the tabs that stand, read again before each folder is deleted.
    pub(crate) cwds: Box<Cwds>,
    /// The kernel's reads and signals of a process.
    pub(crate) end: Box<dyn EndControl + Send>,
    /// How long a process asked to end (`SIGTERM`) has before it is killed.
    pub(crate) grace: Duration,
    /// A claimed folder's measure.
    pub(crate) measure: MeasureProbe,
    /// The home and the state home.
    pub(crate) places: Places,
}

/// One cleanup to run ([`run`]).
pub(crate) struct Job {
    /// The store's folder.
    pub(crate) dir: PathBuf,
    /// What [`aterm_requests::Store::begin_clean`] began.
    pub(crate) plan: CleanPlan,
    /// Who.
    pub(crate) actor: ExitActor,
    /// Its token.
    pub(crate) by: String,
    /// Held while the cleanup runs, so a second `request clean` of it answers `OK cleaning`.
    pub(crate) flight: Flight,
}

/// How a cleanup ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Cleaned {
        tabs: u64,
        deleted: u64,
        kept: u64,
        failed: u64,
        /// The archive's folder, relative to the state home.
        archive: String,
    },
    Uncleaned {
        why: Unclean,
        detail: String,
    },
}

/// A cleanup's hold on its request: the exclusive lock of `<id>.lock` in the archive's folder of
/// its store, which refuses every other cleanup of it, in this aterm or another on the same state
/// home (a lock belongs to one opening of the file, so two takes in one process refuse each other
/// too). Released when the cleanup ends: its Drop lets the lock go before the file closes.
pub(crate) struct Flight(std::fs::File);

impl Flight {
    /// The hold on request `id` of the store in folder `dir`; `None` where another cleanup holds
    /// it.
    fn take(dir: &Path, id: &str) -> std::io::Result<Option<Self>> {
        let root = dir.join(ARCHIVE);
        crate::control_auth::ensure_private_dir(&root)?;
        let path = root.join(format!("{id}.lock"));
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is not a plain file", path.display()),
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A close posted to the main thread ([`crate::Wake::CloseRequestTabs`]), run only while its
/// cleanup still waits: the main thread takes it, or the cleanup, its wait over, cancels it; never
/// both, so a cleanup that gave up never finds its tabs closed after.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ticket(Arc<AtomicU8>);

impl Ticket {
    const POSTED: u8 = 0;
    const TAKEN: u8 = 1;
    const CANCELLED: u8 = 2;

    /// The main thread's: whether it may close, the cleanup still waiting.
    pub(crate) fn take(&self) -> bool {
        self.0
            .compare_exchange(
                Self::POSTED,
                Self::TAKEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The cleanup's, its wait over: whether the close was cancelled before the main thread took
    /// it.
    fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                Self::POSTED,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// The main thread's answer on `rx` to a close of tabs `sids` posted with `ticket`, waited on for
/// `wait`. Past it the close is cancelled where the main thread has not taken it (`Err`: nothing
/// closed); one it took runs, so it is waited for to its end. A reply the main thread dropped
/// after it took the close reads every tab as not closed, since some may have.
fn closed_within(
    rx: &mpsc::Receiver<Closed>,
    ticket: &Ticket,
    wait: Duration,
    sids: &[String],
) -> Result<Closed, String> {
    let dropped = || Closed {
        failed: sids
            .iter()
            .map(|sid| (sid.clone(), "the main thread dropped its close".to_owned()))
            .collect(),
        ..Closed::default()
    };
    match rx.recv_timeout(wait) {
        Ok(closed) => Ok(closed),
        Err(mpsc::RecvTimeoutError::Timeout) if ticket.cancel() => Err(format!(
            "the main thread did not take the close within {}s, so nothing was closed",
            wait.as_secs()
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => rx.recv().map_or_else(|_| Ok(dropped()), Ok),
        Err(mpsc::RecvTimeoutError::Disconnected) if ticket.cancel() => {
            Err("the main thread dropped the close before it ran".to_owned())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(dropped()),
    }
}

/// `request clean <id> [stop=1]` by `actor` (its token `by`), on the connection being served.
pub(crate) fn cmd(
    after: &str,
    actor: ExitActor,
    by: &str,
    sessions: &Store,
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> String {
    let hosted = |sid: &str| {
        SessionId::parse(sid).is_some_and(|sid| {
            sessions
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_sid(&sid)
                .is_some()
        })
    };
    let start = |job: Job| {
        let kit = host_kit(sessions.clone(), proxy.clone());
        post(job, kit)
    };
    cmd_as(after, actor, by, &hosted, &start, CLEAN_WAIT)
}

/// [`cmd`] with `hosted` saying whether this process hosts a tab, `start` running a cleanup and
/// handing back its outcome's channel, waited on for `wait`.
pub(crate) fn cmd_as(
    after: &str,
    actor: ExitActor,
    by: &str,
    hosted: &dyn Fn(&str) -> bool,
    start: &dyn Fn(Job) -> mpsc::Receiver<Outcome>,
    wait: Duration,
) -> String {
    let mut words = after.split_whitespace();
    let Some(id) = words.next() else {
        return USAGE.to_owned();
    };
    if let Err(why) = aterm_types::control_verbs::request_id(id) {
        return format!(
            "ERR request clean {}: {why}\n",
            aterm_codec::tty::one_line(id, 40)
        );
    }
    let mut stop = None;
    for word in words {
        match word.strip_prefix("stop=") {
            Some("1") if stop.is_none() => stop = Some(true),
            Some("0") if stop.is_none() => stop = Some(false),
            _ => return USAGE.to_owned(),
        }
    }
    let stop = stop.unwrap_or(false);
    let dir = match crate::requests_host::dir() {
        Ok(Some(dir)) if dir.join(aterm_requests::JOURNAL).exists() => dir,
        Ok(_) => return format!("ERR request clean: {}\n", ActRefusal::Unknown),
        Err(why) => return format!("ERR request store: {why}\n"),
    };
    let flight = match Flight::take(&dir, id) {
        Ok(Some(flight)) => flight,
        Ok(None) => return format!("OK cleaning req={id}\n"),
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    let at = crate::requests_host::now_unix_ms();
    let begun = crate::requests_host::with_store(&dir, |store| {
        Ok(store.begin_clean(at, id, by, stop, hosted))
    });
    let plan = match begun {
        Ok(Ok(CleanStart::Plan(plan))) => *plan,
        Ok(Ok(CleanStart::Already)) => return format!("OK already req={id}\n"),
        Ok(Err(refusal)) => return format!("ERR request clean: {refusal}\n"),
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    aterm_log::info!(
        "requests: {id} cleanup begun by {by} (stop={}{})",
        u8::from(stop),
        if plan.resumed { ", resumed" } else { "" }
    );
    crate::requests_host::relisted(&dir);
    let outcome = start(Job {
        dir,
        plan,
        actor,
        by: by.to_owned(),
        flight,
    });
    match outcome.recv_timeout(wait) {
        Ok(Outcome::Cleaned {
            tabs,
            deleted,
            kept,
            failed,
            archive,
        }) => format!(
            "OK cleaned req={id} tabs={tabs} deleted={deleted} kept={kept} failed={failed} \
             archive={}\n",
            pct::encode(&archive)
        ),
        Ok(Outcome::Uncleaned { why, detail }) => format!(
            "ERR request clean: req={id} was not cleaned (why={}): {detail}\n",
            why.word()
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => format!("OK cleaning req={id}\n"),
        Err(mpsc::RecvTimeoutError::Disconnected) => format!(
            "ERR request clean: req={id}'s cleanup stopped before it ended; `request clean {id}` \
             resumes it\n"
        ),
    }
}

/// The cleanup thread's queue.
static CLEANER: Mutex<Option<mpsc::Sender<Queued>>> = Mutex::new(None);

/// One cleanup on the cleanup thread's queue, with where its outcome goes.
type Queued = (Job, Kit, mpsc::Sender<Outcome>);

/// Run `job` with `kit` on the cleanup thread: its outcome's channel.
fn post(job: Job, kit: Kit) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let mut thread = CLEANER.lock().unwrap_or_else(|p| p.into_inner());
    let item = match thread.as_ref() {
        Some(queue) => match queue.send((job, kit, tx)) {
            Ok(()) => return rx,
            Err(mpsc::SendError(item)) => item,
        },
        None => (job, kit, tx),
    };
    let (queue, jobs) = mpsc::channel::<Queued>();
    let started = std::thread::Builder::new()
        .name("requests-clean".to_owned())
        .spawn(move || {
            for (job, mut kit, reply) in jobs {
                let _ = reply.send(run(job, &mut kit));
            }
        });
    match started {
        Ok(_) => {
            let _ = queue.send(item);
            *thread = Some(queue);
        }
        Err(error) => aterm_log::warn!("requests: no cleanup thread: {error}"),
    }
    rx
}

/// This host's [`Kit`]: the session registry `sessions`, and the main thread through `proxy`.
fn host_kit(sessions: Store, proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>) -> Kit {
    let read = sessions.clone();
    let standing = sessions.clone();
    Kit {
        capture: Box::new(move |sid| capture_host(&read, sid)),
        cwds: Box::new(move || standing_cwds(&standing)),
        close: Box::new(move |sids, by, force| {
            let tabs: Vec<(u64, String)> = {
                let registry = sessions.read().unwrap_or_else(|p| p.into_inner());
                sids.iter()
                    .filter_map(|sid| {
                        SessionId::parse(sid)
                            .and_then(|id| registry.by_sid(&id))
                            .map(|handle| (handle.local_id, sid.clone()))
                    })
                    .collect()
            };
            let ticket = Ticket::default();
            let posted = ticket.clone();
            let (rx, _hop) = crate::control::control_media::post_main(&proxy, |reply| {
                crate::Wake::CloseRequestTabs {
                    tabs,
                    by: by.clone(),
                    force,
                    ticket: posted,
                    reply,
                }
            })
            .map_err(|error| error.to_string())?;
            closed_within(&rx, &ticket, CLEAN_WAIT, sids)
        }),
        end: Box::new(aterm_uds::process::EndControlLive),
        grace: aterm_uds::process::END_GRACE,
        measure: crate::requests_footprint::measure_probe(),
        places: Places::host(),
    }
}

/// The working folder of every tab `sessions` registers now, in [`claim::normal`] form.
fn standing_cwds(sessions: &Store) -> Vec<String> {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<crate::session_store::SessionHandle> = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .live_handles()
        .cloned()
        .collect();
    handles
        .iter()
        .filter_map(|handle| {
            let term = crate::term_lock(&handle.term);
            term.native_working_directory()
                .and_then(|cwd| claim::normal(&cwd).ok())
        })
        .collect()
}

/// Tab `sid` of `sessions` as a cleanup keeps it ([`Captured`]).
fn capture_host(sessions: &Store, sid: &str) -> Option<Captured> {
    let id = SessionId::parse(sid)?;
    let handle = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(&id)
        .cloned()?;
    let result = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .and_then(|task| task.result_path.clone())
        .and_then(|path| read_capped(&path, RESULT_BYTES));
    let brief = crate::task_launch::task_file_of(&handle.sid)
        .and_then(|path| read_capped(&path, BRIEF_BYTES));
    Some(Captured {
        sid: sid.to_owned(),
        screen: screen_of(&handle.term),
        result,
        brief,
        tree: tree_of(&handle.shell_identity, &crate::tab_marks::TreeProbe::HOST),
    })
}

/// The last [`ARCHIVE_ROWS`] rows of `term`, scrollback included, each with its trailing blanks
/// trimmed: read [`ROW_CHUNK`] rows per hold of its lock, so a cleanup never holds a drawing tab
/// for long; the newest [`SCREEN_BYTES`] of them.
fn screen_of(term: &std::sync::Mutex<aterm_core::terminal::Terminal>) -> String {
    let lock = || term.lock().unwrap_or_else(|p| p.into_inner());
    let (floor, rows) = {
        let t = lock();
        (
            -i64::try_from(t.grid().scrollback_lines()).unwrap_or(i64::MAX),
            i64::from(t.rows()),
        )
    };
    let span = i64::try_from(ARCHIVE_ROWS).unwrap_or(i64::MAX);
    let chunk = i64::try_from(ROW_CHUNK).unwrap_or(1);
    let mut out = String::new();
    let mut row = floor.max(rows - span);
    while row < rows {
        let end = (row + chunk).min(rows);
        let t = lock();
        for at in row..end {
            if let Some(text) = i32::try_from(at)
                .ok()
                .and_then(|at| t.written_line_text(at))
            {
                let start = out.len();
                out.extend(text.chars().map(aterm_control::wire::visible_char));
                let kept = out[start..].trim_end().len();
                out.truncate(start + kept);
            }
            out.push('\n');
        }
        drop(t);
        row = end;
    }
    newest(out, SCREEN_BYTES)
}

/// The newest `cap` bytes of `text`, cut at a line's start.
fn newest(text: String, cap: usize) -> String {
    if text.len() <= cap {
        return text;
    }
    let mut from = text.len() - cap;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let from = text[from..]
        .find('\n')
        .map_or(text.len(), |at| from + at + 1);
    text[from..].to_owned()
}

/// The first `cap` bytes of the regular file at `path` (never through a final link).
fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let (file, _) = aterm_uds::regular_file::open(path).ok()?;
    let mut out = Vec::new();
    file.take(cap).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Every process of the tree under the shell `shell`, by identity: the shell first where its
/// recorded birth matches the kernel's identity, then each descendant whose identity reads, at
/// most [`crate::tab_marks::TREE_MAX`]. A missing or mismatched recorded birth returns no tree.
pub(crate) fn tree_of(
    shell: &aterm_pty::ShellIdentity,
    probe: &crate::tab_marks::TreeProbe,
) -> Vec<Identity> {
    tree_with(shell, probe, aterm_uds::process::read_identity)
}

fn tree_with(
    shell: &aterm_pty::ShellIdentity,
    probe: &crate::tab_marks::TreeProbe,
    identity: impl Fn(i32) -> Option<Identity>,
) -> Vec<Identity> {
    let Some(root) = u32::try_from(shell.pid).ok().filter(|pid| *pid > 1) else {
        return Vec::new();
    };
    let Some(first) = identity(shell.pid) else {
        return Vec::new();
    };
    if !shell.matches_kernel_birth(first.pid, first.birth.0) {
        return Vec::new();
    }
    let mut tree = vec![first];
    let mut seen = BTreeSet::from([root]);
    let mut next: Vec<u32> = (probe.children)(root).unwrap_or_default();
    while let Some(pid) = next.pop() {
        if seen.len() >= crate::tab_marks::TREE_MAX || !seen.insert(pid) {
            continue;
        }
        if let Some(id) = i32::try_from(pid).ok().and_then(&identity) {
            tree.push(id);
        }
        next.extend((probe.children)(pid).unwrap_or_default());
    }
    tree
}

/// End every process of `tree` that the kernel still names by its identity: `SIGTERM`, one shared
/// `grace` for all, then `SIGKILL` for each still there. A process whose pid now names another
/// (a new birth, or an exec) is never signalled. Returns how many were asked to end.
fn end_tree(tree: &[Identity], end: &mut dyn EndControl, grace: Duration) -> usize {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    let alive = |end: &mut dyn EndControl, id: &Identity| end.identity(id.pid) == Some(*id);
    let mut termed: Vec<Identity> = Vec::new();
    for id in tree {
        if !termed.contains(id) && alive(end, id) && end.signal(id, SIGTERM).is_ok() {
            termed.push(*id);
        }
    }
    let asked = termed.len();
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(Instant::now);
    loop {
        termed.retain(|id| alive(end, id));
        if termed.is_empty() || Instant::now() >= deadline {
            break;
        }
        end.nap(END_POLL);
    }
    for id in &termed {
        if alive(end, id) {
            let _ = end.signal(id, SIGKILL);
        }
    }
    asked
}

/// A cleanup's archive while it is written: a private folder beside the archive's, renamed into
/// place once whole.
struct Archiving {
    root: PathBuf,
    tmp: PathBuf,
    name: String,
    bytes: u64,
    files: u64,
    listed: u64,
    whole: bool,
    text_left: u64,
    manifest: String,
}

impl Archiving {
    /// A new archive of request `id` in store folder `dir`: the first of its names
    /// ([`aterm_requests::archive_name`]) no archive holds.
    fn begin(dir: &Path, id: &str) -> std::io::Result<Self> {
        let root = dir.join(ARCHIVE);
        crate::control_auth::ensure_private_dir(&root)?;
        let pid = std::process::id();
        let mut n = 1;
        let (name, tmp) = loop {
            let name = aterm_requests::archive_name(id, n);
            let tmp = root.join(format!("{name}.tmp-{pid}"));
            if std::fs::symlink_metadata(root.join(&name)).is_err()
                && std::fs::symlink_metadata(&tmp).is_err()
            {
                break (name, tmp);
            }
            n += 1;
        };
        crate::control_auth::ensure_private_dir(&tmp)?;
        Ok(Self {
            root,
            tmp,
            name,
            bytes: 0,
            files: 0,
            listed: 0,
            whole: true,
            text_left: ARCHIVE_TEXT_ALL,
            manifest: String::new(),
        })
    }

    /// Write `bytes` as the archive's file `name` (a relative path), finished by an ordered flush.
    fn write(&mut self, name: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.tmp.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(bytes)?;
        aterm_durable::ordered(&file)?;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }

    /// The archive's first part, before any tab closes: the request's rows of the store in `dir`
    /// and what `captured` read of its standing tabs.
    fn first(&mut self, dir: &Path, id: &str, captured: &[Captured]) -> std::io::Result<()> {
        let (journal, said) = aterm_requests::lines_of(dir, id)?;
        let lines = |rows: Vec<String>| {
            let mut text = rows.join("\n");
            text.push('\n');
            newest(text, RECORD_BYTES)
        };
        self.write(Path::new("record"), lines(journal).as_bytes())?;
        self.write(Path::new("said"), lines(said).as_bytes())?;
        let mut screens = SCREENS_ALL;
        for tab in captured {
            let screen = newest(tab.screen.clone(), screens.min(SCREEN_BYTES));
            screens = screens.saturating_sub(screen.len());
            self.write(
                Path::new(&format!("screen-{}.txt", tab.sid)),
                screen.as_bytes(),
            )?;
            if let Some(result) = &tab.result {
                self.write(Path::new(&format!("result-{}.md", tab.sid)), result)?;
            }
            if let Some(brief) = &tab.brief {
                self.write(Path::new(&format!("brief-{}.txt", tab.sid)), brief)?;
            }
        }
        Ok(())
    }

    /// List claimed folder `path`, the `n`th listed, in the manifest ([`walk_claim`]): every
    /// entry under it, a regular file with its size and SHA-256 and kept whole under `files/<n>/`
    /// where it is small text ([`TEXT_EXTENSIONS`], at most [`ARCHIVE_TEXT_EACH`], while the
    /// archive keeps under [`ARCHIVE_TEXT_ALL`] of them). Its shape ([`shape_of`]) where the
    /// listing is whole; `None` where a cap passed, a folder macOS guards, or anything unread
    /// left it not.
    fn list(&mut self, path: &Path, home: Option<&Path>, budget: &mut Budget) -> Option<Shape> {
        self.listed += 1;
        let n = self.listed;
        let mut rows = format!("# claim {} n={n}\n", pct::encode(&path.to_string_lossy()));
        let mut shape = aterm_digest::Sha256::new();
        let whole = walk_claim(path, home, budget, &mut |rel, seen, entry, budget| {
            if rows.len() + self.manifest.len() > MANIFEST_BYTES_MAX {
                return false;
            }
            shape.update(stamp(rel, seen, entry).as_bytes());
            let shown = pct::encode(&rel.to_string_lossy());
            match seen {
                Seen::File => match self.file(&path.join(rel), rel, n, budget) {
                    Some((bytes, sha)) => {
                        self.files += 1;
                        rows.push_str(&format!("file\t{shown}\t{bytes}\t{sha}\n"));
                    }
                    None => return false,
                },
                other => rows.push_str(&format!("{}\t{shown}\t-\t-\n", other.word())),
            }
            true
        });
        rows.push_str(&format!("# whole={}\n", u8::from(whole)));
        self.manifest.push_str(&rows);
        self.whole &= whole;
        whole.then(|| shape.finalize())
    }

    /// One regular file of the `n`th claim, at `path` (relative `rel`): its size and SHA-256, and
    /// a copy where it is small text; `None` where it could not be read whole within `budget`.
    fn file(
        &mut self,
        path: &Path,
        rel: &Path,
        n: u64,
        budget: &mut Budget,
    ) -> Option<(u64, String)> {
        let (file, meta) = aterm_uds::regular_file::open(path).ok()?;
        let len = meta.len();
        if budget.hashed.saturating_add(len) > HASH_BYTES_MAX {
            return None;
        }
        let text = rel
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| TEXT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
            && len <= ARCHIVE_TEXT_EACH
            && len <= self.text_left;
        let mut hasher = aterm_digest::Sha256::new();
        let mut kept = Vec::new();
        let mut read = 0_u64;
        let mut chunk = vec![0_u8; 64 * 1024];
        let mut file = file;
        loop {
            let got = file.read(&mut chunk).ok()?;
            if got == 0 {
                break;
            }
            hasher.update(&chunk[..got]);
            read += got as u64;
            if text && read <= ARCHIVE_TEXT_EACH {
                kept.extend_from_slice(&chunk[..got]);
            }
            if budget.started.elapsed() > HASH_BUDGET {
                return None;
            }
        }
        budget.hashed = budget.hashed.saturating_add(read);
        if text && read == len && std::str::from_utf8(&kept).is_ok() {
            let copy = Path::new("files").join(n.to_string()).join(rel);
            if self.write(&copy, &kept).is_ok() {
                self.text_left = self.text_left.saturating_sub(read);
            }
        }
        Some((read, aterm_codec::hex::encode_lower(&hasher.finalize())))
    }

    /// Drop the archive being written: a cleanup that closed nothing keeps none, and a second
    /// cleanup writes its own.
    fn discard(self) {
        if let Err(error) = std::fs::remove_dir_all(&self.tmp) {
            aterm_log::warn!(
                "requests: an unfinished archive {} stays: {error}",
                self.tmp.display()
            );
        }
    }

    /// Finish the archive: its manifest written, then its folder renamed into place.
    fn finish(mut self, at_ms: u64) -> std::io::Result<Archive> {
        let manifest = std::mem::take(&mut self.manifest);
        self.write(Path::new("manifest.tsv"), manifest.as_bytes())?;
        aterm_durable::ordered_dir(&self.tmp)?;
        let at = self.root.join(&self.name);
        std::fs::rename(&self.tmp, &at)?;
        aterm_durable::ordered_dir(&self.root)?;
        Ok(Archive {
            at_ms,
            dir: self.name,
            bytes: self.bytes,
            files: self.files,
            listed: self.listed,
            whole: self.whole,
        })
    }
}

/// What a cleanup's listing has spent of its caps.
struct Budget {
    started: Instant,
    entries: usize,
    hashed: u64,
}

impl Budget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            entries: 0,
            hashed: 0,
        }
    }
}

/// A claimed folder's shape: the SHA-256 of every entry's [`stamp`], in its walk's order.
type Shape = [u8; 32];

/// What a walk of a claimed folder met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Link,
    Dir,
    File,
    Other,
}

impl Seen {
    /// Its manifest word.
    const fn word(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Dir => "dir",
            Self::File => "file",
            Self::Other => "other",
        }
    }
}

/// What a walk read of one entry: its own metadata (never a link's target's) and, on Windows, its
/// change-journal counter.
struct Entry {
    meta: std::fs::Metadata,
    /// The entry's counter in its volume's change journal ([`aterm_dirfd::usn`]); `None` where the
    /// volume keeps no journal.
    #[cfg(windows)]
    usn: Option<aterm_dirfd::Usn>,
}

/// Entry `path`'s [`Entry`], read through one handle on the entry itself, opened as
/// `symlink_metadata` opens it: no right asked (neither read needs one), every sharing allowed,
/// and a folder, a link or another reparse point opened as itself.
#[cfg(windows)]
fn entry_at(path: &Path) -> std::io::Result<Entry> {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
    const FILE_SHARE_ALL: u32 = 0x7;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    Ok(Entry {
        meta: file.metadata()?,
        usn: aterm_dirfd::usn(&file)?,
    })
}

/// Walk claimed folder `path` breadth first, no link followed and no folder macOS guards entered
/// (none at all without a home), calling `visit` with each entry's path relative to `path`, what
/// it is and its [`Entry`]; `visit` answers whether the walk goes on. Whether the walk was whole:
/// `path` a folder and no link, every folder listed, every entry read, `visit` never stopping it,
/// and within `budget`'s [`MANIFEST_FILES_MAX`] and [`HASH_BUDGET`].
fn walk_claim(
    path: &Path,
    home: Option<&Path>,
    budget: &mut Budget,
    visit: &mut dyn FnMut(&Path, Seen, &Entry, &mut Budget) -> bool,
) -> bool {
    // A claim that is now a link, or no folder, is never walked through.
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        return false;
    }
    let mut next = VecDeque::from([PathBuf::new()]);
    while let Some(rel) = next.pop_front() {
        let dir = path.join(&rel);
        if home.is_none_or(|home| atpkg::protected::under_protected_root(home, &dir)) {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            budget.entries += 1;
            if budget.entries > MANIFEST_FILES_MAX || budget.started.elapsed() > HASH_BUDGET {
                return false;
            }
            let rel = rel.join(entry.file_name());
            // The entry's own metadata: a link is read as a link. Off Windows that is the
            // listing's `DirEntry::metadata`. Windows lists the folder's copy of an entry's times
            // and size, which NTFS can leave behind the entry's own until something opens it, so
            // a walk just after a write read a shape the next walk did not: there it is read
            // through a handle on the entry, with the entry's change-journal counter.
            #[cfg(not(windows))]
            let read = entry.metadata().map(|meta| Entry { meta });
            #[cfg(windows)]
            let read = entry_at(&path.join(&rel));
            let Ok(facts) = read else {
                return false;
            };
            let kind = facts.meta.file_type();
            let seen = if kind.is_symlink() {
                Seen::Link
            } else if kind.is_dir() {
                Seen::Dir
            } else if kind.is_file() {
                Seen::File
            } else {
                Seen::Other
            };
            if !visit(&rel, seen, &facts, budget) {
                return false;
            }
            if seen == Seen::Dir {
                next.push_back(rel);
            }
        }
    }
    true
}

/// One entry's line in a [`Shape`]: its [`times`], then what tells a change its times cannot (a
/// rewrite of as many bytes in the clock step of the write before it leaves them as they were).
/// On unix that is its inode and status change. On Windows it is its change-journal counter,
/// `usn <journal>.<usn>`, which moves at the first change of each kind made while the file stays
/// open and as its last handle closes (`aterm_dirfd::Usn`), or `no-journal` where its volume
/// keeps none (FAT, exFAT, a network share, a journal turned off) and the line tells only what its
/// times do. Neither counts a rewrite in the clock step of the write before it every time: not on
/// Windows by a writer that held the file open across both, nor on unix where the clock steps
/// coarser than the writes (ATERM-KI-1115-47201).
fn stamp(rel: &Path, seen: Seen, entry: &Entry) -> String {
    #[cfg(unix)]
    let counter = {
        use std::os::unix::fs::MetadataExt as _;
        let meta = &entry.meta;
        format!("{}\t{}.{}", meta.ino(), meta.ctime(), meta.ctime_nsec())
    };
    #[cfg(windows)]
    let counter = entry.usn.map_or_else(
        || "no-journal".to_owned(),
        |usn| format!("usn {:x}.{}", usn.journal, usn.usn),
    );
    #[cfg(not(any(unix, windows)))]
    let counter = String::new();
    format!("{}\t{counter}\n", times(rel, seen, &entry.meta))
}

/// What an entry is, its path, its size and its modified time: the first fields of its
/// [`stamp`].
fn times(rel: &Path, seen: Seen, meta: &std::fs::Metadata) -> String {
    let modified = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |at| at.as_nanos());
    format!(
        "{}\t{}\t{}\t{modified}",
        seen.word(),
        pct::encode(&rel.to_string_lossy()),
        meta.len()
    )
}

/// Claimed folder `path`'s [`Shape`] now, by a walk held to the listing's caps; `None` where the
/// walk was not whole.
fn shape_of(path: &Path, home: Option<&Path>) -> Option<Shape> {
    let mut shape = aterm_digest::Sha256::new();
    let whole = walk_claim(
        path,
        home,
        &mut Budget::new(),
        &mut |rel, seen, entry, _| {
            shape.update(stamp(rel, seen, entry).as_bytes());
            true
        },
    );
    whole.then(|| shape.finalize())
}

/// `job`'s cleanup with `kit`: each step in order, as the module says.
pub(crate) fn run(job: Job, kit: &mut Kit) -> Outcome {
    let Job {
        dir,
        plan,
        actor,
        by,
        flight,
    } = job;
    let request = plan.request;
    let id = request.id.clone();
    let stop = request.clean.as_ref().is_some_and(|clean| clean.stop);
    let uncleaned = |why: Unclean, detail: String| {
        let at = crate::requests_host::now_unix_ms();
        if let Err(error) = crate::requests_host::with_store(&dir, |store| {
            store.end_clean(at, &id, CleanEnd::Uncleaned(why))
        }) {
            aterm_log::warn!("requests: {id}'s uncleaned not recorded: {error}");
        }
        aterm_log::info!("requests: {id} not cleaned (why={}): {detail}", why.word());
        crate::requests_host::relisted(&dir);
        Outcome::Uncleaned { why, detail }
    };
    let standing: Vec<String> = request
        .tabs
        .iter()
        .filter(|tab| tab.is_open())
        .map(|tab| tab.sid.clone())
        .collect();
    let captured: Vec<Captured> = standing
        .iter()
        .filter_map(|sid| (kit.capture)(sid))
        .collect();
    let mut archive = match Archiving::begin(&dir, &id) {
        Ok(archive) => archive,
        Err(error) => {
            return uncleaned(
                Unclean::Store,
                format!("its archive could not be begun, so nothing was closed: {error}"),
            );
        }
    };
    if let Err(error) = archive.first(&dir, &id, &captured) {
        archive.discard();
        return uncleaned(
            Unclean::Store,
            format!("its archive could not be written, so nothing was closed: {error}"),
        );
    }
    let closed = match (kit.close)(&standing, &actor, stop) {
        Ok(closed) => closed,
        Err(why) => {
            archive.discard();
            return uncleaned(Unclean::Closer, format!("its tabs were not closed: {why}"));
        }
    };
    if !closed.busy.is_empty() {
        archive.discard();
        return uncleaned(
            Unclean::Busy,
            format!(
                "a command runs in {}: clean it with stop=1 to end it",
                closed.busy.join(", ")
            ),
        );
    }
    // Only the trees of the tabs that closed: a tab whose close failed stands, its work with it.
    let tree: Vec<Identity> = captured
        .iter()
        .filter(|tab| closed.closed.contains(&tab.sid))
        .flat_map(|tab| tab.tree.iter().copied())
        .collect();
    let asked = end_tree(&tree, kit.end.as_mut(), kit.grace);
    if asked > 0 {
        aterm_log::info!("requests: {id}'s cleanup asked {asked} processes of its tabs to end");
    }
    if !closed.failed.is_empty() {
        let failed: Vec<String> = closed
            .failed
            .iter()
            .map(|(sid, why)| format!("{sid} ({why})"))
            .collect();
        // The screens of the tabs that did close are kept; nothing is deleted while one stands.
        let kept = if closed.closed.is_empty() {
            archive.discard();
            String::new()
        } else {
            record_archive(&dir, &id, archive).map_or_else(String::new, |name| {
                format!(
                    "; the closed tabs' archive is {}/{ARCHIVE}/{name}",
                    aterm_requests::DIR
                )
            })
        };
        return uncleaned(
            Unclean::Closer,
            format!(
                "{} did not close, so nothing was deleted{kept}",
                failed.join(", ")
            ),
        );
    }
    // A person's work in a claimed folder since the minute's last look, which the main thread
    // handed the writer before the tabs closed, is recorded before any folder is judged.
    let looked = crate::requests_host::flush_within(CLEAN_WAIT);
    if !looked {
        aterm_log::warn!("requests: {id}'s cleanup could not read the latest work in its folders");
    }
    let request = current(&dir, &id).unwrap_or(request);
    let home = kit.places.home.clone();
    let now = crate::requests_host::now_unix_ms();
    let mut judged: Vec<(
        aterm_requests::claim::HeldClaim,
        Option<Measured>,
        Option<Keep>,
    )> = Vec::new();
    let mut deleted_before = 0_u64;
    for held in &request.claims {
        let path = held.claim.path.as_str();
        if request
            .gone_of(path)
            .is_some_and(|gone| gone.did == Did::Deleted)
        {
            deleted_before += 1;
            continue;
        }
        if std::fs::symlink_metadata(path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            // Resumed, the cut cleanup deleted it and could not say so; a first cleanup did not.
            let why = (!plan.resumed).then_some(Keep::Missing);
            judged.push((held.clone(), None, why));
            continue;
        }
        let measured =
            crate::requests_footprint::measure_now(now, path, home.as_deref(), kit.measure);
        let keep = if looked {
            let host = HostFacts {
                clean_at_ms: plan.at_ms,
                born_now: crate::requests_footprint::born_ms(Path::new(path)),
                cwds: &closed.cwds,
                listed: true,
                archived: true,
            };
            clean_verdict(&request, held, Some(&measured), &host, now)
        } else {
            Some(Keep::Unmeasured)
        };
        judged.push((held.clone(), Some(measured), keep));
    }
    let mut budget = Budget::new();
    let mut listed: Vec<(String, Option<Shape>)> = Vec::new();
    for (held, measured, keep) in &judged {
        if keep.is_none() && measured.is_some() {
            let shape = archive.list(Path::new(&held.claim.path), home.as_deref(), &mut budget);
            listed.push((held.claim.path.clone(), shape));
        }
    }
    let archived = record_archive(&dir, &id, archive);
    let (mut deleted, mut kept, mut failed) = (deleted_before, 0_u64, 0_u64);
    for (held, measured, keep) in judged {
        let path = held.claim.path.clone();
        let Some(measured) = measured else {
            let did = if keep.is_some() {
                Did::Kept
            } else {
                Did::Deleted
            };
            match did {
                Did::Kept => kept += 1,
                _ => deleted += 1,
            }
            record_gone(&dir, &id, &path, None, keep, did);
            continue;
        };
        let shape = listed
            .iter()
            .find(|(listed, _)| *listed == path)
            .and_then(|(_, shape)| *shape);
        let keep = keep.or_else(|| {
            if shape.is_none() {
                Some(Keep::Unlisted)
            } else if archived.is_none() {
                Some(Keep::Unarchived)
            } else {
                None
            }
        });
        let (did, why) = match (keep, shape) {
            (None, Some(shape)) => remove(&dir, &id, &held, measured, shape, plan.at_ms, kit),
            (why, _) => (Did::Kept, why),
        };
        match did {
            Did::Deleted => deleted += 1,
            Did::Kept => kept += 1,
            Did::Failed => failed += 1,
        }
        record_gone(&dir, &id, &path, measured.bytes, why, did);
    }
    let tabs = closed.closed.len() as u64;
    let at = crate::requests_host::now_unix_ms();
    if let Err(error) = crate::requests_host::with_store(&dir, |store| {
        store.end_clean(
            at,
            &id,
            CleanEnd::Cleaned {
                tabs,
                deleted,
                kept,
                failed,
                by: by.clone(),
            },
        )
    }) {
        aterm_log::warn!("requests: {id}'s cleanup not recorded: {error}");
    }
    crate::requests_host::relisted(&dir);
    let archive = archived.map_or_else(
        || "-".to_owned(),
        |name| format!("{}/{ARCHIVE}/{name}", aterm_requests::DIR),
    );
    aterm_log::info!(
        "requests: {id} cleaned by {by}: {tabs} tabs closed, {deleted} folders deleted, {kept} \
         kept, {failed} failed (archive {archive})"
    );
    drop(flight);
    Outcome::Cleaned {
        tabs,
        deleted,
        kept,
        failed,
        archive,
    }
}

/// Finish `archive` of request `id` (the store in `dir`) and record it (`archived`): its folder's
/// name, or `None` where it was not written or not recorded.
fn record_archive(dir: &Path, id: &str, archive: Archiving) -> Option<String> {
    match archive.finish(crate::requests_host::now_unix_ms()) {
        Ok(archive) => {
            match crate::requests_host::with_store(dir, |store| store.archived(id, &archive)) {
                Ok(()) => Some(archive.dir),
                Err(error) => {
                    aterm_log::warn!("requests: {id}'s archive not recorded: {error}");
                    None
                }
            }
        }
        Err(error) => {
            aterm_log::warn!("requests: {id}'s archive not written: {error}");
            None
        }
    }
}

/// Request `id` of the store in `dir` as it stands now.
fn current(dir: &Path, id: &str) -> Option<Request> {
    crate::requests_host::with_store(dir, |store| {
        let _ = store.inspect()?;
        Ok(store.get(id).cloned())
    })
    .ok()
    .flatten()
}

/// Record what the cleanup of `id` did with its claimed folder `path`.
fn record_gone(dir: &Path, id: &str, path: &str, bytes: Option<u64>, why: Option<Keep>, did: Did) {
    let gone = GoneClaim {
        at_ms: crate::requests_host::now_unix_ms(),
        path: path.to_owned(),
        bytes,
        why,
        did,
    };
    if let Err(error) = crate::requests_host::with_store(dir, |store| store.gone(id, &gone)) {
        aterm_log::warn!(
            "requests: {id}'s {} {} not recorded: {error}",
            pct::encode(path),
            did.word()
        );
    }
}

/// Delete claimed folder `held` of request `id` (the store in `dir`), judged again first, as it
/// stands now: the request as the store reads it (kept since, or the folder touched), the folder
/// still the one its path names with no link on the way and never one a claim may not be
/// ([`Places::refusal`]), unchanged since its listing (`shape`), its repos read again by `kit`'s
/// measure (`measured` otherwise), its birth unmoved, and no tab standing in it now (`kit`'s
/// cwds); the cleanup began at `clean_at`. What was done, and why where it was kept.
fn remove(
    dir: &Path,
    id: &str,
    held: &aterm_requests::claim::HeldClaim,
    measured: Measured,
    shape: Shape,
    clean_at: u64,
    kit: &Kit,
) -> (Did, Option<Keep>) {
    let path = Path::new(&held.claim.path);
    let home = kit.places.home.as_deref();
    let Some(request) = current(dir, id) else {
        return (Did::Kept, Some(Keep::Unmeasured));
    };
    let real = crate::requests_footprint::walk(home, path);
    let plain = std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
    if kit.places.refusal(path).is_some() || real.as_deref() != Ok(path) || !plain {
        return (Did::Kept, Some(Keep::Replaced));
    }
    // Anything written, made or removed in it since its listing (a commit included): the
    // manifest no longer describes it.
    if shape_of(path, home) != Some(shape) {
        return (Did::Kept, Some(Keep::Changed));
    }
    let now = crate::requests_host::now_unix_ms();
    let measured = Measured {
        at_ms: now,
        unpushed: (kit.measure.repos)(path, home),
        ..measured
    };
    let cwds = (kit.cwds)();
    let host = HostFacts {
        clean_at_ms: clean_at,
        born_now: crate::requests_footprint::born_ms(path),
        cwds: &cwds,
        listed: true,
        archived: true,
    };
    if let Some(keep) = clean_verdict(&request, held, Some(&measured), &host, now) {
        return (Did::Kept, Some(keep));
    }
    // `remove_dir_all` follows no link: one inside is removed as a link.
    match std::fs::remove_dir_all(path) {
        Ok(()) => (Did::Deleted, None),
        Err(error) => {
            aterm_log::warn!(
                "requests: {id}'s {} not wholly deleted: {error}",
                pct::encode(&held.claim.path)
            );
            (Did::Failed, None)
        }
    }
}

/// What the main thread's close of a request's tabs found, before a window's teardown ran.
pub(crate) struct Closing {
    busy: Vec<String>,
    tried: Vec<(u64, String, Result<crate::app_tabs::CloseProgress, String>)>,
    /// The close's attribution and act, and the close policy it replaced: held until
    /// [`crate::App::request_tabs_closed`], so a tab a window's teardown closes after is the
    /// cleanup's close too.
    held: Option<(
        crate::session_store::CloseAttribution,
        crate::tab_marks::ActScope,
        crate::app_window::CloseConfirm,
    )>,
}

impl crate::App {
    /// Close request tabs `tabs` (local id and sid) by `by`, as a cleanup asks: where `force` is
    /// false and a command runs in any, none is closed. First, the minute's footprint look runs
    /// now, so a person's latest work in a claimed folder, in these tabs as in any, reaches the
    /// requests' writer before the cleanup judges the folder.
    pub(crate) fn close_request_tabs(
        &mut self,
        tabs: &[(u64, String)],
        by: &ExitActor,
        force: bool,
    ) -> Closing {
        let handles: Vec<crate::session_store::SessionHandle> = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store.live_handles().cloned().collect()
        };
        self.look_at_footprints(&handles, crate::requests_host::now_unix_ms());
        if !force {
            let busy: Vec<String> = tabs
                .iter()
                .filter(|(session, _)| self.session_foreground_busy(*session))
                .map(|(_, sid)| sid.clone())
                .collect();
            if !busy.is_empty() {
                return Closing {
                    busy,
                    tried: Vec::new(),
                    held: None,
                };
            }
        }
        let closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            by.clone(),
        );
        let act = crate::tab_marks::ActScope::closing(by.clone(), "cleanup");
        let policy = std::mem::replace(
            &mut self.close_confirm,
            if force {
                crate::app_window::CloseConfirm::Programmatic
            } else {
                crate::app_window::CloseConfirm::WireRefuseBusy
            },
        );
        let tried = tabs
            .iter()
            .map(|(session, sid)| (*session, sid.clone(), self.close_session_by_id(*session)))
            .collect();
        Closing {
            busy: Vec::new(),
            tried,
            held: Some((closing, act, policy)),
        }
    }

    /// What `closing` did, once any window's teardown it began ran: each tab closed or why not,
    /// and the working folder of every tab that still stands. Its attribution ends here, and the
    /// close policy it replaced is back.
    pub(crate) fn request_tabs_closed(&mut self, closing: Closing) -> Closed {
        let mut closed = Closed {
            busy: closing.busy,
            ..Closed::default()
        };
        for (session, sid, progress) in closing.tried {
            match progress.and_then(|progress| self.close_session_verdict(session, progress)) {
                Ok(()) => closed.closed.push(sid),
                Err(why) => closed.failed.push((sid, why)),
            }
        }
        if let Some((attribution, act, policy)) = closing.held {
            self.close_confirm = policy;
            drop(act);
            drop(attribution);
        }
        closed.cwds = standing_cwds(&self.store);
        closed
    }
}

#[cfg(test)]
#[path = "requests_clean_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S SESSION TABLE (tracker ST-15; decisions ST-D6, ST-D14, ST-D21;
//! `docs/DESIGN-streams-2026-11-12.md` §5): one row per ended session, on disk in its own store,
//! `<state>/sessions/<sid>.toml`, one file per session, written by an atomic rename. Every end
//! leaves a row, a person's own close included: its ID, the title standing at the end (never a
//! command line, and redacted), who opened it, when it opened and ended, who ended it and why,
//! the task's end, the close's totals, the last folder, and the recorded launch of its last agent.
//! `aterm ctl exits` reads the table, so it answers after a quit, and `task`, `result` and `await
//! task` naming an ended session answer from its row (`control::ended_record`). A live session's
//! row joins the same store as the same row (ST-24).
//!
//! A row stays until a person removes it ([`SessionTable::remove`], History's act, ST-33), under
//! one byte budget ([`TABLE_BUDGET_BYTES`]) that counts each row with the result file and sidecar
//! it keeps, oldest ended first. A row a live session names as its creator (until ST-06, a live
//! session whose task's `from=` is the row's session), and a concluded row whose live creator has
//! not been told its conclusion (`worker_mail::Told`), is not dropped. The result a row's task
//! names lives exactly as long as the row: a close keeps it ([`crate::task_launch::Kept`] consults
//! the table), and the row's removal or eviction removes it first, then the row.
//!
//! The close path only hands the row over ([`SessionTable::write`]): the table indexes its head at
//! once, so the close's own sweep of the tasks directory keeps what it names, and a worker thread
//! writes the file and runs the budget (its evictions and their result files included), never the
//! window's thread. A thread of its own reads the rows earlier runs left, so a write never waits
//! behind that read. The budget runs only once it has the whole table and the launch's sessions
//! are registered ([`SessionTable::sessions_settled`]), since live sessions protect rows. Other
//! processes on the same state root (a seamless update's predecessor and successor, a second
//! instance) write and remove rows in the same folder: a lookup the index misses reads the row's
//! file, a head whose file went is dropped, and `exits` lists the folder again where it moved.
//!
//! The quit, and an update's exit, wait for the worker at most [`FLUSH_WAIT`]
//! ([`SessionTable::flush`]), writing again any row whose write failed. A row still unwritten
//! then is lost with the process, and the run's end says so; a temporary file a write cut short
//! leaves is swept at the next load. A table dropped with its last holder waits for its worker. A
//! launch with no state directory keeps its rows in memory ([`SessionTable::default`]).
//!
//! Not every end leaves a row yet: a crash ends its sessions with no close, and one shortly after a
//! close can come before the worker wrote that close's row (`docs/KNOWN_ISSUES.md`
//! ATERM-KI-1114-715).
//!
//! Each row is TOML, `v = 1`; a later field is an added key an older reader ignores, so what the
//! design's row also names comes as such a key with the package that has it: the summary (ST-27)
//! and the agent's model (with the harness's snapshot of it); the agent's kind and version are in
//! its `[launch]` already. A row past [`ROW_MAX_BYTES`] gives up, in order, the long words of its
//! agent's argv (a first prompt; the resume names the conversation), its folder, its launch, then
//! its task's program, trusted folder and line, and says what it gave up (`trimmed`): the row is
//! never lost.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, Weak, mpsc};
use std::time::{Duration, Instant};

use aterm_session::SessionId;
use serde::{Deserialize, Serialize};

use crate::restore::AgentRestore;
use crate::session_log::TabTotals;
use crate::session_log::observed::TitleStands;
use crate::session_store::{Ended, ExitReason, SessionStore, Store};
use crate::task_record::TaskRecord;

/// The most bytes one row's file takes.
pub(crate) const ROW_MAX_BYTES: usize = 16 * 1024;
/// The one byte budget (ST-D14): every row with the result file and sidecar it keeps.
pub(crate) const TABLE_BUDGET_BYTES: u64 = 16 << 20;
/// The most rows kept, a listing's bound beside the byte budget.
pub(crate) const ROWS_MAX: usize = 8192;
/// How long a reader of every row (`exits`, History), and the quit's sweep of the tasks
/// directory, wait for the launch's load.
pub(crate) const LOAD_WAIT: Duration = Duration::from_secs(2);
/// How long the quit and an update's exit wait for the table's writes to reach disk. In a test it
/// is a hang detector of a minute, never a latency budget.
pub(crate) const FLUSH_WAIT: Duration = if cfg!(test) {
    Duration::from_secs(60)
} else {
    Duration::from_secs(2)
};
/// The most bytes of a row's title.
const TITLE_MAX: usize = 256;
/// The most bytes of a row's folder; a longer one is not kept.
const CWD_MAX: usize = 1024;
/// An argv word longer than this is the first to go from a row past [`ROW_MAX_BYTES`].
const ARGV_WORD_MAX: usize = 1024;
/// The row format's version.
const ROW_V: u32 = 1;
/// The coarsest modification-time tick a volume keeps (FAT's two seconds).
const MTIME_TICK: Duration = Duration::from_secs(2);
/// The folder under the state directory.
const DIR_NAME: &str = "sessions";

/// Which rung of a tab's title ladder a row's title is
/// (`app_tabs::resolved_terminal_title_rung`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TitleFrom {
    /// A title set on the session (`meta set title`, a rename).
    User,
    /// The title its program gave it.
    Program,
    /// Its task's first line.
    Task,
    /// Its last folder.
    Folder,
    /// Nothing named it.
    None,
}

impl TitleFrom {
    const ALL: [Self; 5] = [
        Self::User,
        Self::Program,
        Self::Task,
        Self::Folder,
        Self::None,
    ];

    const fn word(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Program => "program",
            Self::Task => "task",
            Self::Folder => "folder",
            Self::None => "none",
        }
    }

    fn parse(word: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|from| from.word() == word)
            .unwrap_or(Self::None)
    }
}

/// One ended session's row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    /// The session's ID, the durable one (LT-D10).
    pub(crate) sid: SessionId,
    /// Its `exits` row id: an ordering cursor: the end's unix µs, or one above the highest id held.
    pub(crate) id: u64,
    /// Wall ms of the end.
    pub(crate) ended_ms: u64,
    /// Wall ms of its open: its tab's, else its log's start.
    pub(crate) opened_ms: Option<u64>,
    /// Who opened it (its open mark's `by=`; ST-06 makes it its creator).
    pub(crate) opened_by: Option<aterm_slog::Actor>,
    /// Why it ended.
    pub(crate) out: ExitReason,
    /// Who ended it, as its wire word (`ExitActor::as_wire`).
    pub(crate) by: String,
    /// The proven node of a fleet drive that ended it.
    pub(crate) by_node: Option<String>,
    /// The act of aterm's own that ended it (`tab_marks::close_why`).
    pub(crate) why: Option<String>,
    /// Its shell's exit code.
    pub(crate) exit_code: Option<i32>,
    /// Its log's epoch at the end (a reopen opens above it, ST-17).
    pub(crate) ep: Option<u32>,
    /// The process that wrote it (its table's [`instance`], set as the table takes it) and the
    /// local id it had there: `@<local>` names it only in that process.
    pub(crate) inst: Option<String>,
    pub(crate) local: Option<u64>,
    /// The title standing at its end ([`standing_title`]).
    pub(crate) title: String,
    pub(crate) title_from: TitleFrom,
    /// Its last folder.
    pub(crate) cwd: Option<String>,
    /// Its task record as it ended.
    pub(crate) task: Option<TaskRecord>,
    /// Its creator was told its conclusion (`worker_mail::Told`): its protection lapsed.
    pub(crate) told_end: bool,
    /// The close's totals.
    pub(crate) totals: TabTotals,
    /// The recorded launch of its last agent (`harness_host::take_last_launch`).
    pub(crate) launch: Option<AgentRestore>,
    /// What the size bound took from it ([`ROW_MAX_BYTES`]).
    pub(crate) trimmed: Vec<String>,
}

/// What the table's index keeps of a row: what `exits`, a listing and the budget read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowHead {
    /// Identity of this indexed write, independent of its exit cursor. A stat keeps it alive
    /// so a replacement cannot reuse its identity while that stat is in flight.
    generation: Arc<()>,
    pub(crate) sid: SessionId,
    pub(crate) id: u64,
    pub(crate) ended_ms: u64,
    pub(crate) out: ExitReason,
    pub(crate) by: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) inst: Option<String>,
    pub(crate) local: Option<u64>,
    /// Its task's creator (`from=`).
    pub(crate) from: Option<String>,
    /// Its task concluded.
    pub(crate) concluded: bool,
    pub(crate) told_end: bool,
    /// The result file its task names.
    pub(crate) result_path: Option<PathBuf>,
    /// Its charge against the budget: its file, and its result and sidecar.
    pub(crate) bytes: u64,
}

impl Row {
    fn head(&self, bytes: u64) -> RowHead {
        RowHead {
            generation: Arc::new(()),
            sid: self.sid.clone(),
            id: self.id,
            ended_ms: self.ended_ms,
            out: self.out,
            by: self.by.clone(),
            exit_code: self.exit_code,
            inst: self.inst.clone(),
            local: self.local,
            from: self.task.as_ref().map(|task| task.from.clone()),
            concluded: self
                .task
                .as_ref()
                .is_some_and(|task| task.conclusion().is_some()),
            told_end: self.told_end,
            result_path: self.task.as_ref().and_then(|task| task.result_path.clone()),
            bytes,
        }
    }

    /// The row as its file holds it, within [`ROW_MAX_BYTES`] where giving up what it may lets
    /// it fit.
    pub(crate) fn encode(&self) -> String {
        let mut row = self.clone();
        let mut text = row.file().encode();
        let trims: [(&str, fn(&mut Row) -> bool); 4] = [
            ("argv", |row| {
                let Some(launch) = row.launch.as_mut() else {
                    return false;
                };
                let before = launch.argv.len();
                launch.argv.retain(|word| word.len() <= ARGV_WORD_MAX);
                launch.argv.len() != before
            }),
            ("cwd", |row| row.cwd.take().is_some()),
            ("launch", |row| row.launch.take().is_some()),
            ("task", |row| {
                row.task.as_mut().is_some_and(|task| {
                    let had =
                        task.program.is_some() || task.trusted_cwd.is_some() || task.line.is_some();
                    task.program = None;
                    task.trusted_cwd = None;
                    task.line = None;
                    had
                })
            }),
        ];
        for (word, trim) in trims {
            if text.len() <= ROW_MAX_BYTES {
                break;
            }
            if trim(&mut row) {
                row.trimmed.push(word.to_owned());
                text = row.file().encode();
            }
        }
        text
    }

    fn file(&self) -> RowFile {
        RowFile {
            v: ROW_V,
            sid: self.sid.as_str().to_owned(),
            id: self.id,
            ended_ms: self.ended_ms,
            opened_ms: self.opened_ms,
            opened_by: self.opened_by.map(|by| by.word().to_owned()),
            out: self.out.as_str().to_owned(),
            by: self.by.clone(),
            by_node: self.by_node.clone(),
            why: self.why.clone(),
            exit_code: self.exit_code,
            ep: self.ep,
            inst: self.inst.clone(),
            local: self.local,
            title: self.title.clone(),
            title_from: self.title_from.word().to_owned(),
            cwd: self.cwd.clone(),
            task: self
                .task
                .as_ref()
                .map(|task| TaskRecord::carry_face(&task.carry_row(Instant::now()))),
            told_end: self.told_end,
            trimmed: self.trimmed.clone(),
            totals: TotalsFile {
                wall_s: self.totals.wall_s,
                cpu_s: self.totals.cpu_s,
                peak_mb: self.totals.peak_mb,
                tokens: self.totals.tokens,
                account: self.totals.account.clone(),
            },
            launch: self.launch.clone(),
        }
    }

    /// The row file `text` holds, its task's result settled against the tasks directory `tasks`
    /// (`TaskRecord::settle_result`): `None` for one that does not read, or names no session.
    pub(crate) fn decode(text: &str, tasks: Option<&Path>) -> Option<Self> {
        let file: RowFile = aterm_toml::from_str(text).ok()?;
        let sid = SessionId::parse(&file.sid)?;
        let task = file.task.as_deref().and_then(|row| {
            let now_ms = aterm_time::unix_millis().unwrap_or(0);
            let mut task = TaskRecord::from_carry_row(row, tasks, Instant::now(), now_ms)?;
            task.settle_result(tasks);
            Some(task)
        });
        Some(Self {
            sid,
            id: file.id,
            ended_ms: file.ended_ms,
            opened_ms: file.opened_ms,
            opened_by: file.opened_by.as_deref().map(aterm_slog::Actor::from_word),
            out: ExitReason::parse(&file.out).unwrap_or(ExitReason::Unknown),
            by: file.by,
            by_node: file.by_node,
            why: file.why,
            exit_code: file.exit_code,
            ep: file.ep,
            inst: file.inst,
            local: file.local,
            title: file.title,
            title_from: TitleFrom::parse(&file.title_from),
            cwd: file.cwd,
            task,
            told_end: file.told_end,
            totals: TabTotals {
                wall_s: file.totals.wall_s,
                cpu_s: file.totals.cpu_s,
                peak_mb: file.totals.peak_mb,
                tokens: file.totals.tokens,
                account: file.totals.account,
            },
            launch: file.launch,
            trimmed: file.trimmed,
        })
    }
}

/// A row's file (`v = 1`; keys are only added).
#[derive(Serialize, Deserialize)]
struct RowFile {
    v: u32,
    sid: String,
    id: u64,
    ended_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opened_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opened_by: Option<String>,
    out: String,
    by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    by_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ep: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inst: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    title_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    /// The task record's carry row without its ages (`TaskRecord::carry_face`): read days later,
    /// its times go by the wall clock's gap from `since=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    told_end: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    trimmed: Vec<String>,
    #[serde(default)]
    totals: TotalsFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch: Option<AgentRestore>,
}

impl RowFile {
    fn encode(&self) -> String {
        aterm_toml::to_string(self).unwrap_or_default()
    }
}

/// A row's `[totals]`: each key absent where aterm could not read it, never `0`.
#[derive(Serialize, Deserialize, Default)]
struct TotalsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wall_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cpu_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peak_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

/// What a close reads of its session's terminal and meta before the registry lets it go.
#[derive(Clone, Debug)]
pub(crate) struct AtClose {
    /// A title set on the session.
    pub(crate) user_title: Option<String>,
    /// The terminal's live title, and what it stands for.
    pub(crate) live_title: String,
    pub(crate) stands: TitleStands,
    /// Nothing the tab runs gave the title (`app_take_over::untitled_by_its_program`).
    pub(crate) untitled: bool,
    /// Its last folder.
    pub(crate) cwd: Option<String>,
}

impl AtClose {
    /// Read session `ctx`'s terminal `term` and its meta.
    pub(crate) fn read(
        term: &Arc<Mutex<aterm_core::terminal::Terminal>>,
        ctx: &crate::SessionCtx,
    ) -> Self {
        let user_title = ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .presentation_value("title");
        let cwd = crate::sys_host::cwd_of(term);
        let t = term.lock().unwrap_or_else(PoisonError::into_inner);
        Self {
            user_title,
            live_title: t.title().to_owned(),
            stands: crate::session_status::title_stands(&t),
            untitled: crate::app_take_over::untitled_by_its_program(&t),
            cwd,
        }
    }
}

/// The title a row keeps, by the tab's own ladder (`app_tabs::resolved_terminal_title_rung`):
/// the session's set title, else its program's title where that is not a command line
/// (`TitleStands::CommandLine` is never kept), else its task's first line, else its folder.
/// Every title is redacted (`survey::redact`): until a person's title can be told from a
/// driver's (ST-28), a person's own loses only its credential-shaped words. At most
/// [`TITLE_MAX`] bytes.
pub(crate) fn standing_title(at: &AtClose, task_line: Option<&str>) -> (String, TitleFrom) {
    let live = match at.stands {
        TitleStands::Own => at.live_title.as_str(),
        TitleStands::CommandLine => "",
    };
    let Some(title) = crate::app_tabs::resolved_terminal_title_rung(
        at.user_title.as_deref(),
        live,
        at.untitled,
        task_line,
        at.cwd.as_deref(),
    ) else {
        return (String::new(), TitleFrom::None);
    };
    let from = if at.user_title.as_deref() == Some(title.as_str()) {
        TitleFrom::User
    } else if !live.is_empty() && crate::tab_label::informative(live).to_string() == title {
        TitleFrom::Program
    } else if task_line == Some(title.as_str()) {
        TitleFrom::Task
    } else {
        TitleFrom::Folder
    };
    let mut title = crate::survey::redact(&title);
    if title.len() > TITLE_MAX {
        let mut cut = TITLE_MAX;
        while !title.is_char_boundary(cut) {
            cut -= 1;
        }
        title.truncate(cut);
    }
    (title, from)
}

/// The row a close leaves: session `sid`'s end as its close read it under its timeline's lock
/// (`ended`) and what it read of its terminal (`at`), with the launch of the last agent it ran,
/// taken from the harness's record (`harness_host::take_last_launch`). Its task's first line is
/// redacted as its title is ([`standing_title`]): it may be a driver's words, and the row keeps it
/// far longer than the session did.
pub(crate) fn row_at_close(sid: &SessionId, ended: &Ended, at: &AtClose) -> Row {
    let task_line = ended.task.as_ref().and_then(|task| task.line.as_deref());
    let (title, title_from) = standing_title(at, task_line);
    let mut task = ended.task.clone();
    if let Some(task) = task.as_mut() {
        task.line = task.line.as_deref().map(crate::survey::redact);
    }
    let tab = ended.tab.as_ref();
    let launch = crate::harness_host::take_last_launch(sid.as_str()).map(|mut launch| {
        launch.argv = launch
            .argv
            .iter()
            .map(|word| crate::survey::redact(word))
            .collect();
        launch
    });
    Row {
        sid: sid.clone(),
        id: 0,
        ended_ms: ended.ended_ms,
        opened_ms: tab.map(|tab| tab.opened_wall).or(ended.started_ms),
        opened_by: tab.and_then(|tab| tab.opened_by),
        out: ended.out,
        by: ended.by.as_wire().to_owned(),
        by_node: ended.by.fleet_node().map(str::to_owned),
        why: ended.why.map(str::to_owned),
        exit_code: ended.exit_code,
        ep: Some(ended.ep),
        inst: None,
        local: Some(ended.local),
        title,
        title_from,
        cwd: at.cwd.clone().filter(|cwd| cwd.len() <= CWD_MAX),
        told_end: ended
            .task
            .as_ref()
            .is_some_and(|task| task.told.told_end(task.entered)),
        task,
        totals: tab.map(|tab| tab.totals.clone()).unwrap_or_default(),
        launch,
        trimmed: Vec::new(),
    }
}

/// This process's instance: a row it wrote answers `@<local>` here alone.
#[cfg(not(test))]
fn instance() -> String {
    crate::control_auth::process_instance_id().to_owned()
}

/// A test's process instance: each table a test opens stands for a process of its own, so a
/// table reopened over the same folder reads its rows as another process's.
#[cfg(test)]
fn instance() -> String {
    static TABLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}",
        crate::control_auth::process_instance_id(),
        TABLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// A conclusion a worker's mail told its creator (`worker_mail::Owed::told`): its task's start and
/// when it entered that state, the two a row's task record keeps exactly (`TaskRecord::carry_row`'s
/// `started=` and `since=`; the rev it entered at is not, a carried record entering at its rev).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Told {
    pub(crate) started_ms: u64,
    pub(crate) since_ms: u64,
}

impl Told {
    /// Whether `task` concluded with this conclusion.
    fn is_of(self, task: &TaskRecord) -> bool {
        task.conclusion().is_some()
            && task.started_unix_ms == self.started_ms
            && task.since_unix_ms == self.since_ms
    }
}

/// The table is still loading, past what a reader waits.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Loading;

/// What a set of live sessions protects of the table: each row a live session's task names as its
/// creator, and each concluded row whose live creator was not told its conclusion.
#[derive(Default)]
struct Live {
    sessions: HashSet<String>,
    creators: HashSet<String>,
}

impl Live {
    fn of(store: &Store) -> Self {
        let handles = store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();
        let mut live = Self::default();
        for handle in handles {
            if let Some(task) = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .task()
            {
                live.creators.insert(task.from.clone());
            }
            live.sessions.insert(handle.sid.as_str().to_owned());
        }
        live
    }

    fn protects(&self, head: &RowHead) -> bool {
        self.creators.contains(head.sid.as_str())
            || (head.concluded
                && !head.told_end
                && head
                    .from
                    .as_deref()
                    .is_some_and(|from| self.sessions.contains(from)))
    }
}

/// What the table keeps in tasks directory `dir`, asked of its index as a sweep of that directory
/// meets each file ([`SessionTable::result_keeps`]): no copy of the index is taken.
pub(crate) struct ResultKeeps {
    shared: Arc<Shared>,
    dir: PathBuf,
}

impl ResultKeeps {
    /// Whether file `name` of the tasks directory is a result a row keeps, or its sidecar: one a
    /// row names, by whatever name (`task mark result_path=` takes any file in the directory);
    /// while the table has not loaded, every file but a session's brief
    /// (`task_launch::is_brief`), which no row keeps; and a snapshot of a session the index does
    /// not answer for whose row file stands (another process's row).
    pub(crate) fn holds(&self, name: &str) -> bool {
        let owner = crate::task_launch::result_owner(name);
        let unknown = {
            let state = self.shared.lock();
            if state.results.contains_key(&self.dir.join(name)) {
                return true;
            }
            if !state.loaded {
                return !crate::task_launch::is_brief(name);
            }
            let Some(owner) = owner else {
                return false;
            };
            !state.heads.contains_key(owner) && !state.dropping.contains(owner)
        };
        unknown
            && owner
                .and_then(|owner| self.shared.path_of(owner))
                .is_some_and(|path| path.is_file())
    }
}

fn row_file_name(sid: &str) -> String {
    format!("{sid}.toml")
}

/// The session a row file's name (`<sid>.toml`) is of; `None` for any other name.
fn row_sid(name: &str) -> Option<&str> {
    name.strip_suffix(".toml")
        .filter(|sid| crate::keeper_link::valid_sid(sid))
}

/// The process that writes a row's temporary file (`<sid>.toml.tmp-<pid>`, [`write_atomically`]);
/// `None` for any other name.
fn tmp_writer(name: &str) -> Option<u32> {
    let (row, pid) = name.rsplit_once(".tmp-")?;
    row_sid(row)?;
    pid.parse().ok()
}

/// What the budget holds the table to: [`TABLE_BUDGET_BYTES`] and [`ROWS_MAX`], smaller in a test.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    pub(crate) bytes: u64,
    pub(crate) rows: usize,
}

impl Budget {
    /// The machine's: [`TABLE_BUDGET_BYTES`] and [`ROWS_MAX`].
    pub(crate) const MACHINE: Self = Self {
        bytes: TABLE_BUDGET_BYTES,
        rows: ROWS_MAX,
    };
}

/// The index and what the worker has in hand.
#[derive(Default)]
struct State {
    loaded: bool,
    /// The launch's sessions are registered ([`SessionTable::sessions_settled`]): until then the
    /// budget judges nothing, since the live sessions that protect rows may not be in the store.
    settled: bool,
    /// What waits for the load to end ([`SessionTable::when_loaded`]).
    after_load: Vec<Box<dyn FnOnce() + Send>>,
    /// The folder's modification time when [`Shared::refresh`] last listed it.
    listed: Option<std::time::SystemTime>,
    /// A write or a removal reached the folder since its last flush.
    dirty: bool,
    heads: HashMap<String, RowHead>,
    /// Every head by `(id, sid)`, oldest first.
    order: BTreeSet<(u64, String)>,
    /// The rows not on disk yet (all of them, for a table with no folder).
    rows: HashMap<String, Row>,
    /// The id the last row took, at least every id the index holds or held: a new row's id is
    /// above them all, the wall clock stepping back included ([`Self::next_id`]).
    last_id: u64,
    /// The conclusions told of sessions whose row the index did not hold when they were told
    /// ([`SessionTable::told_end`]): a row that comes, or that the load reads, of that conclusion
    /// says it was told.
    told: HashMap<String, Told>,
    /// Rows being removed: their heads are gone, their files not yet.
    dropping: HashSet<String>,
    /// Rows removed before the load ended: the load, which may have read them first, keeps none.
    gone: HashSet<String>,
    /// The result files the heads name, and their sidecars, each with how many heads name it.
    results: HashMap<PathBuf, usize>,
    /// Σ the heads' bytes.
    bytes: u64,
}

impl State {
    fn index(&mut self, mut head: RowHead) {
        head.generation = Arc::new(());
        let key = head.sid.as_str().to_owned();
        if let Some(old) = self.heads.remove(&key) {
            self.order.remove(&(old.id, key.clone()));
            self.bytes = self.bytes.saturating_sub(old.bytes);
            self.keep(&old, false);
        }
        self.order.insert((head.id, key.clone()));
        self.bytes = self.bytes.saturating_add(head.bytes);
        self.last_id = self.last_id.max(head.id);
        self.keep(&head, true);
        self.heads.insert(key, head);
    }

    fn unindex(&mut self, sid: &str) -> Option<RowHead> {
        let head = self.heads.remove(sid)?;
        self.order.remove(&(head.id, sid.to_owned()));
        self.bytes = self.bytes.saturating_sub(head.bytes);
        self.keep(&head, false);
        Some(head)
    }

    /// Count `head`'s result and sidecar in [`Self::results`] (`add`), or no longer.
    fn keep(&mut self, head: &RowHead, add: bool) {
        let Some(path) = head.result_path.as_deref() else {
            return;
        };
        for named in [
            path.to_path_buf(),
            aterm_agent::harness::result::sidecar_of(path),
        ] {
            if add {
                *self.results.entry(named).or_default() += 1;
            } else if let std::collections::hash_map::Entry::Occupied(mut slot) =
                self.results.entry(named)
            {
                *slot.get_mut() -= 1;
                if *slot.get() == 0 {
                    slot.remove();
                }
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        let now = aterm_time::since_epoch()
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
        self.last_id = now.max(self.last_id.saturating_add(1));
        self.last_id
    }
}

/// What the worker is asked.
enum Job {
    /// Write the row in hand for this session.
    Write(String),
    /// The heads of the rows earlier runs left here, as the load read them ([`Shared::read_rows`]):
    /// index them, tell those whose conclusion was told before, then run the budget over the
    /// whole table where the launch's sessions are registered.
    Loaded(Vec<RowHead>),
    /// Read the rows earlier runs left here, where no thread of its own could, then as
    /// [`Self::Loaded`].
    Load,
    /// The launch's sessions are registered: run the budget ([`SessionTable::sessions_settled`]).
    Retain,
    /// Remove this session's result files, then its row file.
    #[cfg(test)]
    Drop(RowHead),
    /// Write again each row still in hand, bring the folder's writes to disk, and answer whether
    /// every row is on disk.
    Flush(mpsc::Sender<bool>),
    /// A loaded table runs a deferred sweep on its worker, never on the window's thread.
    AfterLoad(Box<dyn FnOnce() + Send>),
}

/// The table's state, shared with its worker.
struct Shared {
    /// `<state>/sessions/`; `None` for a table kept in memory.
    dir: Option<PathBuf>,
    /// The tasks directory its rows' results live in.
    tasks: Option<PathBuf>,
    /// The process this table's rows are written by ([`instance`]).
    inst: String,
    /// What the budget holds the table to.
    budget: Budget,
    state: Mutex<State>,
    moved: Condvar,
    /// The store whose live sessions protect rows and whose records keep result files.
    store: Mutex<Weak<RwLock<SessionStore>>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn store(&self) -> Option<Store> {
        self.store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
    }

    /// The row file of `sid`.
    fn path_of(&self, sid: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(row_file_name(sid)))
    }

    /// The heads of every row file in the folder. A temporary file a write of a process that has
    /// since ended left ([`write_atomically`]) goes.
    fn read_rows(&self) -> Vec<RowHead> {
        let mut heads = Vec::new();
        if let Some(dir) = &self.dir {
            match std::fs::read_dir(dir) {
                Ok(entries) => {
                    for entry in entries.flatten().take(ROWS_MAX.saturating_mul(2)) {
                        let name = entry.file_name();
                        let Some(name) = name.to_str() else {
                            continue;
                        };
                        if let Some(pid) = tmp_writer(name) {
                            if pid != std::process::id()
                                && !aterm_uds::process::pid_alive(pid)
                                && let Err(error) = std::fs::remove_file(entry.path())
                                && error.kind() != std::io::ErrorKind::NotFound
                            {
                                aterm_log::warn!(
                                    "session table: {} not removed: {error}",
                                    entry.path().display()
                                );
                            }
                            continue;
                        }
                        let Some(sid) = row_sid(name) else {
                            continue;
                        };
                        let Ok(text) = std::fs::read_to_string(entry.path()) else {
                            continue;
                        };
                        match Row::decode(&text, self.tasks.as_deref()) {
                            Some(row) if row.sid.as_str() == sid => {
                                let bytes = self.charge(text.len(), &row);
                                heads.push(row.head(bytes));
                            }
                            _ => aterm_log::warn!(
                                "session table: the row {} does not read; it is left as it is",
                                entry.path().display()
                            ),
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => aterm_log::warn!(
                    "session table: {} not read: {error}; rows written now are kept",
                    dir.display()
                ),
            }
        }
        heads
    }

    /// Index `heads`, as the load read them ([`Self::read_rows`]), beside the rows this run wrote
    /// or removed since: the table is loaded. A row this run wrote before the load at an id not
    /// above every row the load read (the wall clock stepped back since that row) takes the next
    /// id under the same lock that says the table is loaded, so it stays below every row written
    /// after, and is written again; no reader listed it before the load. Then what waited for the
    /// load runs ([`SessionTable::when_loaded`]). On the worker, which therefore holds every row
    /// this run wrote since it last wrote one in hand ([`State::rows`]).
    fn merge(&self, heads: Vec<RowHead>) {
        let ours: HashSet<String> = self.lock().heads.keys().cloned().collect();
        let floor = heads
            .iter()
            .filter(|head| !ours.contains(head.sid.as_str()))
            .map(|head| head.id)
            .max()
            .unwrap_or(0);
        let low = |state: &State| -> Vec<String> {
            state
                .order
                .iter()
                .take_while(|(id, _)| *id <= floor)
                .map(|(_, sid)| sid.clone())
                .collect()
        };
        // The rows of these already on disk, read before the lock is taken.
        let read = low(&self.lock());
        let on_disk: HashMap<String, Row> = read
            .into_iter()
            .filter_map(|sid| Some((sid.clone(), self.row(&sid)?)))
            .collect();
        let mut state = self.lock();
        state.last_id = state.last_id.max(floor);
        let mut renumbered = Vec::new();
        for sid in low(&state) {
            let row = state.rows.get(&sid).or_else(|| on_disk.get(&sid)).cloned();
            let bytes = state.heads.get(&sid).map(|head| head.bytes);
            if let (Some(mut row), Some(bytes)) = (row, bytes) {
                row.id = state.next_id();
                state.index(row.head(bytes));
                state.rows.insert(sid.clone(), row);
                renumbered.push(sid);
            }
        }
        for head in heads {
            let sid = head.sid.as_str();
            if !state.heads.contains_key(sid)
                && !state.dropping.contains(sid)
                && !state.gone.contains(sid)
            {
                state.index(head);
            }
        }
        state.gone.clear();
        state.loaded = true;
        let after = std::mem::take(&mut state.after_load);
        drop(state);
        self.moved.notify_all();
        for sid in renumbered {
            self.write(&sid);
        }
        for then in after {
            then();
        }
    }

    /// Session `sid`'s row, whole: the one in hand, else its file. A row file the loaded index
    /// does not hold (another process on this state root wrote it since the load) is indexed as
    /// it is read; a head whose file went is dropped ([`Self::forget`]).
    fn row(&self, sid: &str) -> Option<Row> {
        {
            let state = self.lock();
            if state.dropping.contains(sid) {
                return None;
            }
            if let Some(row) = state.rows.get(sid) {
                return Some(row.clone());
            }
        }
        let text = match std::fs::read_to_string(self.path_of(sid)?) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    self.forget(sid);
                }
                return None;
            }
        };
        let row =
            Row::decode(&text, self.tasks.as_deref()).filter(|row| row.sid.as_str() == sid)?;
        self.adopt(&row, text.len());
        Some(row)
    }

    /// Index `row`, read from its file of `len` bytes, where the loaded index does not hold it:
    /// a row another process on this state root wrote after this one loaded.
    fn adopt(&self, row: &Row, len: usize) {
        let sid = row.sid.as_str();
        let held = |state: &State| {
            !state.loaded || state.heads.contains_key(sid) || state.dropping.contains(sid)
        };
        if held(&self.lock()) {
            return;
        }
        let bytes = self.charge(len, row);
        let mut state = self.lock();
        if !held(&state) {
            state.index(row.head(bytes));
        }
    }

    /// Drop the head of `sid` where its file is gone though this process holds no write or
    /// removal of it: another process on this state root removed the row. A write of this
    /// process's holds its row in hand until its rename landed ([`Self::write`]). Stat outside
    /// the lock and recheck the indexed generation before dropping it.
    fn forget(&self, sid: &str) {
        let Some(path) = self.path_of(sid) else {
            return;
        };
        self.forget_if(sid, || !path.exists());
    }

    fn forget_if(&self, sid: &str, missing: impl FnOnce() -> bool) {
        let eligible = |state: &State| {
            state.loaded && !state.rows.contains_key(sid) && !state.dropping.contains(sid)
        };
        let generation = {
            let state = self.lock();
            if !eligible(&state) {
                return;
            }
            state
                .heads
                .get(sid)
                .map(|head| Arc::clone(&head.generation))
        };
        let Some(generation) = generation else {
            return;
        };
        if !missing() {
            return;
        }
        let mut state = self.lock();
        if eligible(&state)
            && state
                .heads
                .get(sid)
                .is_some_and(|head| Arc::ptr_eq(&head.generation, &generation))
        {
            state.unindex(sid);
        }
    }

    /// Bring the index level with the folder where it moved since it was last listed: index each
    /// row file it does not hold, and drop each head whose file went ([`Self::row`],
    /// [`Self::forget`]). Other processes on this state root write and remove rows there.
    fn refresh(&self) {
        self.refresh_at(std::time::SystemTime::now());
    }

    fn refresh_at(&self, now: std::time::SystemTime) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let moved = std::fs::metadata(dir).and_then(|meta| meta.modified()).ok();
        // A folder whose time is recent is listed again even where it reads the same: a volume
        // with a coarse clock gives two changes within one tick the same time.
        let aged = moved
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > MTIME_TICK);
        if aged && self.lock().listed == moved {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let names: HashSet<String> = entries
            .flatten()
            .take(ROWS_MAX.saturating_mul(2))
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(row_sid)
                    .map(str::to_owned)
            })
            .collect();
        let (new, went): (Vec<String>, Vec<String>) = {
            let state = self.lock();
            (
                names
                    .iter()
                    .filter(|sid| !state.heads.contains_key(sid.as_str()))
                    .cloned()
                    .collect(),
                state
                    .heads
                    .keys()
                    .filter(|sid| !names.contains(sid.as_str()))
                    .cloned()
                    .collect(),
            )
        };
        for sid in new {
            let _ = self.row(&sid);
        }
        for sid in went {
            self.forget(&sid);
        }
        self.lock().listed = if aged { moved } else { None };
    }

    /// Put `row`, as `edit` changes it, in hand for the worker to write, keeping its id and its
    /// charge; `false` where `edit` changed nothing, or the index no longer holds the row.
    fn amend(&self, mut row: Row, edit: impl FnOnce(&mut Row) -> bool) -> bool {
        if !edit(&mut row) {
            return false;
        }
        let mut state = self.lock();
        let Some(head) = state.heads.get(row.sid.as_str()) else {
            return false;
        };
        row.id = head.id;
        let bytes = head.bytes;
        state.index(row.head(bytes));
        state.rows.insert(row.sid.as_str().to_owned(), row);
        true
    }

    /// The edit that says a row's conclusion was told, where the row is of conclusion `told` and
    /// does not say so yet.
    fn tells(told: Told) -> impl FnOnce(&mut Row) -> bool {
        move |row: &mut Row| {
            let tells = !row.told_end && row.task.as_ref().is_some_and(|task| told.is_of(task));
            row.told_end |= tells;
            tells
        }
    }

    /// The load ended: each row it read whose conclusion was told before it says so.
    fn tell_loaded(&self) {
        let told: Vec<(String, Told)> = {
            let mut state = self.lock();
            let read: Vec<String> = state
                .told
                .keys()
                .filter(|sid| state.heads.contains_key(sid.as_str()))
                .cloned()
                .collect();
            read.into_iter()
                .filter_map(|sid| state.told.remove(&sid).map(|told| (sid, told)))
                .collect()
        };
        for (sid, told) in told {
            if let Some(row) = self.row(&sid)
                && self.amend(row, Self::tells(told))
            {
                self.write(&sid);
            }
        }
    }

    /// Do `job`, on the worker.
    fn run(&self, job: Job) {
        match job {
            Job::Write(sid) => {
                self.write(&sid);
                self.retain();
            }
            Job::Load => {
                self.merge(self.read_rows());
                self.tell_loaded();
                self.retain();
            }
            Job::Loaded(heads) => {
                self.merge(heads);
                self.tell_loaded();
                self.retain();
            }
            Job::Retain => self.retain(),
            Job::AfterLoad(then) => then(),
            #[cfg(test)]
            Job::Drop(head) => self.drop_files(std::slice::from_ref(&head)),
            Job::Flush(ack) => {
                let landed = self.flush_rows();
                let _ = ack.send(landed);
            }
        }
    }

    /// Write again each row still in hand (a write that failed), then bring the folder's renames
    /// to disk: whether every such row is on disk now. A table in memory holds its rows in hand
    /// for good.
    fn flush_rows(&self) -> bool {
        if self.dir.is_none() {
            return true;
        }
        let held: Vec<String> = self.lock().rows.keys().cloned().collect();
        for sid in &held {
            self.write(sid);
        }
        self.flush_dir();
        let state = self.lock();
        held.iter().all(|sid| !state.rows.contains_key(sid))
    }

    /// Hand `job` to the worker over `jobs`; `false` where it has ended.
    fn enqueue(jobs: &mpsc::Sender<Job>, job: Job) -> bool {
        jobs.send(job).is_ok()
    }

    /// A row's charge against the budget: its file's `len` bytes, and its result and sidecar.
    fn charge(&self, len: usize, row: &Row) -> u64 {
        let size = |path: &Path| std::fs::metadata(path).map_or(0, |meta| meta.len());
        let result = row
            .task
            .as_ref()
            .and_then(|task| task.result_path.as_deref())
            .map_or(0, |path| {
                size(path) + size(&aterm_agent::harness::result::sidecar_of(path))
            });
        u64::try_from(len)
            .unwrap_or(u64::MAX)
            .saturating_add(result)
    }

    /// Write the row in hand for `sid` to its file, by an atomic rename, and charge its head.
    fn write(&self, sid: &str) {
        let Some(row) = self.lock().rows.get(sid).cloned() else {
            return;
        };
        let text = row.encode();
        let written = self.path_of(sid).map(|path| write_atomically(&path, &text));
        let bytes = self.charge(text.len(), &row);
        let mut state = self.lock();
        state.dirty |= matches!(written, Some(Ok(())));
        // A newer write or a removal of the same session since this one took it in hand wins.
        if state.rows.get(sid) == Some(&row) {
            if matches!(written, Some(Ok(()))) {
                state.rows.remove(sid);
            }
            if state.heads.contains_key(sid) {
                state.index(row.head(bytes));
            }
        }
        drop(state);
        if let Some(Err(error)) = written {
            aterm_log::warn!(
                "session table: the row of {sid} not written: {error}; it is kept in memory \
                 and written again at the next flush"
            );
        }
    }

    /// Remove the result files `heads` name, in one pass over the tasks directory, then their
    /// row files.
    fn drop_files(&self, heads: &[RowHead]) {
        if heads.is_empty() {
            return;
        }
        let store = self.store();
        let named: Vec<PathBuf> = heads
            .iter()
            .filter_map(|head| head.result_path.clone())
            .collect();
        crate::task_launch::retire_row_results(store.as_ref(), &named);
        for head in heads {
            if let Some(path) = self.path_of(head.sid.as_str())
                && let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                aterm_log::warn!(
                    "session table: the row {} not removed: {error}",
                    path.display()
                );
            }
        }
        let mut state = self.lock();
        state.dirty = true;
        for head in heads {
            state.dropping.remove(head.sid.as_str());
            if !state.loaded {
                state.gone.insert(head.sid.as_str().to_owned());
            }
        }
    }

    /// The budget: past its bytes or its rows ([`TABLE_BUDGET_BYTES`], [`ROWS_MAX`]), the oldest
    /// ended rows go, their result files first, but those live sessions protect ([`Live`]). Not
    /// before the load ended, nor before the launch's sessions are registered: a table it has not
    /// read whole is not judged, and until the restore and a seamless update's adoption have
    /// placed every session, a row they protect would read as unprotected.
    fn retain(&self) {
        let budget = self.budget;
        {
            let state = self.lock();
            if !state.loaded
                || !state.settled
                || (state.bytes <= budget.bytes && state.heads.len() <= budget.rows)
            {
                return;
            }
        }
        let live = self
            .store()
            .map(|store| Live::of(&store))
            .unwrap_or_default();
        let victims: Vec<RowHead> = {
            let mut state = self.lock();
            let mut over_bytes = state.bytes.saturating_sub(budget.bytes);
            let mut over_rows = state.heads.len().saturating_sub(budget.rows);
            let mut chosen = Vec::new();
            let mut held = 0usize;
            for (_, sid) in &state.order {
                if over_bytes == 0 && over_rows == 0 {
                    break;
                }
                let Some(head) = state.heads.get(sid) else {
                    continue;
                };
                if live.protects(head) {
                    held += 1;
                    continue;
                }
                over_bytes = over_bytes.saturating_sub(head.bytes);
                over_rows = over_rows.saturating_sub(1);
                chosen.push(sid.clone());
            }
            if held > 0 && (over_bytes > 0 || over_rows > 0) {
                aterm_log::info!(
                    "session table: over its budget with {held} rows live sessions still need; \
                     they stay"
                );
            }
            chosen
                .iter()
                .filter_map(|sid| {
                    let head = state.unindex(sid)?;
                    state.rows.remove(sid);
                    state.dropping.insert(sid.clone());
                    Some(head)
                })
                .collect()
        };
        self.drop_files(&victims);
    }

    /// Bring the folder's renames and removals to disk, where any came since the last flush.
    fn flush_dir(&self) {
        let Some(dir) = &self.dir else {
            return;
        };
        if !std::mem::take(&mut self.lock().dirty) {
            return;
        }
        if dir.is_dir()
            && let Err(error) = aterm_durable::ordered_dir(dir)
        {
            self.lock().dirty = true;
            aterm_log::warn!("session table: {} not flushed: {error}", dir.display());
        }
    }
}

/// Write `text` to `path` by a rename from a file of this process's beside it, owner-only.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(std::io::ErrorKind::InvalidInput)?;
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        aterm_durable::ordered(&file)?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// THE MACHINE'S SESSION TABLE (the module's head).
pub(crate) struct SessionTable {
    shared: Arc<Shared>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for SessionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTable")
            .field("dir", &self.shared.dir)
            .finish_non_exhaustive()
    }
}

/// A table that keeps its rows in memory: a launch with no state directory, and a store no
/// launch gave a folder. It holds no row of an earlier run, so none waits for the launch's
/// sessions to protect it.
impl Default for SessionTable {
    fn default() -> Self {
        let mut state = State::default();
        state.loaded = true;
        state.settled = true;
        Self {
            shared: Arc::new(Shared {
                dir: None,
                tasks: crate::task_record::tasks_dir(),
                inst: instance(),
                budget: Budget::MACHINE,
                state: Mutex::new(state),
                moved: Condvar::new(),
                store: Mutex::new(Weak::new()),
            }),
            jobs: Mutex::new(None),
            worker: Mutex::new(None),
        }
    }
}

impl Drop for SessionTable {
    /// The worker writes what it holds and ends before the table goes.
    fn drop(&mut self) {
        drop(
            self.jobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

impl SessionTable {
    /// The table's folder, `<state>/sessions/`; `None` with no state directory.
    pub(crate) fn dir() -> Option<PathBuf> {
        aterm_types::dirs::state_dir().map(|state| state.join(DIR_NAME))
    }

    /// The table over folder `dir`: a worker thread of its own writes its rows and runs its
    /// budget, and another reads the rows earlier runs left there.
    pub(crate) fn at(dir: PathBuf) -> Arc<Self> {
        Self::open(dir, Budget::MACHINE, None)
    }

    /// [`Self::at`] under `budget`, its load held until `gate` gives way (its sender sends or
    /// goes): a test's table.
    #[cfg(test)]
    pub(crate) fn at_with(
        dir: PathBuf,
        budget: Budget,
        gate: Option<mpsc::Receiver<()>>,
    ) -> Arc<Self> {
        Self::open(dir, budget, gate)
    }

    fn open(dir: PathBuf, budget: Budget, gate: Option<mpsc::Receiver<()>>) -> Arc<Self> {
        let shared = Arc::new(Shared {
            dir: Some(dir),
            tasks: crate::task_record::tasks_dir(),
            inst: instance(),
            budget,
            state: Mutex::new(State::default()),
            moved: Condvar::new(),
            store: Mutex::new(Weak::new()),
        });
        let (tx, rx) = mpsc::channel::<Job>();
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("session-table".to_owned())
                .spawn(move || {
                    for job in rx {
                        shared.run(job);
                    }
                })
        };
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                aterm_log::warn!("session table: no worker started ({error}); rows stay in memory");
                shared.lock().loaded = true;
                return Arc::new(Self {
                    jobs: Mutex::new(None),
                    worker: Mutex::new(None),
                    shared,
                });
            }
        };
        let loader = {
            let (shared, tx) = (Arc::clone(&shared), tx.clone());
            std::thread::Builder::new()
                .name("session-table-load".to_owned())
                .spawn(move || {
                    if let Some(gate) = gate {
                        let _ = gate.recv();
                    }
                    Shared::enqueue(&tx, Job::Loaded(shared.read_rows()));
                })
        };
        if let Err(error) = loader {
            aterm_log::warn!(
                "session table: no thread started to read it ({error}); its worker does"
            );
            Shared::enqueue(&tx, Job::Load);
        }
        Arc::new(Self {
            jobs: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            shared,
        })
    }

    /// Serve `store`: hold this table as its session table, whose live sessions protect rows and
    /// whose records keep result files.
    pub(crate) fn serve(self: &Arc<Self>, store: &Store) {
        *self
            .shared
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(store);
        store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .set_table(Arc::clone(self));
    }

    /// Hand `job` to the worker; `false` where there is none (a table in memory).
    fn send(&self, job: Job) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.as_ref().is_some_and(|jobs| Shared::enqueue(jobs, job))
    }

    /// Keep `row` as its session's row: a new end takes the next id, its head is indexed at once,
    /// and the worker writes it and runs the budget. A conclusion told before the row came is
    /// told on it, where it is the row's.
    pub(crate) fn write(&self, mut row: Row) {
        let sid = row.sid.as_str().to_owned();
        {
            let mut state = self.shared.lock();
            row.id = state.next_id();
            row.inst = Some(self.shared.inst.clone());
            if let Some(told) = state.told.remove(&sid) {
                Shared::tells(told)(&mut row);
            }
            state.dropping.remove(&sid);
            state.index(row.head(0));
            state.rows.insert(sid.clone(), row);
        }
        if !self.send(Job::Write(sid)) {
            self.shared.retain();
        }
    }

    /// Session `sid`'s creator was told its conclusion `told`: its row's protection lapses
    /// (`Live`). A row the index does not hold yet (its close not handed over, or the load not
    /// ended) says so once it comes, where it is of that conclusion.
    pub(crate) fn told_end(&self, sid: &SessionId, told: Told) {
        {
            let mut state = self.shared.lock();
            match state.heads.get(sid.as_str()) {
                Some(head) if head.told_end => return,
                Some(_) => {}
                None => {
                    state.told.insert(sid.as_str().to_owned(), told);
                    return;
                }
            }
        }
        if let Some(row) = self.shared.row(sid.as_str())
            && self.shared.amend(row, Shared::tells(told))
        {
            self.send(Job::Write(sid.as_str().to_owned()));
        }
    }

    /// The launch's sessions are registered: the restore and a seamless update's adoption have
    /// placed every one (`App::finish_restore_placement`). The budget judges the table from now
    /// on; until then it holds every row, since a row is protected by live sessions ([`Live`])
    /// that may not be in the store yet. Returns whether this call settled it.
    pub(crate) fn sessions_settled(&self) -> bool {
        {
            let mut state = self.shared.lock();
            if state.settled {
                return false;
            }
            state.settled = true;
        }
        if !self.send(Job::Retain) {
            self.shared.retain();
        }
        true
    }

    /// Whether the launch's sessions are registered ([`Self::sessions_settled`]): until then a
    /// judge of what lives (this budget, the accounts' session reconcile) may not see them all.
    pub(crate) fn settled(&self) -> bool {
        self.shared.lock().settled
    }

    /// Run `then` on the disk table's worker once its load has ended (inline for a memory table).
    /// A one-shot sweep of the tasks directory goes through here, so it
    /// does not run while every result is held ([`ResultKeeps::holds`]) and keep for good what no
    /// row names.
    pub(crate) fn when_loaded(&self, then: impl FnOnce() + Send + 'static) {
        {
            let mut state = self.shared.lock();
            if !state.loaded {
                state.after_load.push(Box::new(then));
                return;
            }
        }
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(jobs) = jobs.as_mut() {
            Shared::enqueue(jobs, Job::AfterLoad(Box::new(then)));
        } else {
            // With no state directory there is no disk-table worker. Tests use this memory
            // table with a tasks directory; a real launch with no state keeps no task files.
            drop(jobs);
            then();
        }
    }

    /// Session `sid`'s row, whole: the one in hand, else its file.
    pub(crate) fn row(&self, sid: &SessionId) -> Option<Row> {
        self.shared.row(sid.as_str())
    }

    /// Session `sid`'s head: the index's while its row is in hand or its file stands, else read
    /// from its file, which another process on this state root may have written since the load.
    pub(crate) fn head(&self, sid: &SessionId) -> Option<RowHead> {
        let indexed = {
            let state = self.shared.lock();
            let head = state.heads.get(sid.as_str()).cloned();
            if head.is_some()
                && (state.rows.contains_key(sid.as_str()) || self.shared.dir.is_none())
            {
                return head;
            }
            head
        };
        if indexed.is_some() {
            if self
                .shared
                .path_of(sid.as_str())
                .is_some_and(|path| path.is_file())
            {
                return indexed;
            }
            self.shared.forget(sid.as_str());
            return self.shared.lock().heads.get(sid.as_str()).cloned();
        }
        let row = self.row(sid)?;
        let indexed = self.shared.lock().heads.get(sid.as_str()).cloned();
        Some(indexed.unwrap_or_else(|| row.head(0)))
    }

    /// The row this process wrote of the session it routed by local id `local`.
    pub(crate) fn by_local(&self, local: u64) -> Option<RowHead> {
        let inst = self.shared.inst.as_str();
        let state = self.shared.lock();
        state
            .heads
            .values()
            .filter(|head| head.local == Some(local) && head.inst.as_deref() == Some(inst))
            .max_by_key(|head| head.id)
            .cloned()
    }

    /// The local id `head`'s session had, where this process wrote the row: `@<local>` names it
    /// here alone.
    pub(crate) fn local_here(&self, head: &RowHead) -> Option<u64> {
        head.local
            .filter(|_| head.inst.as_deref() == Some(self.shared.inst.as_str()))
    }

    /// Whether session `sid` has a row.
    // Read only by `control::hosts_or_keeps` (unix), the fleet drive gate in
    // `control::drive_admission` (`any(unix, test)`) and tests.
    #[cfg(any(unix, test))]
    pub(crate) fn holds(&self, sid: &SessionId) -> bool {
        self.head(sid).is_some()
    }

    /// The tab IDs of the ended sessions whose rows this table indexes that begin `prefix`, for
    /// a typed prefix of an ended tab's ID ([`crate::control`]'s request reading).
    pub(crate) fn sids_beginning(&self, prefix: &str) -> Vec<String> {
        self.shared
            .lock()
            .heads
            .keys()
            .filter(|sid| aterm_types::tab_id::is_tab_id(sid) && sid.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Whether the launch's load ended within `wait`.
    pub(crate) fn loaded_within(&self, wait: Duration) -> bool {
        let state = self.shared.lock();
        let (state, _) = self
            .shared
            .moved
            .wait_timeout_while(state, wait, |state| !state.loaded)
            .unwrap_or_else(PoisonError::into_inner);
        state.loaded
    }

    /// Every head, oldest first, `f` of them, once the load ended within [`LOAD_WAIT`], the index
    /// brought level with the folder ([`Shared::refresh`]).
    fn heads_by(&self, f: impl FnOnce(&State) -> Vec<RowHead>) -> Result<Vec<RowHead>, Loading> {
        if !self.loaded_within(LOAD_WAIT) {
            return Err(Loading);
        }
        self.shared.refresh();
        Ok(f(&self.shared.lock()))
    }

    /// The heads of the rows past id `since`, oldest first (`exits`).
    pub(crate) fn exits_since(&self, since: u64) -> Result<Vec<RowHead>, Loading> {
        self.heads_by(|state| {
            state
                .order
                .range((since.saturating_add(1), String::new())..)
                .filter_map(|(_, sid)| state.heads.get(sid).cloned())
                .collect()
        })
    }

    /// Every head, newest first (History, ST-33).
    #[cfg(test)]
    pub(crate) fn heads_newest_first(&self) -> Result<Vec<RowHead>, Loading> {
        self.heads_by(|state| {
            state
                .order
                .iter()
                .rev()
                .filter_map(|(_, sid)| state.heads.get(sid).cloned())
                .collect()
        })
    }

    /// A person removes session `sid`'s row (History's Remove, ST-33): its result files go first,
    /// then the row. `false` where it has none.
    #[cfg(test)]
    pub(crate) fn remove(&self, sid: &SessionId) -> bool {
        let head = {
            let mut state = self.shared.lock();
            let Some(head) = state.unindex(sid.as_str()) else {
                return false;
            };
            state.rows.remove(sid.as_str());
            state.dropping.insert(sid.as_str().to_owned());
            head
        };
        if !self.send(Job::Drop(head.clone())) {
            self.shared.drop_files(std::slice::from_ref(&head));
        }
        true
    }

    /// What the rows keep in the tasks directory `dir`, for one sweep of it to ask as it goes.
    pub(crate) fn result_keeps(&self, dir: &Path) -> ResultKeeps {
        ResultKeeps {
            shared: Arc::clone(&self.shared),
            dir: dir.to_path_buf(),
        }
    }

    /// Wait at most [`FLUSH_WAIT`] for every write and removal handed over, write again each row
    /// whose write failed, then bring the folder's renames to disk: the quit's last act on the
    /// table, and an update's before its process exits. Whether every row landed.
    pub(crate) fn flush(&self) -> bool {
        self.flush_started()
            .recv_timeout(FLUSH_WAIT)
            .unwrap_or(false)
    }

    /// Queue the durability barrier without waiting on the window's thread.
    pub(crate) fn flush_started(&self) -> mpsc::Receiver<bool> {
        let (ack, acked) = mpsc::channel();
        if !self.send(Job::Flush(ack.clone())) {
            let _ = ack.send(true);
        }
        acked
    }
}

/// Close session `local` of `store` as the App's close does (`App::retire_session_registration_keeping`):
/// read its terminal, let the registry go of it, and leave its row in the store's table.
#[cfg(test)]
pub(crate) fn close_leaving_row(
    store: &Store,
    local: u64,
    reason: ExitReason,
    actor: crate::session_store::ExitActor,
) -> Option<SessionId> {
    let at = {
        let guard = store.read().unwrap_or_else(PoisonError::into_inner);
        let handle = guard.by_local(local)?;
        AtClose::read(&handle.term, &handle.ctx)
    };
    close_leaving_row_with(store, local, reason, actor, at)
}

/// [`close_leaving_row`] with what the close read of the terminal given.
#[cfg(test)]
pub(crate) fn close_leaving_row_with(
    store: &Store,
    local: u64,
    reason: ExitReason,
    actor: crate::session_store::ExitActor,
    at: AtClose,
) -> Option<SessionId> {
    let closed = store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .deregister_closing(local, reason, actor)?;
    let ended = closed.ended.as_ref()?;
    let row = row_at_close(&closed.sid, ended, &at);
    let table = store.read().unwrap_or_else(PoisonError::into_inner).table();
    table.write(row);
    Some(closed.sid)
}

#[cfg(test)]
#[path = "session_table_tests.rs"]
mod tests;

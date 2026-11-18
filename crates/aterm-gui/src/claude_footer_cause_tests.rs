// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE FOOTER'S READS COST, COUNTED: a [`CauseRig`] drives the resolver as its thread does
//! ([`Resolver::retain`], then [`Resolver::next`], then [`Resolver::turn`] at the instant `next`
//! named) on a scripted clock, the monotonic one and the wall one stepping together
//! ([`Asks::wall`]), over a scratch Claude Code session on its stored login, and books what each
//! turn did ([`FooterWork`]). The rig's asks are the live ones' shapes over the scratch tree: the
//! settings default (on its own command, [`Command::Default`]) and the model catalog are read
//! through their real paces
//! ([`crate::permission_click::note_default_of`], [`crate::claude_facts::note_catalog_under`]),
//! the chain itself stood in for, the catalog read under the scratch harness state, and the
//! roster is the file beside the scratch tree's root (absent: an unconfigured one), held by its
//! stamp ([`RosterAsks::Files`]). The host facts a read takes are scripted on the rig's thread
//! ([`script`]): the usage tap's switch, a step of the wall clock and the lab's journal, each
//! stand-in noting the host accessor it stands for ([`crate::host_facts::noted`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_agent::harness::accounts;
use aterm_agent::harness::alab_seen;
use aterm_agent::harness::looks::Input;
use aterm_agent::harness::profile::WeeklyGap;
use aterm_agent::harness::tap;
use aterm_agent::supervise::policy::claude_settings::{PermissionDefault, Resolved};

use super::{Asks, Command, Job, LogWake, RECHECK, Resolver, RosterAsks, TapAsks, TapSeat};
use crate::claude_inputs::{InputWatch, Sink};
use crate::host_facts::{Bell, noted, take_noted, unbelled};
use crate::session_timeline::SessionTimeline;
use crate::work_counts::{FooterWork, NO_GATHERS, gathers_of, take_footer_work};

/// The session the rig's tab is.
const SESSION: u64 = 11;

/// The session of a second tab on the same agent ([`unchanged_inputs_are_decoded_once`]).
const SECOND: u64 = 12;

/// The Claude Code session the registry row names.
const SESSION_ID: &str = "00000000-0000-4000-8000-000000000c01";

/// The tab the agent's exec environment names.
const TAB: &str = "t0000000c01";

/// The registry row's `startedAt`, in seconds.
const STARTED_AT: u64 = 1_790_607_848;

/// The account the login profile names.
const EMAIL: &str = "cause@example.test";

/// One rig's scratch tree and the start of its clock, as its asks read them on its thread.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    /// The tree's root, canonical: the agent's `HOME`.
    root: PathBuf,
    /// Where the scripted clock starts, on the monotonic clock and on the wall one.
    t0: Instant,
    wall0: SystemTime,
    /// Whether the window's usage tap switch is on ([`seat`]).
    tap_on: bool,
    /// How far the wall clock was stepped past the monotonic one ([`wall`]).
    step: Duration,
    /// The lab's journal as the process holds it ([`OUTLOOK_READS`]).
    journal: alab_seen::Journal,
}

thread_local! {
    /// The rig on this thread: the resolver reads on the test's own thread.
    static SCRATCH: RefCell<Option<Scratch>> = const { RefCell::new(None) };
}

fn with_scratch<T>(f: impl FnOnce(&Scratch) -> T) -> T {
    SCRATCH.with(|scratch| f(scratch.borrow().as_ref().expect("a rig on this thread")))
}

/// Script the host facts the rig's reads take from now on.
fn script(f: impl FnOnce(&mut Scratch)) {
    SCRATCH.with(|scratch| f(scratch.borrow_mut().as_mut().expect("a rig on this thread")));
}

/// The agent's `HOME`.
fn root() -> PathBuf {
    with_scratch(|scratch| scratch.root.clone())
}

/// The agent's Claude dir, its default home's records.
fn records() -> PathBuf {
    root().join(".claude")
}

/// The harness state the accounts store and the catalogs are under.
fn state() -> PathBuf {
    root().join("state")
}

/// The directory the usage tap writes its readings to.
fn rl() -> PathBuf {
    root().join("rl")
}

/// The pid the rig's agent is: this test process, whose kernel birth the resolver reads and
/// which lives for as long as the rig does.
fn pid() -> i32 {
    i32::try_from(std::process::id()).expect("a pid")
}

/// The exec image of a Claude Code started by hand in [`TAB`] on the default home's stored
/// login: no tap on its line.
fn exec_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into()],
        env: vec![
            format!("{}={}", accounts::HOMEDIR_ENV, root().display()),
            format!("{}={TAB}", aterm_types::domain::ENV_PARENT_SESSION_ID),
        ],
        ..Default::default()
    })
}

/// A window whose relaunch lines carry the tap while its switch is on ([`Scratch::tap_on`]), its
/// readings under [`rl`], this machine's accounts store under [`state`]: the stand-in of the
/// live seat, which reads the switch and the harness state.
fn seat() -> TapSeat {
    noted("usage_tap_on");
    noted("harness_state");
    TapSeat {
        on: with_scratch(|scratch| scratch.tap_on),
        named: true,
        dir: Some(rl()),
        store: Some(state()),
    }
}

/// The settings default's read, at its pace, of a chain that names no default.
fn default_read(pid: u32, now: Instant) {
    crate::permission_click::note_default_of(TAB, pid, now, || PermissionDefault {
        mode: Resolved::Unread("the rig's chain names none".into()),
        bypass_disabled: None,
    });
}

/// The catalog's read, at its pace, under the scratch harness state.
fn catalog_read(version: Option<&str>) {
    crate::claude_facts::note_catalog_under(version, || Some(state()));
}

/// The roster file beside the scratch tree's root, and the harness state its bindings are in.
fn roster_files() -> Option<(PathBuf, Option<PathBuf>)> {
    Some((root().join(accounts::ACCOUNTS_FILE), Some(state())))
}

/// The wall clock the scripted instant `at` is: as far past the rig's wall start as `at` is past
/// its monotonic one, and as far again as the wall was stepped ([`Scratch::step`]).
fn wall(at: Instant) -> SystemTime {
    with_scratch(|scratch| scratch.wall0 + scratch.step + at.saturating_duration_since(scratch.t0))
}

/// The exec image of the same Claude Code as aterm relaunched it with its usage tap on its line
/// ([`each_input_is_read_when_its_file_moves`]).
fn tapped_exec_args(pid: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    let mut args = exec_args(pid)?;
    args.argv.extend([
        "--settings".into(),
        tap::settings_value(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"))
            .expect("an admissible path"),
    ]);
    Some(args)
}

/// What the rig's reads ask.
const ASKS: Asks = Asks {
    files: |_| None,
    args: exec_args,
    roster: RosterAsks::Files(roster_files),
    home: || Some(root()),
    route: |_, _, _| accounts::AuthRoute::Stored,
    tap: TapAsks {
        seat,
        settings: |_, _| true,
        restart: |_, _| {
            noted("offered_for");
            true
        },
        room: |_, _| {
            noted("tap_room_for");
            true
        },
    },
    default: default_read,
    catalog: catalog_read,
    wall,
};

/// [`ASKS`] of the agent aterm relaunched with its usage tap.
const TAPPED_ASKS: Asks = Asks {
    args: tapped_exec_args,
    ..ASKS
};

/// A resolver over one scratch Claude Code session, on a scripted clock ([`Self::at`]).
pub(super) struct CauseRig {
    resolver: Resolver,
    timeline: Arc<Mutex<SessionTimeline>>,
    /// What the input watches tell, as the resolver's thread would hear it.
    told: Receiver<Command>,
}

/// One turn's work, at its instant in seconds after the rig's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Turn {
    pub(super) at: u64,
    pub(super) work: FooterWork,
}

impl CauseRig {
    /// A session of this process in [`TAB`] on its default home's stored login: its registry row,
    /// its login profile (an address and a plan, no usage figure), and an accounts store whose
    /// own account holds no figure, so no weekly figure of its own is current; no transcript yet,
    /// no tap reading. The watcher is a real one.
    pub(super) fn new() -> CauseRig {
        CauseRig::with(ASKS, lay)
    }

    /// The rig over the session `lay` lays, read with `asks`.
    fn with(asks: Asks, lay: fn()) -> CauseRig {
        CauseRig::watched_by(asks, lay, atpkg::dirwatch::Watcher::new)
    }

    /// [`Self::with`], its input watches opened by `open` (a refused one: no watch, so every
    /// session stays on the clocks arm).
    fn watched_by(
        asks: Asks,
        lay: fn(),
        open: fn() -> std::io::Result<atpkg::dirwatch::Watcher>,
    ) -> CauseRig {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-gui-footer-cause-")
            .tempdir()
            .expect("exclusive rig scratch");
        let root = std::fs::canonicalize(dir.path()).expect("the rig's scratch");
        let t0 = Instant::now();
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock past the epoch")
            .as_secs();
        let wall0 = UNIX_EPOCH + Duration::from_secs(secs);
        SCRATCH.with(|scratch| {
            *scratch.borrow_mut() = Some(Scratch {
                _dir: dir,
                root,
                t0,
                wall0,
                tap_on: true,
                step: Duration::ZERO,
                journal: alab_seen::Journal::Off,
            });
        });
        lay();
        let (tx, told) = channel();
        let sink: Sink = Arc::new(move |told| tx.send(Command::from(told)).is_ok());
        let resolver = Resolver::new(InputWatch::new(open, sink), asks);
        let _ = take_footer_work();
        CauseRig {
            resolver,
            timeline: tab(),
            told,
        }
    }

    /// The scripted instant `after` past the rig's start.
    pub(super) fn at(after: Duration) -> Instant {
        with_scratch(|scratch| scratch.t0 + after)
    }

    /// The tab's ask, as the status sweep makes it, taken by a turn at `after`: what it did.
    pub(super) fn ask(&mut self, after: Duration) -> FooterWork {
        let timeline = Arc::clone(&self.timeline);
        self.ask_in(SESSION, timeline, after)
    }

    /// [`Self::ask`] of session `session`, whose tab's timeline is `timeline`.
    fn ask_in(
        &mut self,
        session: u64,
        timeline: Arc<Mutex<SessionTimeline>>,
        after: Duration,
    ) -> FooterWork {
        let job = Job {
            session,
            timeline,
            pgid: pid(),
            wake: LogWake::default(),
        };
        let now = Self::at(after);
        self.resolver.turn([Command::Request(job)], || now);
        take_footer_work()
    }

    /// What the resolver's thread hears from its watches (`command`), taken by a turn at `after`:
    /// what it did.
    fn tell(&mut self, command: Command, after: Duration) -> FooterWork {
        self.tell_all(vec![command], after)
    }

    /// [`Self::tell`] of `commands`, in one turn.
    fn tell_all(&mut self, commands: Vec<Command>, after: Duration) -> FooterWork {
        let now = Self::at(after);
        self.resolver.turn(commands, || now);
        take_footer_work()
    }

    /// The looked paths the input watches told of since the last call, once the forwarder has
    /// finished every wait the kernel's queued events wake (the events of each write made before
    /// this call among them), as the commands the resolver's thread would take.
    fn heard(&mut self) -> Vec<Command> {
        self.resolver.inputs.wake_and_settle();
        self.told.try_iter().collect()
    }

    /// The rig's agent as the resolver holds it, by pid and kernel start time: what its process
    /// watch is keyed by and tells.
    fn process(&self) -> (u32, u64) {
        let identity = &self.resolver.known[&SESSION];
        (
            u32::try_from(identity.pid).expect("a pid"),
            identity.started.expect("a known kernel start"),
        )
    }

    /// The resolver's own turns from `from` to `until` past the rig's start with no command, as
    /// its thread takes them: each turn that did any work, in order.
    pub(super) fn idle(&mut self, from: Duration, until: Duration) -> Vec<Turn> {
        let (mut now, end) = (Self::at(from), Self::at(until));
        let mut turns = Vec::new();
        loop {
            self.resolver.retain(now);
            let Some(at) = self.resolver.next(now).filter(|at| *at <= end) else {
                break;
            };
            assert!(at > now, "the clock a turn left owed is still to come");
            now = at;
            self.resolver.turn(std::iter::empty(), || now);
            let work = take_footer_work();
            if work != FooterWork::default() {
                turns.push(Turn {
                    at: with_scratch(|scratch| now.saturating_duration_since(scratch.t0)).as_secs(),
                    work,
                });
            }
        }
        turns
    }
}

/// A tab whose foreground is the rig's agent, a Claude Code.
fn tab() -> Arc<Mutex<SessionTimeline>> {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(pid());
        held.set_program(pid(), Some("claude".into()));
    }
    timeline
}

/// The account a footer read of `timeline` names.
fn shown_email(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<String> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.account_email.clone())
}

/// The rig's login profile file.
fn profile_file() -> PathBuf {
    aterm_agent::harness::profile::path_in(&root(), None, &records())
}

impl Drop for CauseRig {
    fn drop(&mut self) {
        SCRATCH.with(|scratch| scratch.borrow_mut().take());
    }
}

/// Lay the rig's session ([`CauseRig::new`]).
fn lay() {
    let work = root().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(records().join("sessions")).unwrap();
    std::fs::create_dir_all(state()).unwrap();
    let pid = pid();
    std::fs::write(
        records().join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
            aterm_json::to_string(&work.display().to_string()).unwrap(),
            STARTED_AT * 1000 + 684
        ),
    )
    .unwrap();
    let profile = aterm_json::json!({
        "oauthAccount": {
            "accountUuid": "cause-account",
            "emailAddress": EMAIL,
            "planDisplayName": "Max 20x"
        }
    });
    std::fs::write(profile_file(), aterm_json::to_string(&profile).unwrap()).unwrap();
    let mut store = accounts::State::default();
    store.accounts.insert(
        aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL.into(),
        accounts::Account::default(),
    );
    accounts::Store::at(&state()).save(&store).unwrap();
}

/// A line of a transcript: a user's turn.
const TURN: &str = r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-11-15T00:00:00.000Z"}"#;

/// The agent's working directory.
fn work() -> PathBuf {
    root().join("work")
}

/// The session's transcript, in the project of its working directory.
fn transcript() -> PathBuf {
    records()
        .join("projects")
        .join(aterm_agent::harness::footer::project_slug(&work()))
        .join(format!("{SESSION_ID}.jsonl"))
}

/// A subagent's transcript beside it.
fn subagent() -> PathBuf {
    transcript()
        .with_extension("")
        .join("subagents")
        .join("agent-a0000000000000001.jsonl")
}

/// The usage tap's reading for the rig's tab and agent.
fn reading_file() -> PathBuf {
    rl().join(tap::file_name(TAB, std::process::id()).expect("a tab and a pid"))
}

/// Write the tap's reading for the rig's agent, reported at wall second `at`, by a rename into
/// place as the tap writes it.
fn write_reading(at: i64) {
    let reading = tap::Reading {
        at,
        sid: TAB.into(),
        pid: std::process::id(),
        session: None,
        login: None,
        home: None,
        api_ms: 1_200,
        five_hour: None,
        seven_day: None,
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        effort: Some("high".into()),
        ctx_left: None,
        dur_us: 0,
        row: None,
        route: None,
    };
    let staged = rl().join("reading.staged");
    std::fs::write(&staged, reading.to_json()).unwrap();
    std::fs::rename(&staged, reading_file()).unwrap();
}

/// [`lay`], with every input a read of the tapped agent looks at there: a transcript with a
/// subagent's beside it, its working directory a repository on `main`, the tap's reading for
/// its tab, and the catalog folder.
fn lay_inputs() {
    lay();
    std::fs::create_dir_all(subagent().parent().unwrap()).unwrap();
    std::fs::write(transcript(), format!("{TURN}\n")).unwrap();
    std::fs::write(subagent(), format!("{TURN}\n")).unwrap();
    std::fs::create_dir_all(work().join(".git")).unwrap();
    std::fs::write(work().join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(state().join("upgrade").join("builds")).unwrap();
    std::fs::create_dir_all(rl()).unwrap();
    write_reading(wall_now());
}

/// The wall clock's second now.
fn wall_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock past the epoch")
            .as_secs(),
    )
    .expect("a second")
}

/// Append one line to the file at `path`.
fn append(path: &Path) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(format!("{TURN}\n").as_bytes())
        .unwrap();
}

/// The work one read of the rig's session does once its image is known and its inputs are held:
/// a derive, one read of the process's kernel start (by which its identity is known, its process
/// watch standing) and one ask of the token view's lane, which reads it on its own thread. The
/// image is held, and the login profile, the accounts store, the roster and the catalog stand, so
/// it reads, decodes and loads none of them; the settings chain is read only on the window's ask
/// ([`a_default_ask_reads_the_settings_chain_alone`]).
const LATER_READ: FooterWork = FooterWork {
    derives: 1,
    profile_decodes: 0,
    exec_arg_reads: 0,
    birth_reads: 1,
    store_loads: 0,
    roster_loads: 0,
    settings_chain_reads: 0,
    token_view_reads: 0,
    token_wants: 1,
    outlook_replays: 0,
    catalog_reads: 0,
    gathers: NO_GATHERS,
};

/// What the ask's read of the rig's session gathers ([`an_untouched_hour_reads_no_file`]): its
/// registry row, its login profile, the roster, the accounts store and the catalog, each once;
/// no transcript, git `HEAD` or tap reading is there to gather.
const FIRST_GATHERS: [Input; 5] = [
    Input::Row,
    Input::Profile,
    Input::Roster,
    Input::Store,
    Input::Catalog,
];

/// [`lay`], its accounts store holding the tap's weekly figure of its own account, read 100 s
/// before the rig's wall start: exact until it is 901 s old (+801), a bound until it is 3601 s
/// old (+3501), gone after, its week resetting thirty days on.
fn lay_weekly() {
    lay();
    let wall0 = with_scratch(|scratch| {
        scratch
            .wall0
            .duration_since(UNIX_EPOCH)
            .expect("a clock past the epoch")
            .as_secs()
    });
    let wall0 = i64::try_from(wall0).expect("a second");
    let mut account = accounts::Account::default();
    account.windows.insert(
        "seven_day".into(),
        accounts::WindowFig {
            pct_x10: 370,
            reset: wall0 + 30 * 86_400,
            at: wall0 - 100,
            by: "m22".into(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        },
    );
    let mut store = accounts::State::default();
    store.accounts.insert(
        aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL.into(),
        account,
    );
    accounts::Store::at(&state()).save(&store).unwrap();
}

/// The weekly words a footer read of `timeline` shows.
fn shown_weekly(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<String> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.subscription.as_ref()?.weekly.clone())
}

/// A watcher this Mac refuses: no input watch and no process watch is placed.
fn refused() -> std::io::Result<atpkg::dirwatch::Watcher> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// AN UNTOUCHED HOUR READS NO FILE (law L1 QuietIsFree; the exit test of the footer's cause
/// design's step C7). One ask read of a session on its stored login, its accounts store holding
/// its own account's weekly figure read 100 s before the ask, decodes the login profile once,
/// reads the agent's exec image once, reads the process's kernel start twice (the read's own, and
/// once more after its process watch is placed, `InputWatch::watch_process`), loads the accounts
/// store, loads the roster and reads the catalog once, asks the token view's lane and reads no
/// settings chain. Every file it
/// looked at is watched and so is its process, so the settle puts it on the watched arm
/// ([`super::Arm::Watched`]). With no command for the hour after it, the resolver takes a turn
/// only at the two times that read posted (`Cause::Deadline`): +801, where the figure turns a
/// bound, and +3501, where it goes; each is a later read's work ([`LATER_READ`]), and so a derive,
/// a kernel start read and an ask of the lane, which gather, decode, load and read no file and no
/// token view. Nothing in the rig's window holds a roster, so the accounts outlook owes no
/// replay and runs none (`outlook_replays` = 0, its deadlines in the hour). The same session with
/// the watcher refused stays on the clocks arm: it is read at the two follow-ups, every 15 s from
/// 21 s to 591 s, every 60 s from 651 s, at the deadline +801 (which moves the 60 s clock to
/// 861 s on) and at +3501 with the clock: 92 reads.
///
/// The counts are of the read sites a file read goes through (`FooterWork`): a stat of a looked
/// path is no read and is not counted, and no count of file opens exists to hold the hour to.
///
/// NEGATIVE CONTROL (run 2026-11-15): with the idle clock (`IDLE_RECHECK`, `LONG_IDLE_RECHECK`)
/// put back in `WatchSchedule::next` and `WatchSchedule::due_idle` for every arm, the test fails
/// at "a watched session's hour" with `left: [15, 30, 45, …, 585, 645, 705, 765, 801, 861, …]
/// right: [801, 3501]`: a read every 15 s, then every 60 s.
#[test]
fn an_untouched_hour_reads_no_file() {
    let hour = Duration::from_secs(3_600);
    let mut rig = CauseRig::with(ASKS, lay_weekly);
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask,
        FooterWork {
            profile_decodes: 1,
            exec_arg_reads: 1,
            birth_reads: 2,
            store_loads: 1,
            roster_loads: 1,
            catalog_reads: 1,
            gathers: gathers_of(&FIRST_GATHERS),
            ..LATER_READ
        },
        "the ask's read"
    );
    assert_eq!(
        shown_email(&rig.timeline).as_deref(),
        Some(EMAIL),
        "the read is a whole footer's"
    );
    assert_eq!(
        rig.resolver.schedule.watched[&SESSION].arm,
        super::Arm::Watched,
        "every input and the process watched: the watched arm"
    );
    let exact = shown_weekly(&rig.timeline);
    assert!(exact.is_some(), "the figure is shown");

    let turns = rig.idle(Duration::ZERO, hour);
    assert_eq!(
        turns.iter().map(|turn| turn.at).collect::<Vec<_>>(),
        [801, 3_501],
        "a watched session's hour: {turns:?}"
    );
    let mut work = FooterWork::default();
    for turn in &turns {
        assert_eq!(turn.work, LATER_READ, "the read at {} s", turn.at);
        work += turn.work;
    }
    assert_eq!(
        work,
        FooterWork {
            derives: 2,
            birth_reads: 2,
            token_wants: 2,
            ..FooterWork::default()
        },
        "the hour after the ask reads no file"
    );
    assert_ne!(
        shown_weekly(&rig.timeline),
        exact,
        "the deadlines' reads moved the figure's words"
    );
    drop(rig);

    let mut rig = CauseRig::watched_by(ASKS, lay_weekly, refused);
    let _ = rig.ask(Duration::ZERO);
    assert_eq!(
        rig.resolver.schedule.watched[&SESSION].arm,
        super::Arm::Clocks,
        "CONTROL: with the watcher refused, the clocks arm"
    );
    let turns = rig.idle(Duration::ZERO, hour);
    let instants: Vec<u64> = [2, 6]
        .into_iter()
        .chain((21..=591).step_by(15))
        .chain([651, 711, 771, 801])
        .chain((861..=3_561).step_by(60))
        .collect();
    assert_eq!(
        turns.iter().map(|turn| turn.at).collect::<Vec<_>>(),
        instants,
        "the follow-ups, the 15 s clock to ten minutes, then the 60 s clock and the deadlines"
    );
    assert_eq!(
        1 + turns.iter().map(|turn| turn.work.derives).sum::<usize>(),
        92,
        "CONTROL: the clocks arm's hour reads the session 92 times"
    );
}

/// A DEADLINE THE MAC SLEPT PAST IS READ WITHIN A LOOK WHERE NO WAKE IS HEARD (law L4's wait, and
/// the input watch's wall clock watch, `claude_inputs::InputWatch::hears_wall`). A time a read
/// posts is a wall second, waited toward on the monotonic clock, which stops while the Mac
/// sleeps. The rig's session, whose accounts store holds its weekly figure (exact until +801),
/// is on the watched arm after its ask. Where the input watch's wall clock watch hears each sleep
/// (the rig's own, told by hand), a wake would be told as `Bell::Wall`, and the resolver waits
/// whole from +5 to +801. Where it hears none (here no wall clock watch is had at all; a macOS
/// watch whose power half was refused hears steps alone, which
/// `claude_inputs::tests::a_watch_hears_the_wall_only_where_its_wall_clock_watch_hears_each_sleep`
/// holds), the wait is held to `WALL_LOOK`, from +5 to +65. With the wall then an hour past the
/// monotonic clock and nothing told (a sleep no wake announced), the turn at +65 measures the
/// deadline again from the wall (`WatchSchedule::measure`) and reads the session for it, once,
/// as a later read's work, and the figure's words move; the looks after it to +200 read nothing.
///
/// NEGATIVE CONTROLS (run 2026-11-16 on macOS, each restored): with `WatchSchedule::next` waiting
/// whole whatever `wall_heard` says, the test fails at "the wait toward the deadline is held to a
/// look", its left instant 736 s past its right (+801 where +65 is wanted); with
/// `Resolver::serve_commands` not calling `WatchSchedule::measure`, it fails at "the look after
/// the sleep reads the deadline it slept past, once" with `left: [] right: [Turn { at: 65, … }]`.
#[test]
fn a_deadline_slept_past_is_read_within_a_look_where_no_wake_is_heard() {
    let (five, look) = (Duration::from_secs(5), Duration::from_secs(65));
    let mut rig = CauseRig::with(ASKS, lay_weekly);
    let _ = rig.ask(Duration::ZERO);
    assert!(
        rig.resolver.inputs.hears_wall(),
        "CONTROL: the rig's wall clock watch hears each sleep"
    );
    assert_eq!(
        rig.resolver.next(CauseRig::at(five)),
        Some(CauseRig::at(Duration::from_secs(801))),
        "CONTROL: where each wake is heard, the wait runs whole to the deadline"
    );
    drop(rig);

    let mut rig = CauseRig::with(ASKS, lay_weekly);
    rig.resolver.inputs.hear_wall_by(|| None);
    let _ = rig.ask(Duration::ZERO);
    assert_eq!(
        rig.resolver.schedule.watched[&SESSION].arm,
        super::Arm::Watched,
        "CONTROL: every input and the process watched: the watched arm"
    );
    assert!(
        !rig.resolver.inputs.hears_wall(),
        "no wall clock watch: no sleep is heard"
    );
    let exact = shown_weekly(&rig.timeline);
    assert!(exact.is_some(), "CONTROL: the figure is shown");
    assert_eq!(
        rig.resolver.next(CauseRig::at(five)),
        Some(CauseRig::at(look)),
        "the wait toward the deadline is held to a look"
    );
    script(|scratch| scratch.step = Duration::from_secs(3_600));
    let turns = rig.idle(five, Duration::from_secs(200));
    assert_eq!(
        turns,
        [Turn {
            at: look.as_secs(),
            work: LATER_READ,
        }],
        "the look after the sleep reads the deadline it slept past, once"
    );
    assert_ne!(
        shown_weekly(&rig.timeline),
        exact,
        "the deadline's read moved the figure's words"
    );
}

/// The bytes one read of [`a_fold_cut_short_is_read_on_at_its_floor`]'s session folds at most,
/// in the place of `footer::FOLD_BUDGET`.
const FOLD_SLICE: u64 = 1_024;

/// How many assistant rows [`lay_long`]'s transcript holds, each of 10 input and 7 output tokens.
const LONG_ROWS: u64 = 20;

/// [`lay`], its session's transcript [`LONG_ROWS`] responses long: more than one
/// [`FOLD_SLICE`].
fn lay_long() {
    lay();
    std::fs::create_dir_all(transcript().parent().unwrap()).unwrap();
    let rows: String = (0..LONG_ROWS)
        .map(|n| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-11-15T00:00:05Z","message":{{"id":"m{n:02}","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":7}}}}}}"#
            ) + "\n"
        })
        .collect();
    std::fs::write(transcript(), rows).unwrap();
}

/// The tokens a footer read of `timeline` shows, input and output summed over its models; `None`
/// while it shows none.
fn shown_tokens(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<(u64, u64)> {
    let held = timeline.lock().unwrap();
    let models = &held.claude_footer()?.usage.as_ref()?.models;
    (!models.is_empty()).then(|| {
        models
            .iter()
            .fold((0, 0), |(i, o), m| (i + m.input, o + m.output))
    })
}

/// A FOLD CUT SHORT IS READ ON AT ITS FLOOR (the cause of a read no file move makes): a watched
/// session whose transcript is longer than one read folds (here [`FOLD_SLICE`] bytes, in the
/// place of `footer::FOLD_BUDGET`'s 64 MiB) shows no tokens after its ask's read, which stopped
/// at the budget, and owes itself the next read at its floor (`Cause::Behind`). With no command
/// after the ask, the resolver reads it on every [`RECHECK`] until a read reaches the
/// transcript's end, each read a later read's work ([`LATER_READ`]: no gather, since no file
/// moved), and then the footer shows every row's tokens and nothing more is owed: no turn in the
/// rest of the hour.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `WatchSchedule::posted` owing no read for a fold left
/// behind (its `found.behind` arm taken for `false`), the test fails at "the fold left behind
/// owes the next read at the floor" with `left: None right: Some((…, Behind))`: nothing is owed,
/// so the footer shows no tokens until a file moves.
#[test]
fn a_fold_cut_short_is_read_on_at_its_floor() {
    let mut rig = CauseRig::with(ASKS, lay_long);
    rig.resolver.folds.insert(
        SESSION,
        aterm_agent::harness::footer::FooterCache::with_budget(FOLD_SLICE),
    );
    let _ = rig.ask(Duration::ZERO);
    assert_eq!(
        rig.resolver.schedule.watched[&SESSION].arm,
        super::Arm::Watched,
        "CONTROL: every input and the process watched: the watched arm"
    );
    assert_eq!(
        shown_tokens(&rig.timeline),
        None,
        "the ask's read stopped at its budget: no tokens yet"
    );
    assert_eq!(
        rig.resolver.schedule.inputs.get(&SESSION),
        Some(&(CauseRig::at(RECHECK), super::Cause::Behind)),
        "the fold left behind owes the next read at the floor"
    );
    let len = std::fs::metadata(transcript()).unwrap().len();
    let slices = len.div_ceil(FOLD_SLICE);
    assert!(
        slices > 2,
        "CONTROL: the transcript is more than two slices: {len}"
    );
    let turns = rig.idle(Duration::ZERO, Duration::from_secs(3_600));
    assert_eq!(
        turns.iter().map(|turn| turn.at).collect::<Vec<_>>(),
        (1..slices)
            .map(|n| n * RECHECK.as_secs())
            .collect::<Vec<_>>(),
        "the fold is read on at each floor until it catches up: {turns:?}"
    );
    for turn in &turns {
        assert_eq!(turn.work, LATER_READ, "the read at {} s", turn.at);
    }
    assert_eq!(
        shown_tokens(&rig.timeline),
        Some((LONG_ROWS * 10, LONG_ROWS * 7)),
        "caught up: every row's tokens"
    );
    assert!(
        !rig.resolver.schedule.inputs.contains_key(&SESSION),
        "caught up: nothing owed"
    );
}

/// A DEFAULT ASK READS THE SETTINGS CHAIN ALONE (the window's ask for the permission chip,
/// [`Command::Default`]): an ask read of a session on its stored login whose own figure is not
/// current reads no settings chain (and loads the accounts store, for its week); the window's ask
/// of the agent's default reads the chain once, at its pace, with no footer read, and an ask
/// inside that pace reads nothing.
///
/// NEGATIVE CONTROL (run 2026-11-15): with `(asks.default)(pid, at)` put back after each read in
/// `resolve_and_post_hearing`, the test fails at "the ask's read reads no settings chain" with
/// `settings_chain_reads: 1` where it allows 0.
#[test]
fn a_default_ask_reads_the_settings_chain_alone() {
    let mut rig = CauseRig::new();
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask,
        FooterWork {
            profile_decodes: 1,
            exec_arg_reads: 1,
            birth_reads: 2,
            store_loads: 1,
            roster_loads: 1,
            catalog_reads: 1,
            gathers: gathers_of(&FIRST_GATHERS),
            ..LATER_READ
        },
        "the ask's read reads no settings chain"
    );
    let pid = u32::try_from(pid()).expect("a pid");
    let default = rig.tell(Command::Default(pid), Duration::from_secs(5));
    assert_eq!(
        default,
        FooterWork {
            settings_chain_reads: 1,
            ..FooterWork::default()
        },
        "the window's ask reads the chain and no footer"
    );
    let again = rig.tell(Command::Default(pid), Duration::from_secs(6));
    assert_eq!(
        again,
        FooterWork::default(),
        "inside the chain's pace: nothing"
    );
}

/// UNCHANGED INPUTS ARE DECODED ONCE (law L5 OneDecodePerStamp, the exit test of the footer's
/// cause design's step C3): a hundred ask reads of two tabs on one agent, whose login profile,
/// accounts store, roster and catalog no one changes, decode the profile once, load the store
/// once, load the roster once and read the catalog once between them. A rewrite of the profile in
/// place to another address at the same length, its modification time in the same second as
/// before, is decoded at the next read, once for both tabs, and the footer names the new address.
///
/// NEGATIVE CONTROL (run 2026-11-14): with `footer_inputs::Stamp` taken as the file's length and
/// its modification time's whole second, the test fails at "the rewrite is decoded once" with
/// `profile_decodes: 0` (`left: 0 right: 1`): the old address is held. With
/// `footer_inputs::Memo::fresh` reading on every ask and `claude_facts::Catalogs::due` always
/// true, it fails at "a hundred reads of two tabs": `left: (100, 200, 100, 500, 100) right: (100,
/// 1, 1, 1, 1)`.
#[test]
fn unchanged_inputs_are_decoded_once() {
    let mut rig = CauseRig::new();
    let second = tab();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock past the epoch")
        .as_secs();
    let whole = UNIX_EPOCH + Duration::from_secs(wall + 2);
    let set_modified = |at: SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(profile_file())
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(at))
            .unwrap();
    };
    set_modified(whole + Duration::from_millis(100));
    let mut work = FooterWork::default();
    for n in 0..100u64 {
        let after = Duration::from_secs(n);
        work += if n % 2 == 0 {
            rig.ask(after)
        } else {
            rig.ask_in(SECOND, Arc::clone(&second), after)
        };
    }
    assert_eq!(
        (
            work.derives,
            work.profile_decodes,
            work.store_loads,
            work.roster_loads,
            work.catalog_reads
        ),
        (100, 1, 1, 1, 1),
        "a hundred reads of two tabs: {work:?}"
    );
    assert_eq!(shown_email(&rig.timeline).as_deref(), Some(EMAIL));
    assert_eq!(shown_email(&second).as_deref(), Some(EMAIL));

    let text = std::fs::read_to_string(profile_file()).unwrap();
    let rewritten = text.replacen(EMAIL, "other@example.test", 1);
    assert_eq!(text.len(), rewritten.len(), "at the same length");
    std::fs::write(profile_file(), rewritten).unwrap();
    set_modified(whole + Duration::from_millis(600));
    let mut work = rig.ask(Duration::from_secs(100));
    work += rig.ask_in(SECOND, Arc::clone(&second), Duration::from_secs(101));
    assert_eq!(
        work.profile_decodes, 1,
        "the rewrite is decoded once: {work:?}"
    );
    assert_eq!(
        shown_email(&rig.timeline).as_deref(),
        Some("other@example.test"),
        "the footer names the rewrite"
    );
    assert_eq!(shown_email(&second).as_deref(), Some("other@example.test"));
}

/// THE FOOTER NEVER READS THE TOKEN VIEW (law L7 of the footer's cause design; the footer half
/// of its C2 exit test, whose lane half is `token_lane_tests::the_lane_reads_only_for_its_causes`):
/// 1800 asks of the rig's session, one every 2 s for an hour, as the status sweep makes them for
/// a tab that writes all the while, read the session 1800 times, each asking the token view's
/// lane once and reading the token view not at all on the footer's thread.
///
/// NEGATIVE CONTROL (run 2026-11-14): with `crate::token_lane::want()` in
/// `resolve_facts_at_with_cwd` replaced by the lane's own read inline
/// (`crate::token_lane::Here.read(clock.now)`), the test fails at "the hour of asks" with
/// `token_view_reads: 1800, token_wants: 0`.
#[test]
fn the_footer_never_reads_the_token_view() {
    let mut rig = CauseRig::new();
    let mut hour = FooterWork::default();
    for at in (0..3_600).step_by(2) {
        hour += rig.ask(Duration::from_secs(at));
    }
    assert_eq!(
        (hour.derives, hour.token_view_reads, hour.token_wants),
        (1_800, 0, 1_800),
        "the hour of asks: {hour:?}"
    );
}

/// AN IMAGE READS ITS EXEC ARGS ONCE (law L9 ExitEndsWatch; the exit test of the footer's cause
/// design's step C4): a hundred ask reads of the rig's session read the agent's exec image once
/// between them, the process watch the first read placed on it (before it read the image) standing
/// for the rest. The watch hearing it exec (scripted: the [`Command::ProcessMoved`] its forwarder
/// tells of a `NOTE_EXEC`) drops the held image, and the read the exec owes reads the new one:
/// two in all. Its exit (scripted, as the forwarder tells a `NOTE_EXIT`) stops the session: no
/// watch is placed after it, no tail and no fold are kept, and in the hour after it the resolver
/// takes no turn and reads no process's kernel start.
///
/// NEGATIVE CONTROL (run 2026-11-15): with `Resolver::serve_commands` ignoring the exits it
/// accepted (no `Resolver::exit`), the test fails at "no watch after the exit" with
/// `left: 1 right: 0`: the process watch still stands. With the pass's image read again on
/// every pass instead of `Identity::confirmed_image` (an `exec_args` read and its argv and
/// stamp filter put back in `resolve_facts_at_with_cwd`), it fails at "a hundred reads" with
/// `left: (100, 101) right: (100, 1)`.
#[test]
fn an_image_reads_its_exec_args_once() {
    let mut rig = CauseRig::new();
    let mut work = FooterWork::default();
    for n in 0..100u64 {
        work += rig.ask(Duration::from_secs(n));
    }
    assert_eq!(
        (work.derives, work.exec_arg_reads),
        (100, 1),
        "a hundred reads: {work:?}"
    );
    let (pid, started) = rig.process();
    let exec = rig.tell(
        Command::ProcessMoved {
            pid,
            started,
            exited: false,
        },
        Duration::from_secs(100),
    );
    work += exec;
    assert_eq!(
        (exec.derives, work.exec_arg_reads),
        (1, 2),
        "the exec's read reads the new image: {exec:?}"
    );
    assert_eq!(
        shown_email(&rig.timeline).as_deref(),
        Some(EMAIL),
        "the new image's read is a whole footer's"
    );
    assert!(
        rig.resolver.inputs.holds_process(pid, started),
        "CONTROL: the process watch stands until the exit"
    );

    let exit = rig.tell(
        Command::ProcessMoved {
            pid,
            started,
            exited: true,
        },
        Duration::from_secs(101),
    );
    assert_eq!(rig.resolver.inputs.placed(), 0, "no watch after the exit");
    assert_eq!(rig.resolver.tails.kept.len(), 0, "no tail after the exit");
    assert_eq!(rig.resolver.folds.len(), 0, "no fold after the exit");
    assert_eq!(
        exit.derives, 0,
        "the stopped session is read for no cause: {exit:?}"
    );
    let turns = rig.idle(Duration::from_secs(101), Duration::from_secs(3_701));
    assert_eq!(turns, Vec::new(), "no turn in the hour after the exit");
    assert_eq!(
        take_footer_work().birth_reads,
        0,
        "no process's start is read in the hour after the exit"
    );
}

/// One input of [`each_input_is_read_when_its_file_moves`]: the file a change of it changes, and
/// the change, as its writer makes it.
type Rewrite = (Input, PathBuf, Box<dyn Fn()>);

/// The branch a footer read of `timeline` names.
fn shown_branch(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<String> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.branch.clone())
}

/// EACH INPUT IS READ WHEN ITS FILE MOVES (laws L2 EveryReadFileIsWatched, L6 FloorHolds and L8
/// HeartbeatIsNoRead; the exit test of the footer's cause design's step C5): a tapped agent's
/// session whose read looks at its registry row, transcript, subagent, git `HEAD`, tap reading,
/// login profile, roster, accounts store and catalog folder is asked once, which puts it on the
/// watched arm (no follow-up reads it); then each of those files is changed in turn, 2 s apart
/// (each read's floor), as
/// its writer changes it: the watch on it fires, and the read that owes counts exactly one gather,
/// of that input, and one derive. Ten appends to the transcript inside one second are told ten
/// times and read once, at the floor's end. A tap re-report (a touch of its reading) is told by
/// no watch and gathers nothing.
///
/// NEGATIVE CONTROL (run 2026-11-15): with `Resolver::settle` placing no watch for the git
/// looks (the `HEAD` read looked at and never watched), the test fails at "a watch fired for git
/// at <root>/work/.git/HEAD"; with `held_git_head` noting no look, it fails at "what the ask's
/// read looked at", `Git` missing from the inputs looked at. With `WatchSchedule::due_inputs`
/// taking each owed read at once, its floor ignored, it fails at "ten appends inside one second"
/// with `left: (10, 10) right: (1, 1)`: ten derives and ten transcript gathers. With the tap's
/// reading looked at for every change (`Heed::All` in `Identity::tap_reading`: an `add_file`
/// watch, attribute events on), it fails at "a re-report is heard by no watch: 1 told".
#[test]
fn each_input_is_read_when_its_file_moves() {
    let mut rig = CauseRig::with(TAPPED_ASKS, lay_inputs);
    let ask = rig.ask(Duration::ZERO);
    let looked: BTreeSet<Input> = rig.resolver.looks[&SESSION]
        .iter()
        .map(|look| look.input)
        .collect();
    assert_eq!(
        looked,
        BTreeSet::from([
            Input::Row,
            Input::Transcript,
            Input::Subagents,
            Input::Git,
            Input::Tap,
            Input::Profile,
            Input::Roster,
            Input::Store,
            Input::Catalog,
        ]),
        "what the ask's read looked at: {ask:?}"
    );
    assert_eq!(shown_branch(&rig.timeline).as_deref(), Some("main"));
    let _ = rig.idle(Duration::ZERO, Duration::from_secs(6));
    let _ = rig.heard();
    let _ = take_footer_work();

    let roster = root().join(accounts::ACCOUNTS_FILE);
    let head = work().join(".git").join("HEAD");
    let rewrites: [Rewrite; 9] = [
        (
            Input::Row,
            records().join(format!("sessions/{}.json", pid())),
            Box::new(|| {
                let row = records().join(format!("sessions/{}.json", pid()));
                let text = std::fs::read_to_string(&row).unwrap();
                std::fs::write(&row, text).unwrap();
            }),
        ),
        (
            Input::Transcript,
            transcript(),
            Box::new(|| append(&transcript())),
        ),
        (
            Input::Subagents,
            subagent(),
            Box::new(|| append(&subagent())),
        ),
        (
            Input::Git,
            head.clone(),
            Box::new(move || {
                let lock = work().join(".git").join("HEAD.lock");
                std::fs::write(&lock, "ref: refs/heads/topic\n").unwrap();
                std::fs::rename(&lock, work().join(".git").join("HEAD")).unwrap();
            }),
        ),
        (
            Input::Tap,
            reading_file(),
            Box::new(|| write_reading(wall_now())),
        ),
        (
            Input::Profile,
            profile_file(),
            Box::new(|| {
                let text = std::fs::read_to_string(profile_file()).unwrap();
                std::fs::write(profile_file(), text).unwrap();
            }),
        ),
        (
            Input::Roster,
            roster.clone(),
            Box::new(move || std::fs::write(root().join(accounts::ACCOUNTS_FILE), "").unwrap()),
        ),
        (
            Input::Store,
            accounts::Store::at(&state()).state_file(),
            Box::new(|| {
                let mut store = accounts::State::default();
                store.accounts.insert(
                    aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL.into(),
                    accounts::Account::default(),
                );
                accounts::Store::at(&state()).save(&store).unwrap();
            }),
        ),
        (
            Input::Catalog,
            state().join("upgrade").join("builds"),
            Box::new(|| {
                std::fs::write(state().join("upgrade").join("builds").join("2.1.287"), "").unwrap();
            }),
        ),
    ];
    let mut at = 8;
    for (input, path, rewrite) in &rewrites {
        rewrite();
        let heard = rig.heard();
        assert!(
            heard
                .iter()
                .any(|told| matches!(told, Command::InputMoved(moved) if moved == path)),
            "a watch fired for {} at {}",
            input.word(),
            path.display()
        );
        let work = rig.tell_all(heard, Duration::from_secs(at));
        assert_eq!(
            (work.derives, work.gathers),
            (1, gathers_of(&[*input])),
            "the read {} owes: {work:?}",
            input.word()
        );
        at += 2;
    }
    assert_eq!(
        shown_branch(&rig.timeline).as_deref(),
        Some("topic"),
        "the HEAD's rewrite moved the branch"
    );

    let from = Duration::from_secs(at - 2);
    let mut burst = FooterWork::default();
    for tenth in 1..=10u64 {
        append(&transcript());
        let heard = rig.heard();
        burst += rig.tell_all(heard, from + Duration::from_millis(100 * tenth));
    }
    for turn in rig.idle(from + Duration::from_secs(1), from + RECHECK) {
        burst += turn.work;
    }
    assert_eq!(
        (burst.derives, burst.gathers[Input::Transcript.index()]),
        (1, 1),
        "ten appends inside one second: {burst:?}"
    );

    let touch = SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(reading_file())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(touch))
        .unwrap();
    let heard = rig.heard();
    assert!(
        !heard
            .iter()
            .any(|told| matches!(told, Command::InputMoved(moved) if *moved == reading_file())),
        "a re-report is heard by no watch: {} told",
        heard.len()
    );
    let work = rig.tell_all(heard, from + RECHECK + Duration::from_secs(1));
    assert_eq!(
        work.gathers, NO_GATHERS,
        "a re-report gathers nothing: {work:?}"
    );
}

/// Why a footer read of `timeline` shows no weekly figure.
fn shown_gap(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<WeeklyGap> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.subscription.as_ref()?.weekly_gap)
}

/// [`lay`], its roster listing the login's account as its one row, so a read's quota view asks
/// the accounts outlook, which replays it ([`OUTLOOK_READS`]).
fn lay_listed() {
    lay();
    std::fs::write(
        root().join(accounts::ACCOUNTS_FILE),
        format!("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"{EMAIL}\"\n"),
    )
    .unwrap();
}

/// The accounts outlook's reads in the rig: the scratch harness state, early rotation with no
/// usage credits, UTC, and the scripted journal ([`Scratch::journal`]), each stand-in noting the
/// host accessor it stands for.
const OUTLOOK_READS: crate::accounts_outlook::Reads = crate::accounts_outlook::Reads {
    state_dir: || Some(state()),
    policy: || {
        noted("accounts_mode");
        noted("usage_credits");
        (accounts::Mode::Early, false)
    },
    offset: |_| {
        noted("local_offset");
        0
    },
    journal: || {
        noted("journal");
        with_scratch(|scratch| scratch.journal.clone())
    },
};

/// A journal read at wall second `at` that says one account of the lab was last used by `m3`.
fn journal_read(at: i64) -> alab_seen::Journal {
    alab_seen::Journal::Read(Arc::new(alab_seen::Seen {
        accounts: std::collections::BTreeMap::from([(
            "ac-cause".to_owned(),
            alab_seen::AccountSeen {
                last_use: Some(alab_seen::LastUse {
                    by: "m3".into(),
                    t: 1_000,
                }),
                ..alab_seen::AccountSeen::default()
            },
        )]),
        at,
        ..alab_seen::Seen::default()
    }))
}

/// The host moves among `heard`, in order.
fn host_moves(heard: &[Command]) -> Vec<Bell> {
    heard
        .iter()
        .filter_map(|told| match told {
            Command::HostMoved(bell) => Some(*bell),
            _ => None,
        })
        .collect()
}

/// What the resolver's thread hears from the input watch once its wall clock watch is told
/// `moved` ([`crate::claude_inputs::InputWatch::wall`]).
fn told_wall(rig: &mut CauseRig, moved: aterm_uds::wall_watch::Moved) -> Vec<Command> {
    rig.resolver
        .inputs
        .wall()
        .expect("a live input watch polls its wall clock watch")
        .tell(moved);
    rig.heard()
}

/// The rig's wall second at instant `at`.
fn wall_second(at: Instant) -> i64 {
    let wall = wall(at)
        .duration_since(UNIX_EPOCH)
        .expect("a clock past the epoch");
    i64::try_from(wall.as_secs()).expect("a second")
}

/// Each read the resolver's own turns make from `from` to `until` past the rig's start, taken as
/// [`CauseRig::idle`] takes them: its second past the start, whether it replayed the accounts
/// outlook, and whether the time the outlook's last replay left a replay owed at had come at it
/// (`OutlookMemo::replay_owed_at`).
fn outlook_reads(rig: &mut CauseRig, from: Duration, until: Duration) -> Vec<(i64, bool, bool)> {
    let wall0 = wall_second(CauseRig::at(Duration::ZERO));
    let (mut now, end) = (CauseRig::at(from), CauseRig::at(until));
    let mut reads = Vec::new();
    loop {
        rig.resolver.retain(now);
        let Some(at) = rig.resolver.next(now).filter(|at| *at <= end) else {
            break;
        };
        now = at;
        let owed = rig.resolver.machine.outlook.replay_owed_at();
        rig.resolver.turn(std::iter::empty(), || now);
        let work = take_footer_work();
        let second = wall_second(now);
        if work.derives > 0 {
            reads.push((
                second - wall0,
                work.outlook_replays > 0,
                owed.is_some_and(|owed| owed <= second),
            ));
        }
    }
    reads
}

/// THE OUTLOOK REPLAYS AT ITS OWN DEADLINES ALONE. A session whose roster lists its account asks
/// the accounts outlook at each read (`OutlookMemo::at`). Over the hour after its ask, with no
/// input moved, it is read at each time its reads posted, the outlook's own deadlines among them
/// (`Outlook::changes_after`), and at two host bells (`Bell::Config`, at +600 s and +3000 s, each
/// past the replay floor, `REPLAY_FLOOR_S`, since the replay before it unless a deadline's came
/// within it). Each read replays the outlook exactly where the time its last replay left a
/// replay owed at has come: the deadline that replay computed, or the floor's end after it where
/// an input moved since (here the ask's own plan row, `OutlookMemo::note_plan`, which owes the
/// replay at +60 s); never at a read another cause made, however long since that replay. (A
/// forecast whose pace stands, or whose span is too short yet, is said again each minute,
/// `outlook::DRIFT_S`: there every read is the outlook's own, as with a weekly figure stored.)
///
/// NEGATIVE CONTROL (run 2026-11-16): with `|| floor` back among `OutlookMemo::at`'s causes (a
/// replay at any read a minute past the last), the test fails at "the read at +600 s replays
/// exactly where its deadline came" with `left: true right: false`.
#[test]
fn the_outlook_replays_at_its_own_deadlines_alone() {
    let hour = Duration::from_secs(3_600);
    let mut rig = CauseRig::with(ASKS, lay_listed);
    rig.resolver.machine.outlook.reads = Some(OUTLOOK_READS);
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask.outlook_replays, 1,
        "CONTROL: the ask's read replays the outlook: {ask:?}"
    );
    let wall0 = wall_second(CauseRig::at(Duration::ZERO));
    let mut reads = Vec::new();
    let mut from = Duration::ZERO;
    for bell_at in [Duration::from_secs(600), Duration::from_secs(3_000)] {
        reads.extend(outlook_reads(&mut rig, from, bell_at));
        let owed = rig.resolver.machine.outlook.replay_owed_at();
        let bell = rig.tell(Command::HostMoved(Bell::Config), bell_at);
        assert_eq!(
            bell.derives,
            1,
            "CONTROL: the bell at +{} s re-reads the session: {bell:?}",
            bell_at.as_secs()
        );
        let at = wall_second(CauseRig::at(bell_at));
        reads.push((
            at - wall0,
            bell.outlook_replays > 0,
            owed.is_some_and(|owed| owed <= at),
        ));
        from = bell_at;
    }
    reads.extend(outlook_reads(&mut rig, from, hour));
    for (at, replayed, owed) in &reads {
        assert_eq!(
            replayed, owed,
            "the read at +{at} s replays exactly where its deadline came: {reads:?}"
        );
    }
}

/// A NEW TIME ZONE REPLAYS THE ACCOUNTS OUTLOOK. The outlook's local days and times (its
/// midnights, its `↺ Sat 10:00`) are reckoned in the zone's offsets, so a new zone told by the
/// input watch (`Bell::Zone`) replays it at once, inside the replay's floor and with nothing else
/// moved, where a wall move that leaves every deadline ahead replays nothing.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `Resolver::serve_commands` not calling
/// `OutlookMemo::zone_set`, the test fails at "a new zone replays the outlook once" with `left:
/// (1, 0) right: (1, 1)`.
#[test]
fn a_new_zone_replays_the_outlook() {
    let mut rig = CauseRig::with(ASKS, lay_listed);
    rig.resolver.machine.outlook.reads = Some(OUTLOOK_READS);
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask.outlook_replays, 1,
        "CONTROL: the ask's read replays the outlook: {ask:?}"
    );
    let wall = rig.tell(Command::HostMoved(Bell::Wall), Duration::from_secs(10));
    assert_eq!(
        (wall.derives, wall.outlook_replays),
        (1, 0),
        "CONTROL: a wall move inside the floor replays nothing: {wall:?}"
    );
    let zone = rig.tell(Command::HostMoved(Bell::Zone), Duration::from_secs(20));
    assert_eq!(
        (zone.derives, zone.outlook_replays),
        (1, 1),
        "a new zone replays the outlook once: {zone:?}"
    );
}

/// A process that stands in for a sign-in's holder, killed and reaped when it drops.
struct Holder(aterm_uds::spawnfd::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A SIGN-IN KILLED MID-RUN ENDS AT ITS HOLDER'S EXIT. The sign-in record (`signin-running`)
/// names the process that runs it, the sign-in lock's holder, and a kill leaves the record
/// standing, so no file watch tells the sign-in's end. The resolver watches the holder the
/// accounts outlook last read running (`Resolver::watch_sign_in`), which keeps the session on the
/// watched arm, and the holder's exit owes each session that looked at the record a read
/// (`Cause::Input`), which replays the outlook with the sign-in gone: the row stops saying
/// `signing in` at the exit, with no clock and no file moved (what the ask's own read wrote is
/// told and read before the kill).
///
/// NEGATIVE CONTROL (run 2026-11-16): with `Resolver::watch_sign_in` placing no watch (it
/// returns `false` at once), the test fails at "the holder's exit replays the outlook once" with
/// `left: (0, 0) right: (1, 1)`: the session waits, still `signing in`, for a file to move.
#[test]
fn a_killed_sign_in_ends_at_its_holders_exit() {
    let mut rig = CauseRig::with(ASKS, lay_listed);
    rig.resolver.machine.outlook.reads = Some(OUTLOOK_READS);
    let mut holder = Holder(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("a stand-in holder"),
    );
    let started = with_scratch(|scratch| scratch.wall0)
        .duration_since(UNIX_EPOCH)
        .expect("a clock past the epoch")
        .as_secs();
    let _record = accounts::Store::at(&state())
        .signing_in_fixture_by(
            "main",
            i64::try_from(started).expect("a second"),
            holder.0.id(),
        )
        .expect("a record naming the stand-in");
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask.outlook_replays, 1,
        "CONTROL: the ask's read replays the outlook: {ask:?}"
    );
    assert_eq!(
        rig.resolver
            .machine
            .outlook
            .sign_in_running()
            .map(accounts::RunningSignIn::label),
        Some("main"),
        "CONTROL: the outlook reads the sign-in running"
    );
    assert_eq!(
        rig.resolver.schedule.watched[&SESSION].arm,
        super::Arm::Watched,
        "the holder is watched with the rest: the watched arm"
    );
    // What the ask's own read wrote (its plan row in the series) is told and read first, so
    // the exit is the only thing told after the kill.
    let written = rig.heard();
    let _ = rig.tell_all(written, Duration::from_secs(5));
    let rest = rig.heard();
    assert!(
        rest.is_empty(),
        "CONTROL: nothing else is told before the kill ({} told)",
        rest.len()
    );
    holder.0.kill().expect("kill the stand-in");
    holder.0.wait().expect("reap the stand-in");
    let heard = rig.heard();
    let ended = rig.tell_all(heard, Duration::from_secs(10));
    assert_eq!(
        (ended.derives, ended.outlook_replays),
        (1, 1),
        "the holder's exit replays the outlook once: {ended:?}"
    );
    assert!(
        rig.resolver.machine.outlook.sign_in_running().is_none(),
        "the sign-in ended with its holder"
    );
}

/// A HOST MOVE RE-READS EVERY WATCHED SESSION WITHOUT READING A FILE (the exit test of the
/// footer's cause design's step C8). A host fact a read takes moves, and no clock polls for it:
/// its bell ([`Command::HostMoved`]) re-reads each watched session once, on either arm, and each
/// re-read is a later read's work ([`LATER_READ`]: a derive, a kernel start read and an ask of
/// the token view's lane, no gather).
/// - A window switch: two watched tabs on one agent, the usage tap switched off (a config
///   reload, `Bell::Config`): two derives and no gather, and each tab's weekly now says the tap
///   is off.
/// - The lab's journal: one tab whose roster lists its account, so its read asks the accounts
///   outlook (`OutlookMemo::at`). A journal that says something new (`Bell::Journal`) replays
///   the outlook once; one read again that says the same at a later instant replays nothing.
/// - The wall clock: one tab whose accounts store holds its weekly figure (exact until +801).
///   The wall stepped 500 s ahead at +10, told by the input watch's wall clock watch as the
///   forwarder hears it (`Bell::Wall`), is one re-read, and the next wait is the figure's
///   deadline on the stepped wall (+301), never the one the last read posted. A new time zone
///   told the same way (`Bell::Zone`) is one re-read too.
///
/// NEGATIVE CONTROLS (run 2026-11-16): with `WatchSchedule::accept` taking
/// `Command::HostMoved(_)` as nothing, the test fails at "the config bell" with `derives: 0`
/// where it wants 2; with `accounts_outlook::says_the_same` comparing the whole journal
/// (`held == now`, its read instant included), it fails at "a re-read that says the same" with
/// `left: 1 right: 0`.
#[test]
fn a_host_move_rederives_without_reading() {
    let mut rig = CauseRig::new();
    let second = tab();
    let _ = rig.ask(Duration::ZERO);
    let _ = rig.ask_in(SECOND, Arc::clone(&second), Duration::from_secs(1));
    let tapped = shown_gap(&rig.timeline);
    assert!(
        tapped.is_some() && tapped != Some(WeeklyGap::TapOff),
        "CONTROL: with the switch on, the gap is another: {tapped:?}"
    );
    script(|scratch| scratch.tap_on = false);
    let config = rig.tell(Command::HostMoved(Bell::Config), Duration::from_secs(10));
    assert_eq!(
        config,
        FooterWork {
            derives: 2,
            birth_reads: 2,
            token_wants: 2,
            ..FooterWork::default()
        },
        "the config bell re-reads both tabs and gathers nothing: {config:?}"
    );
    assert_eq!(shown_gap(&rig.timeline), Some(WeeklyGap::TapOff));
    assert_eq!(shown_gap(&second), Some(WeeklyGap::TapOff));
    drop(rig);

    let mut rig = CauseRig::with(ASKS, lay_listed);
    rig.resolver.machine.outlook.reads = Some(OUTLOOK_READS);
    let ask = rig.ask(Duration::ZERO);
    assert_eq!(
        ask.outlook_replays, 1,
        "CONTROL: the ask's read replays the outlook: {ask:?}"
    );
    script(|scratch| scratch.journal = journal_read(7));
    let journal = rig.tell(Command::HostMoved(Bell::Journal), Duration::from_secs(10));
    assert_eq!(
        (journal.derives, journal.outlook_replays),
        (1, 1),
        "the journal's move replays the outlook once: {journal:?}"
    );
    script(|scratch| scratch.journal = journal_read(307));
    let again = rig.tell(Command::HostMoved(Bell::Journal), Duration::from_secs(20));
    assert_eq!(
        again.outlook_replays, 0,
        "a re-read that says the same replays nothing: {again:?}"
    );
    drop(rig);

    let mut rig = CauseRig::with(ASKS, lay_weekly);
    let _ = rig.ask(Duration::ZERO);
    assert_eq!(
        rig.resolver.next(CauseRig::at(Duration::from_secs(5))),
        Some(CauseRig::at(Duration::from_secs(801))),
        "CONTROL: the figure turns a bound at +801"
    );
    script(|scratch| scratch.step = Duration::from_secs(500));
    let heard = told_wall(
        &mut rig,
        aterm_uds::wall_watch::Moved {
            stepped: true,
            ..aterm_uds::wall_watch::Moved::default()
        },
    );
    assert_eq!(
        host_moves(&heard),
        [Bell::Wall],
        "the forwarder tells the step: {} told",
        heard.len()
    );
    let wall = rig.tell_all(heard, Duration::from_secs(10));
    assert_eq!(wall, LATER_READ, "the step is one re-read: {wall:?}");
    assert_eq!(
        rig.resolver.next(CauseRig::at(Duration::from_secs(10))),
        Some(CauseRig::at(Duration::from_secs(301))),
        "the next wait is the deadline on the stepped wall"
    );
    let heard = told_wall(
        &mut rig,
        aterm_uds::wall_watch::Moved {
            zoned: true,
            ..aterm_uds::wall_watch::Moved::default()
        },
    );
    assert_eq!(
        host_moves(&heard),
        [Bell::Zone],
        "the forwarder tells the zone: {} told",
        heard.len()
    );
    let zone = rig.tell_all(heard, Duration::from_secs(20));
    assert_eq!(zone, LATER_READ, "a new zone is one re-read: {zone:?}");
}

/// The branches the rig's git `HEAD` names at each version of it the cause model counts
/// (`disk`, `held`).
const BRANCHES: [&str; 4] = ["main", "topic", "other", "fourth"];

/// The rig's git `HEAD`, in its working directory's repository.
fn head() -> PathBuf {
    work().join(".git").join("HEAD")
}

/// Write version `n` of the rig's `HEAD` in place, as one writer's write.
fn write_head(n: usize) {
    std::fs::write(head(), format!("ref: refs/heads/{}\n", BRANCHES[n])).unwrap();
}

/// [`lay`], its working directory a repository on [`BRANCHES`]`[0]`.
fn lay_repository() {
    lay();
    std::fs::create_dir_all(work().join(".git")).unwrap();
    write_head(0);
}

/// The cause model's state, as the rig's resolver holds it for the rig's session and its
/// `HEAD`: the version of `HEAD` its footer names (`held`), whether a watch stands on `HEAD`
/// (`armed`), whether a read of the session is owed (`owed`), and the reads run so far (`reads`,
/// counted by the caller).
fn cause_projection(rig: &CauseRig, reads: usize) -> BTreeMap<&'static str, i64> {
    let branch = shown_branch(&rig.timeline);
    let held = branch
        .as_deref()
        .and_then(|branch| BRANCHES.iter().position(|named| *named == branch))
        .unwrap_or_else(|| panic!("the footer names one of the rig's branches: {branch:?}"));
    BTreeMap::from([
        ("held", i64::try_from(held).expect("a version")),
        ("armed", i64::from(rig.resolver.inputs.holds(&head()))),
        (
            "owed",
            i64::from(rig.resolver.schedule.inputs.contains_key(&SESSION)),
        ),
        ("reads", i64::try_from(reads).expect("a count")),
    ])
}

/// The cause model's `state` on the variables [`cause_projection`] reads.
fn model_projection(state: &BTreeMap<&'static str, i64>) -> BTreeMap<&'static str, i64> {
    ["held", "armed", "owed", "reads"]
        .into_iter()
        .map(|var| (var, state[var]))
        .collect()
}

/// THE CAUSE MODEL BINDS THE SCHEDULE (law L3 PlaceBeforeRead and `NoReadWithoutCause` of
/// `aterm_models_harness::claude_footer_cause_model`; the exit test of the footer's cause
/// design's step C9). The rig's session, its working directory a repository whose `HEAD` its
/// footer read looks at, is driven through each of the model's actions on one input, `HEAD`,
/// and after each the resolver's state, projected ([`cause_projection`]: the branch shown, the
/// watch on `HEAD`, the read owed, the reads run), is the model's:
/// - `Ask`, `Read`: the tab's ask is read at once; then the turn's settle places the watch the
///   read's look wants (`Place`), which owes nothing, since `HEAD` did not move since the read.
/// - `Write`: `HEAD` written while watched is told, and owes a read at the floor; the read at
///   the floor (`Read`) names the new branch.
/// - `Lose`: the kernel lets the watch go (unheard); an ask is read (`Ask`, `Read`), and
///   `HEAD` is written in the gap between that read and the settle that places its watch again
///   (`Write`, told by no watch). The placement finds the look moved and owes a read (`Place`;
///   the model with `Buggy = 1`, a placement that owes nothing, loses the write there:
///   `NoLostChange` fails). A write between that placement and the read it owes (`Write`) is
///   told, and the one read at the floor names it.
/// - Then nothing is owed, and in the hour after, the resolver takes no turn and the model has no
///   read to run: every read paid its cause.
///
/// The committed model breaks no law; its `Buggy = 1` breaks `NoLostChange` and its `Buggy = 2`
/// breaks `NoReadWithoutCause`.
///
/// NEGATIVE CONTROLS (run 2026-11-16): with `Resolver::settle` owing no read for a placement
/// (its `Cause::Placed` loop deleted), the test fails at "the placement owes the read the gap's
/// write needs" with `left: {"armed": 1, "held": 1, "owed": 0, "reads": 3} right: {"armed": 1,
/// "held": 1, "owed": 1, "reads": 3}`: the footer keeps the old branch with nothing owed. With
/// it owing a read of each session a path placed afresh serves, moved or not, the test fails at
/// "the ask's settle places the watch and owes nothing" with `left: {…, "owed": 1, "reads": 1}
/// right: {…, "owed": 0, "reads": 1}`. With the model's committed `Buggy = 1` it fails at the
/// gap's placement with `left: {…, "owed": 1, …} right: {…, "owed": 0, …}`, and with `Buggy = 2`
/// at "the hour after owes the model no read" (`reads: 4, causes: 4`, a read enabled anyway).
#[test]
fn the_cause_model_binds_the_schedule() {
    let model = aterm_models_harness::claude_footer_cause_model();
    let mut state = model.init_state();
    let mut rig = CauseRig::with(ASKS, lay_repository);
    let mut reads = 0usize;
    let fire = |state: &mut BTreeMap<&'static str, i64>, actions: &[&str]| {
        for action in actions {
            assert!(model.fire(action, state), "the model admits {action}");
        }
    };
    let request = |rig: &CauseRig| {
        Command::Request(Job {
            session: SESSION,
            timeline: Arc::clone(&rig.timeline),
            pgid: pid(),
            wake: LogWake::default(),
        })
    };

    let t = CauseRig::at(Duration::ZERO);
    let ask = request(&rig);
    rig.resolver.serve_commands([ask], &mut || t);
    reads += take_footer_work().derives;
    fire(&mut state, &["Ask", "Read"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the ask is read at once, before any watch"
    );
    rig.resolver.settle(&mut || t);
    fire(&mut state, &["Place"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the ask's settle places the watch and owes nothing"
    );

    let _ = rig.heard();
    write_head(1);
    let heard = rig.heard();
    assert!(
        heard
            .iter()
            .any(|told| matches!(told, Command::InputMoved(moved) if *moved == head())),
        "the watch hears the write: {} told",
        heard.len()
    );
    reads += rig.tell_all(heard, Duration::from_secs(1)).derives;
    fire(&mut state, &["Write"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "a watched write owes a read at the floor"
    );
    reads += rig
        .idle(Duration::from_secs(1), Duration::from_secs(2))
        .iter()
        .map(|turn| turn.work.derives)
        .sum::<usize>();
    fire(&mut state, &["Read"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the floor's read names the write"
    );

    rig.resolver.inputs.lose_watch(&head());
    // Notifications from before the loss do not belong to the gap's write below.
    let _ = rig.heard();
    fire(&mut state, &["Lose"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the watch is let go"
    );
    let t = CauseRig::at(Duration::from_secs(4));
    let ask = request(&rig);
    rig.resolver.serve_commands([ask], &mut || t);
    reads += take_footer_work().derives;
    fire(&mut state, &["Ask", "Read"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the ask is read with the watch let go"
    );
    write_head(2);
    let heard = rig.heard();
    assert!(
        !heard
            .iter()
            .any(|told| matches!(told, Command::InputMoved(moved) if *moved == head())),
        "the write in the gap is told by no watch"
    );
    fire(&mut state, &["Write"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the gap's write owes nothing yet"
    );
    let mut lost = state.clone();
    rig.resolver.settle(&mut || t);
    fire(&mut state, &["Place"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the placement owes the read the gap's write needs"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(buggy.fire("Place", &mut lost));
    assert!(
        !buggy.check_invariant("NoLostChange", &lost),
        "CONTROL: a placement that owes nothing loses the gap's write: {lost:?}"
    );

    write_head(3);
    let heard = rig.heard();
    reads += rig.tell_all(heard, Duration::from_secs(5)).derives;
    fire(&mut state, &["Write"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "a write before the read the placement owes folds into it"
    );
    reads += rig
        .idle(Duration::from_secs(5), Duration::from_secs(6))
        .iter()
        .map(|turn| turn.work.derives)
        .sum::<usize>();
    fire(&mut state, &["Read"]);
    assert_eq!(
        cause_projection(&rig, reads),
        model_projection(&state),
        "the one read at the floor names the newest write"
    );

    let hour = rig.idle(Duration::from_secs(6), Duration::from_secs(3_606));
    assert_eq!(hour, Vec::new(), "no turn in the hour after");
    assert!(
        !model.action_enabled("Read", &state),
        "the hour after owes the model no read: {state:?}"
    );
    assert!(
        model
            .invariants
            .iter()
            .all(|law| model.check_invariant(law.name, &state)),
        "every law holds at the end: {state:?}"
    );
    assert_eq!(
        aterm_spec::interp::violated_invariants(&model),
        BTreeSet::new(),
        "the committed model breaks no law"
    );
    for (dial, law) in [(1, "NoLostChange"), (2, "NoReadWithoutCause")] {
        let broken =
            aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(&model, dial));
        assert!(
            broken.contains(law),
            "CONTROL: Buggy = {dial} breaks {law}: {broken:?}"
        );
    }
}

/// EVERY HOST FACT A DERIVE READS IS BELLED (law L10 HostFactsBelled of the footer's cause
/// design): over an ask and a re-read of each of the rig's sessions, a plain one and one whose
/// roster lists its account (its read asking the accounts outlook), every host accessor read on
/// the resolver's thread ([`crate::host_facts::noted`]: the live accessors and the rig's
/// stand-ins of them) is a member of [`crate::host_facts::MEMBERS`], whose bell answers for it.
/// The reads include the window's switches, the restart offer, the tap's room, the journal and
/// the local offset, so the law is not held over nothing.
///
/// NEGATIVE CONTROL (run 2026-11-16): with a read of `crate::harness_host::upgrade_ask()` (a
/// switch no bell answers for) added to `resolve_facts_at_with_cwd`, the test fails at "a host
/// fact no bell answers for" with `left: ["upgrade_ask"] right: []`.
#[test]
fn every_host_fact_a_derive_reads_is_belled() {
    let _ = take_noted();
    let mut rig = CauseRig::new();
    let _ = rig.ask(Duration::ZERO);
    let _ = rig.tell(Command::HostMoved(Bell::Config), Duration::from_secs(10));
    drop(rig);
    let mut rig = CauseRig::with(ASKS, lay_listed);
    rig.resolver.machine.outlook.reads = Some(OUTLOOK_READS);
    let _ = rig.ask(Duration::ZERO);
    let _ = rig.tell(Command::HostMoved(Bell::Journal), Duration::from_secs(10));
    drop(rig);
    let noted = take_noted();
    for read in [
        "usage_tap_on",
        "offered_for",
        "tap_room_for",
        "accounts_mode",
        "journal",
    ] {
        assert!(
            noted.contains_key(read),
            "CONTROL: the derives read {read}: {noted:?}"
        );
    }
    assert_eq!(
        unbelled(&noted),
        Vec::<&str>::new(),
        "a host fact no bell answers for"
    );
}

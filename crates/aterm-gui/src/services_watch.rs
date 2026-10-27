// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background watch of what macOS's own services hold in memory ([`aterm_vitals::services`]).
//!
//! Once a [`crate::harness_host::DISK_TICK`] the watch reads every resource coalition of the
//! system's accounts alone (no root: the kernel's coalition figure) and the memory strain, and
//! the window hears each change in what the judge tells ([`HostNote::ServiceMemory`]): which
//! services it follows, which it tells, their sizes, a figure moved by a sixteenth, the
//! pressure. The Vitals split reads the same figures only while a Deck is in view; this read is
//! the one that runs while no one looks.
//!
//! One aterm process per Mac and account watches: the installed app's, holding the state root's
//! [`LOCK`]. A lab instance, the dev app and a build run from `target/` never watch, so their
//! windows and the checks that photograph them never raise the alert.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_vitals::services::{Judge, Reading, Seen, Size, Told, followed_from, restart_of};

use crate::harness_host::HostNote;

/// The file under the state root whose lock the one watching process holds.
pub(crate) const LOCK: &str = "services-watch.lock";

/// How long after a press of a service's restart another press opens nothing: the first tab's
/// `sudo` may still be asking, or the old process still ending.
pub(crate) const PRESS_QUIET: Duration = Duration::from_mins(5);

/// What the watch knows: the judge, what the window last heard, and the account.
#[derive(Debug, Default)]
pub(crate) struct Board {
    judge: Judge,
    /// What the window last heard ([`signature`]); `None` before the first reading.
    told: Option<Vec<String>>,
    /// Whether this account administers the Mac ([`aterm_uds::account::administrator`]).
    admin: Option<bool>,
}

impl Board {
    /// A board nothing has read, for an account that does (`admin`) or does not administer
    /// the Mac.
    pub(crate) fn new(admin: Option<bool>) -> Self {
        Self {
            admin,
            ..Self::default()
        }
    }
}

/// Process-wide services board.
static BOARD: OnceLock<Mutex<Board>> = OnceLock::new();

fn board() -> &'static Mutex<Board> {
    BOARD.get_or_init(|| {
        #[cfg(unix)]
        let admin = aterm_uds::account::administrator();
        #[cfg(not(unix))]
        let admin = None;
        Mutex::new(Board::new(admin))
    })
}

/// When this process last opened a restart's tab ([`crate::App::restart_service`]).
static PRESSED: Mutex<Option<Instant>> = Mutex::new(None);

/// Why the last press of a restart opened nothing, for the press's feedback ([`take_refusal`]).
static REFUSED: Mutex<Option<String>> = Mutex::new(None);

/// The episodes whose alert the person filed, as the window last judged them ([`set_filed`]).
static FILED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Milliseconds elapsed since the watcher's first reading.
fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A footprint to a sixteenth: its power of two and the next four bits, so a service that moves
/// by less tells nothing new.
fn sixteenth(footprint: u64) -> (u32, u64) {
    let exp = 63_u32.saturating_sub(footprint.leading_zeros());
    (exp, (footprint >> exp.saturating_sub(4)) & 0xf)
}

/// What the window hears of `told`: each service followed (its process, whether told, its size,
/// its restart, its figure to a sixteenth), each of those whose footprint this reading did not
/// read, and the pressure. An unread process the judge does not follow (one that ended between
/// the table and its coalition) is no news.
fn signature(told: &Told) -> Vec<String> {
    let followed: Vec<String> = told.services.iter().map(|s| s.service.episode()).collect();
    told.services
        .iter()
        .map(|s: &Seen| {
            format!(
                "{}:{}:{}:{}:{:?}",
                s.service.episode(),
                s.told(),
                s.size.word(),
                s.restart.map_or("-", |r| r.name),
                sixteenth(s.service.footprint)
            )
        })
        .chain(
            told.unread
                .iter()
                .map(aterm_vitals::services::Member::episode)
                .filter(|e| followed.contains(e))
                .map(|e| format!("{e}:unread")),
        )
        .chain([format!("pressed:{}", told.pressed)])
        .collect()
}

/// Folds `reading` at `at_ms` into `board` and returns the note owed to the window: the whole
/// answer at the first reading and at every change of what it tells; nothing for a reading that
/// failed.
pub(crate) fn step(board: &mut Board, at_ms: u64, reading: Option<&Reading>) -> Option<HostNote> {
    let reading = reading?;
    let was: Vec<String> = board.judge.over().map(|s| s.service.episode()).collect();
    let told = board.judge.absorb(at_ms, Some(reading))?.clone();
    let words = signature(&told);
    if board.told.as_ref() == Some(&words) {
        return None;
    }
    board.told = Some(words);
    for s in told.told().filter(|s| !was.contains(&s.service.episode())) {
        aterm_log::warn!(
            "services: macOS's {} (pid {}) holds {} bytes, past the {}-byte bar since {} s ago \
             ({}; memory {})",
            aterm_vitals::sys::exe_word(&s.service.name),
            s.service.pid,
            s.service.footprint,
            aterm_vitals::services::large_bar(told.total),
            at_ms.saturating_sub(s.since_ms.unwrap_or(at_ms)) / 1000,
            s.size.word(),
            if told.pressed {
                "under pressure"
            } else {
                "not under pressure"
            }
        );
    }
    let now: Vec<String> = told.told().map(|s| s.service.episode()).collect();
    for gone in was.iter().filter(|e| !now.contains(e)) {
        aterm_log::info!("services: the process {gone} no longer holds past its bar");
    }
    Some(HostNote::ServiceMemory {
        told,
        admin: board.admin,
    })
}

/// The `service_mem=*` fields for `status`: `-` before a reading (and in a process that does
/// not watch), else the largest told size (`ok`, `large`, `huge`), and the largest told service
/// with its footprint in MiB.
pub(crate) fn status_fields() -> String {
    let b = board().lock().unwrap_or_else(PoisonError::into_inner);
    let top = b.judge.over().next().map_or_else(
        || "-".to_owned(),
        |o| {
            format!(
                "{}:{}",
                aterm_vitals::sys::exe_word(&o.service.name),
                o.service.footprint >> 20
            )
        },
    );
    format!("service_mem={} service_mem_top={top}", b.judge.word())
}

/// An earlier aterm process told each of `episodes` (the window holds its entry or its alert,
/// at the size given): the watch's judge holds it from its last reading on
/// ([`Judge::carry`]), so `status` and a press read it as the window shows it, and the
/// window, which judged the same reading so, hears nothing new of it.
pub(crate) fn carry(episodes: &[(String, Size)]) {
    let mut b = board().lock().unwrap_or_else(PoisonError::into_inner);
    let mut moved = false;
    for (episode, at_least) in episodes {
        moved |= b.judge.carry(episode, *at_least);
    }
    if moved && let Some(told) = b.judge.told() {
        let words = signature(told);
        b.told = Some(words);
    }
}

/// The episodes whose alert the person filed ([`aterm_messages::MessageCenter::filed_standing`]),
/// as the window last judged them: `sys why` offers no restart for them
/// ([`aterm_vitals::model::Snapshot::services_filed`]).
pub(crate) fn set_filed(episodes: Vec<String>) {
    *FILED.lock().unwrap_or_else(PoisonError::into_inner) = episodes;
}

/// [`set_filed`]'s episodes.
pub(crate) fn filed() -> Vec<String> {
    FILED.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Whether the service aterm restarts as `service` ([`aterm_vitals::services::Restart::name`])
/// holds past its bar: as the watch's last reading follows it (told, or not yet told again by
/// a relaunched watch), or, in a process whose watch has read nothing (before its first
/// reading, or one that does not watch), as a read now finds it ([`read_holds`]).
pub(crate) fn holds(service: &str) -> bool {
    let followed = board()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .judge
        .told()
        .map(|told| {
            told.services
                .iter()
                .any(|o| o.size != Size::Usual && o.restart.is_some_and(|r| r.name == service))
        });
    followed.unwrap_or_else(|| read_holds(service, aterm_uds::services::read().as_ref()))
}

/// Whether `reading` finds the service aterm restarts as `service` from the margin under the
/// large bar ([`followed_from`]), where `aterm init --only` restarts it.
pub(crate) fn read_holds(service: &str, reading: Option<&Reading>) -> bool {
    reading.is_some_and(|r| {
        let from = followed_from(r.total);
        r.services
            .iter()
            .any(|s| s.footprint >= from && restart_of(s).is_some_and(|x| x.name == service))
    })
}

/// Records why a press opened nothing, for its feedback, and returns `false`.
fn refuse(why: impl Into<String>) -> bool {
    let why = why.into();
    aterm_log::info!("services: {why}; nothing opened");
    *REFUSED.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
    false
}

/// Why the last press of a restart opened nothing, as its feedback says it; `None` once read.
pub(crate) fn take_refusal() -> Option<String> {
    REFUSED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .map(|why| format!("Opened nothing: {why}"))
}

/// Why a press of a service's restart at `now` opens nothing, given the last press (`pressed`)
/// and the account (`admin`); `None` when it may open the restart's tab.
pub(crate) fn press_refused(
    pressed: Option<Instant>,
    now: Instant,
    admin: Option<bool>,
) -> Option<&'static str> {
    if admin == Some(false) {
        return Some("this account is not an administrator, so sudo would refuse it");
    }
    pressed
        .is_some_and(|at| now.saturating_duration_since(at) < PRESS_QUIET)
        .then_some(
            "a restart's tab opened within the last five minutes, and its password prompt or the \
             old process's end may still be under way",
        )
}

impl crate::App {
    /// The press of a service's restart ([`aterm_messages::Intent::RestartService`]), checked
    /// again at the press against the restarts aterm knows, the watch's last reading, the
    /// account and the last press: a tab that runs `aterm init --only <item>`, whose `sudo`
    /// asks for the person's password in it ([`aterm_vitals::services::Restart::init`]). The
    /// tab runs a plain `/bin/sh` without rc files, whatever shell or tab command the person
    /// configured, and typing meanwhile stays where it was typed, never reaching the password
    /// prompt. `false` when nothing opened, with why for the press's feedback
    /// ([`take_refusal`]).
    pub(crate) fn restart_service(&mut self, window: crate::WindowId, service: &str) -> bool {
        REFUSED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(restart) = aterm_vitals::services::restart_named(service) else {
            aterm_log::warn!("services: aterm knows no restart named {service:?}");
            return refuse(format!("aterm knows no restart for {service}"));
        };
        if !holds(restart.name) {
            return refuse(format!("{} no longer holds past its bar", restart.name));
        }
        let admin = board().lock().unwrap_or_else(PoisonError::into_inner).admin;
        let now = Instant::now();
        {
            let mut pressed = PRESSED.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(why) = press_refused(*pressed, now, admin) {
                return refuse(why);
            }
            *pressed = Some(now);
        }
        let args = ["init", "--only", restart.init];
        #[cfg(not(windows))]
        let line = crate::ledger_key::own_front_door()
            .and_then(|exe| crate::ai_screen::launch_line(&exe, &args));
        #[cfg(windows)]
        let line: Option<String> = None;
        let Some(line) = line else {
            aterm_log::warn!("services: this program's path has no line a shell runs");
            *PRESSED.lock().unwrap_or_else(PoisonError::into_inner) = None;
            return refuse("this copy of aterm has no path a shell can run");
        };
        aterm_log::info!("services: a tab opens to run `aterm {}`", args.join(" "));
        self.open_off_loop(
            window,
            crate::tab_spawn::SpawnPlace::Tab,
            crate::tab_spawn::SpawnAsk {
                unasked: true,
                exec_command: Some(crate::clt_install::waiting_shell_command()),
                ..crate::tab_spawn::SpawnAsk::default()
            },
            crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| match landed {
                Ok(sid) => app.type_into_new_session(&sid, &line),
                Err(why) => {
                    // Nothing asked for the password: the next press may open the tab again.
                    *PRESSED.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    aterm_log::warn!("services: the restart's tab did not open: {why}");
                }
            })),
        );
        true
    }
}

/// Starts the background watcher thread, in the installed app on macOS only: a lab instance,
/// the dev app and a build from `target/` leave it to the installed app. Of several installed
/// app processes (an update's successor beside its predecessor), the one holding [`LOCK`]
/// watches; another takes the lock once it is free.
pub(crate) fn start(note: Arc<dyn Fn(HostNote) + Send + Sync>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    if aterm_update::running_copy_installable() != Some(true) {
        aterm_log::info!(
            "services: this copy is not the installed app (a lab instance, the dev app or a \
             build), so it leaves macOS's services to the installed app's watch"
        );
        return;
    }
    let Some(lock) = aterm_types::dirs::state_dir().map(|root| root.join(LOCK)) else {
        aterm_log::warn!("services: no state directory to hold the watch's lock in; no watch");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name(format!("{}services", crate::harness_host::THREAD_PREFIX))
        .spawn(move || {
            let mut held: Option<aterm_update_core::FileLock> = None;
            loop {
                if held.is_none() {
                    held = aterm_update_core::FileLock::acquire_within(&lock, Duration::ZERO).ok();
                    if held.is_none() {
                        std::thread::sleep(crate::harness_host::DISK_TICK);
                        continue;
                    }
                    aterm_log::info!("services: this process watches macOS's services");
                }
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let reading = aterm_uds::services::read();
                    step(
                        &mut board().lock().unwrap_or_else(PoisonError::into_inner),
                        now_ms(),
                        reading.as_ref(),
                    )
                }));
                match read {
                    Ok(Some(owed)) => note(owed),
                    Ok(None) => {}
                    Err(_) => aterm_log::warn!(
                        "services: a read stopped after an internal panic; the next is taken at \
                         its time"
                    ),
                }
                std::thread::sleep(crate::harness_host::DISK_TICK);
            }
        });
    if let Err(e) = spawned {
        aterm_log::warn!("services: could not start its watch: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_vitals::services::{FSEVENTSD, HOLD_MS, Service};

    const GIB: u64 = 1 << 30;

    fn reading(footprint: u64) -> Reading {
        Reading {
            total: 128 * GIB,
            services: vec![Service {
                coalition: 7,
                pid: 537,
                start_s: 1_790_000_000,
                uid: 0,
                name: "fseventsd".into(),
                path: Some(FSEVENTSD.path.into()),
                live: 1,
                footprint,
                ..Service::default()
            }],
            ..Reading::default()
        }
    }

    /// `(size, told, restart known)` of each service the note names.
    fn heard(note: Option<HostNote>) -> Option<Vec<(Size, bool, bool)>> {
        match note? {
            HostNote::ServiceMemory { told, .. } => Some(
                told.services
                    .iter()
                    .map(|s| (s.size, s.told(), s.restart.is_some()))
                    .collect(),
            ),
            other => panic!("{other:?}"),
        }
    }

    /// The window hears the first answer, the service followed before it is told, the edge
    /// once the hold is met, a figure moved by a sixteenth, and the end; a failed reading, an
    /// unchanged one and a figure moved by less tell nothing.
    #[test]
    fn the_window_hears_the_first_answer_each_change_and_the_end() {
        let mut b = Board::new(Some(true));
        let small = reading(4 << 20);
        let big = reading(40 * GIB);
        assert_eq!(heard(step(&mut b, 0, Some(&small))), Some(vec![]));
        assert_eq!(heard(step(&mut b, 60_000, Some(&small))), None);
        assert_eq!(heard(step(&mut b, 120_000, None)), None, "a failed reading");
        assert_eq!(
            heard(step(&mut b, 180_000, Some(&big))),
            Some(vec![(Size::Huge, false, true)]),
            "followed, not yet told"
        );
        assert_eq!(heard(step(&mut b, 240_000, Some(&big))), None);
        assert_eq!(
            heard(step(&mut b, 180_000 + HOLD_MS, Some(&big))),
            Some(vec![(Size::Huge, true, true)])
        );
        assert_eq!(
            heard(step(
                &mut b,
                240_000 + HOLD_MS,
                Some(&reading(40 * GIB + (1 << 20)))
            )),
            None,
            "a megabyte more is no news"
        );
        assert_eq!(
            heard(step(&mut b, 300_000 + HOLD_MS, Some(&reading(44 * GIB)))),
            Some(vec![(Size::Huge, true, true)]),
            "a tenth more is"
        );
        assert_eq!(
            heard(step(&mut b, 360_000 + HOLD_MS, Some(&small))),
            Some(vec![]),
            "the end"
        );
    }

    /// A press opens one restart's tab: another within five minutes opens nothing, nor does a
    /// press for an account `sudo` would refuse. NEGATIVE CONTROL: five minutes on, a press
    /// opens again.
    #[test]
    fn a_second_press_within_minutes_opens_nothing() {
        let t0 = Instant::now();
        assert_eq!(press_refused(None, t0, Some(true)), None);
        assert_eq!(press_refused(None, t0, None), None, "unread: sudo decides");
        assert!(press_refused(Some(t0), t0 + Duration::from_secs(60), Some(true)).is_some());
        assert!(press_refused(None, t0, Some(false)).is_some());
        assert_eq!(press_refused(Some(t0), t0 + PRESS_QUIET, Some(true)), None);
    }

    /// With no watch's reading (before the first, or in a process that does not watch), a press
    /// reads the service now and opens the restart's tab from the margin under the large bar,
    /// where `aterm init --only` restarts it. NEGATIVE CONTROL: under it, a service aterm knows
    /// no restart for, and a failed read open nothing.
    #[test]
    fn a_press_with_no_reading_reads_the_service_now() {
        let at = |fp: u64| Some(reading(fp));
        assert!(read_holds("fseventsd", at(141 * GIB).as_ref()));
        assert!(read_holds("fseventsd", at(7 * GIB + (512 << 20)).as_ref()));
        assert!(!read_holds("fseventsd", at(6 * GIB).as_ref()));
        assert!(!read_holds("mds", at(141 * GIB).as_ref()));
        assert!(!read_holds("fseventsd", None));
    }

    /// A refusal names why, once, in the press's words.
    #[test]
    fn a_refused_press_says_why_once() {
        assert!(!refuse("fseventsd no longer holds past its bar"));
        assert_eq!(
            take_refusal().as_deref(),
            Some("Opened nothing: fseventsd no longer holds past its bar")
        );
        assert_eq!(take_refusal(), None);
    }

    #[test]
    fn the_status_fields_say_dash_until_read() {
        let s = status_fields();
        assert!(s.starts_with("service_mem="), "{s}");
        assert!(s.contains(" service_mem_top="), "{s}");
        let b = Board::new(None);
        assert_eq!(b.judge.word(), "-");
    }
}

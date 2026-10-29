// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When this process's Commons runtime (a window's, a terminal session lane's, `aterm node
//! serve`'s) may make its first LAN use: the gate its owner thread asks before it opens its
//! observer or any socket ([`aterm_db::runtime::LanGate`]). On a Mac that first use is what puts
//! macOS's Local Network question on screen while macOS never asked aterm. System State's Local
//! Network step is the one meant to ask it, first in the run's order and never beside another
//! macOS question (`docs/DESIGN-system-state-2026-10-21.md` §6), so Commons waits for that step
//! to hand it the LAN ([`aterm_types::macos_setup::lan_first_use`]). It opens at once where a
//! window of this app on this Mac lived through the question's hold (Commons' own record,
//! [`Host::keeps_its_ask`]). macOS asks each app on each Mac, and aterm's records of its answer and
//! its question are per home, so they may be another Mac's or another app's of aterm (a dev
//! bundle). Where they say macOS answered or asked aterm and no window here lived through the
//! question, where an earlier aterm used the LAN here, or where no step will run for this process,
//! it asks by itself, under this process's dialog slot. macOS asks at the runtime's first LAN use,
//! which may come a while after the gate opened (another process owns the home's observer, or
//! the Commons state errs before its transport), so the slot is held for that use, and the
//! question's 30 s hold, and a window's record that it lived through it, run from the use
//! ([`FirstUse`]). A window's other first LAN uses wait for the step by the rule the gate's wait
//! for it follows ([`hold_first_uses_for_the_step`]).

use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

/// The long-lived host whose Commons runtime asks the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    /// An aterm window's process, where System State's Local Network step runs if the window
    /// `runs_setup`: it runs first-run setup (the run's decision, `first_lan_uses_wait_for_the_step`).
    /// One that runs none (started over ssh or away from the console, under a private state root,
    /// outside the person's scope, or running a command) takes no step.
    Window {
        /// Whether this window runs first-run setup, whose System State runs the step.
        runs_setup: bool,
    },
    /// A terminal session lane (`aterm` at a shell prompt), under the app that holds its terminal.
    SessionLane,
    /// `aterm node serve`, the background job, which yields Commons to any window.
    NodeServe,
}

#[cfg(target_os = "macos")]
impl Host {
    /// Why no System State window can ask macOS for this host's runtime, given whether it runs
    /// `away` from the console owner's graphic session (`aterm_machines::console::remote`: an
    /// ssh login, a session without the window server, another login at the screen, or nobody
    /// at it). `None` in a window that runs first-run setup, whose own System State runs the step,
    /// and at the console, where a window may yet run it. A window that runs no first-run setup
    /// takes no step unless a person presses Set up now in it, so it asks by itself, as an away
    /// host does.
    const fn no_window(self, away: bool) -> Option<&'static str> {
        match self {
            Self::Window { runs_setup: true } => None,
            Self::Window { runs_setup: false } => Some(
                "this window runs no first-run setup, so no System State step asks macOS for it",
            ),
            Self::SessionLane if away => Some(
                "this terminal session runs away from this Mac's console, where no aterm window \
                 runs System State for it",
            ),
            Self::NodeServe if away => Some(
                "nobody runs this account's graphic session at this Mac's console, so no aterm \
                 window runs System State for this job",
            ),
            Self::SessionLane | Self::NodeServe => None,
        }
    }

    /// Whether a Local Network question this host's runtime lived through, and a LAN use it made,
    /// are aterm.app's to keep for later launches ([`keep_asked_here`], [`use_keeper`]), and a
    /// refusal aterm's records hear is the one that fails its sends ([`refusal`]): only a
    /// window's. A session lane's LAN use is attributed to the app that holds its terminal
    /// (Terminal.app, sshd), and `aterm node serve`'s attribution is unverified.
    const fn keeps_its_ask(self) -> bool {
        matches!(self, Self::Window { .. })
    }
}

/// The gate for the Commons runtime of `config` in `host`: on a Mac, one that waits as the
/// module says, holds its question's slot for the runtime's first LAN use ([`FirstUse`]), keeps a
/// window's LAN use for its app's later launches ([`use_keeper`]) and hears, in a window, a
/// refusal aterm's records keep ([`refusal`]); elsewhere, one that never holds (no question
/// guards the LAN there).
pub(crate) fn gate(config: &Path, host: Host) -> aterm_db::runtime::LanGate {
    #[cfg(target_os = "macos")]
    {
        let gate = macos::gate(config.to_owned(), host);
        match refusal(host) {
            Some(refused) => gate.with_refusal(refused),
            None => gate,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (config, host);
        aterm_db::runtime::LanGate::open()
    }
}

/// Hold this window's other first LAN uses (System State's Secrets stage's vault probe and join,
/// Tab Control's Bonjour sweep, the network signal's `_alab._tcp` Bonjour browse) by the rule its
/// Commons runtime's gate for `config` waits on:
/// they wait while System State's Local Network step is still to come and has not asked
/// ([`aterm_types::macos_setup::lan_first_use::may_use`]). It installs the reader of that rule,
/// in a window that runs first-run setup only (`first_lan_uses_wait_for_the_step`), since
/// another runs no step; off a Mac no question guards the LAN, so nothing waits there.
#[cfg(target_os = "macos")]
pub(crate) fn hold_first_uses_for_the_step(config: &Path) {
    macos::install_step_to_come(config.to_owned());
}

/// What reads, for `host`'s runtime, whether macOS refused it the local network: in a window,
/// aterm's records ([`refused`]); none for a terminal session lane, whose sends macOS fails for
/// the app that holds its terminal (Terminal.app's switch, not aterm's), or `aterm node serve`,
/// whose attribution is unverified ([`Host::keeps_its_ask`]). Their failed sends say only that the
/// local network sockets are unavailable.
#[cfg(target_os = "macos")]
fn refusal(host: Host) -> Option<fn() -> bool> {
    host.keeps_its_ask().then_some(refused as fn() -> bool)
}

/// Whether aterm's records hear that macOS refused aterm the local network, which a window's
/// runtime says in its observation while its local network sockets fail
/// ([`aterm_db::runtime::LanGate::with_refusal`]). It never closes the LAN the gate let through:
/// the gate's first use is the one that puts macOS's question to this app on this Mac, and a
/// later use raises none; the records may be stale (only a look at the ALab vault's address or
/// the window's step writes them, and every Mac and app on this home shares them), while a person
/// who turns aterm on in System Settings is heard at the transport's next use.
#[cfg(target_os = "macos")]
fn refused() -> bool {
    aterm_cli::macos_doctor::local_network_heard()
        == aterm_cli::macos_doctor::LocalNetworkHeard::Off
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{FirstUse, Host};
    use aterm_db::runtime::{LanGate, LanTurn};
    use aterm_types::macos_setup::{LOCAL_NETWORK_QUESTION_HOLD, MacosDialog, lan_first_use};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    /// How long a held gate keeps its last look before it reads aterm's records again. The
    /// step's own hand-over is read at every look, so it opens the gate at the owner's next turn.
    const LOOK_AGAIN: Duration = Duration::from_secs(2);

    /// The macOS gate for the Commons runtime of `config` in `host`.
    pub(super) fn gate(config: PathBuf, host: Host) -> LanGate {
        let record = aterm_db::network_state::directory(&config);
        let here = Arc::new(OnceLock::<Option<String>>::new());
        let lived_through = host.keeps_its_ask().then(|| {
            let (record, here) = (record.clone(), here.clone());
            Arc::new(move || {
                if let Some(here) = here.get_or_init(super::here) {
                    keep_asked_here(&record, here);
                }
            }) as Arc<dyn Fn() + Send + Sync>
        });
        let keep = super::use_keeper(record.clone(), host, super::here);
        wired(
            first_use(lived_through),
            || MacosDialog::when_none_up(Duration::ZERO),
            Reads {
                handed: || {
                    let revision = lan_first_use::handed()?;
                    let _ = aterm_log::wait_for_logger(crate::EARLY_LOG_WAIT);
                    aterm_log::info!(
                        "Commons: System State's Local Network step handed over the local network \
                         (revision {revision})"
                    );
                    Some(())
                },
                look: move || {
                    look(
                        &config,
                        &record,
                        here.get_or_init(super::here).as_deref(),
                        host,
                    )
                },
                has_lan,
            },
            move |why| {
                if let Some(why) = why {
                    opened(why);
                }
                keep.as_ref().is_none_or(|keep| keep())
            },
        )
    }

    /// The runtime's first LAN use as the gate holds it: a slot the gate took is held for the use
    /// for as long as the question's hold, at most, and the question's hold runs from the use
    /// ([`FirstUse`]), with `lived_through` run once a window lived through it.
    pub(super) fn first_use<G: Send + 'static>(
        lived_through: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Arc<FirstUse<G>> {
        Arc::new(FirstUse::new(
            LOCAL_NETWORK_QUESTION_HOLD,
            LOCAL_NETWORK_QUESTION_HOLD,
            lived_through,
        ))
    }

    /// What each look of the gate reads ([`wired`]).
    pub(super) struct Reads<H, L, N> {
        /// Whether System State's Local Network step handed the runtime the LAN: its trigger asked
        /// macOS, and the step holds the dialog slot for its own hold.
        pub(super) handed: H,
        /// aterm's records, Commons' own and this process, read at most every [`LOOK_AGAIN`]
        /// while the gate holds.
        pub(super) look: L,
        /// Whether this Mac has an IPv4 LAN address the transport would use.
        pub(super) has_lan: N,
    }

    /// The gate around `first`, the runtime's first LAN use, with `slot` this process's dialog
    /// slot: each look opens where System State's step handed over the LAN (no slot of Commons'
    /// own, and the question's hold runs from the hand-over), and otherwise gives the turn of
    /// what it `reads` ([`super::turn`]), handing a slot that turn took to `first`
    /// ([`FirstUse::take`]); an open gate says why to `first` ([`FirstUse::opened`]). The
    /// runtime's first LAN use asks `first` right before it ([`FirstUse::ready`]); one that must
    /// take the slot again (the gate's went with no use made) reads the look again first and waits
    /// for System State's step where the look now waits for it ([`super::Look::waits_for_step`])
    /// and the step has not handed the LAN over, since the gate's decision may be stale (it opened
    /// on a network the step could not name, and the Mac moved to one it can). A use that reached
    /// the LAN starts the question's hold from it ([`FirstUse::made`]) and runs `keep` with why
    /// the gate opened, the first time; one that reached none lets the slot go
    /// ([`FirstUse::missed`]).
    pub(super) fn wired<G: Send + 'static>(
        first: Arc<FirstUse<G>>,
        slot: impl Fn() -> Option<G> + Send + Sync + 'static,
        reads: Reads<
            impl Fn() -> Option<()> + Send + 'static,
            impl FnMut() -> super::Look + Send + 'static,
            impl Fn() -> bool + Send + 'static,
        >,
        keep: impl Fn(Option<&'static str>) -> bool + Send + 'static,
    ) -> LanGate {
        let Reads {
            handed,
            look,
            has_lan,
        } = reads;
        // Each read is held only for its own call: the use's read of them runs under `first`'s
        // lock, and the gate's look takes that lock after.
        let handed = Arc::new(Mutex::new(handed));
        let look = Arc::new(Mutex::new(look));
        let slot = Arc::new(slot);
        let (turning, looking) = (first.clone(), slot.clone());
        let (readying, using, missing) = (first.clone(), first.clone(), first);
        let (handing, seeing) = (handed.clone(), look.clone());
        let mut last: Option<(Instant, LanTurn)> = None;
        LanGate::asking(move || {
            let handed_now = (handed.lock().unwrap_or_else(PoisonError::into_inner))();
            let turn = if handed_now.is_some() {
                super::hold((), turning.hold, turning.lived_through());
                LanTurn::Open("System State's Local Network step asked macOS")
            } else {
                let now = Instant::now();
                match last {
                    Some((at, held)) if now.saturating_duration_since(at) < LOOK_AGAIN => held,
                    _ => {
                        let seen = (look.lock().unwrap_or_else(PoisonError::into_inner))();
                        let turn = super::turn(seen, &has_lan, &*looking, |slot| {
                            turning.take(slot);
                        });
                        if let LanTurn::Held(_) = turn {
                            last = Some((now, turn));
                        }
                        turn
                    }
                }
            };
            if let LanTurn::Open(why) = turn {
                turning.opened(why);
            }
            turn
        })
        .before_use(move || {
            readying.ready(
                || {
                    let waits = (seeing.lock().unwrap_or_else(PoisonError::into_inner))()
                        .waits_for_step()
                        && (handing.lock().unwrap_or_else(PoisonError::into_inner))().is_none();
                    waits.then_some(super::STEP_FIRST)
                },
                &*slot,
            )
        })
        .keeping_use(move || keep(using.made()))
        .missing_use(move || missing.missed())
    }

    /// What the gate of the Commons runtime of `config` in `host` reads at one look, `record`
    /// being Commons' own directory and `here` this Mac and app ([`super::here`]).
    fn look(config: &Path, record: &Path, here: Option<&str>, host: Host) -> super::Look {
        super::Look {
            first: aterm_cli::macos_doctor::local_network_first_use(
                aterm_gui::local_network_step_off(),
            ),
            asked_here: here.is_some_and(|here| super::asked_here(record, here)),
            used_before: super::used_before(config, record, here),
            no_window: host.no_window(
                !matches!(host, Host::Window { .. }) && aterm_machines::console::remote_here(),
            ),
        }
    }

    /// Install, for this window's process, the reader of whether System State's Local Network
    /// step is still to come ([`lan_first_use::install_step_to_come`]): a look of a window's
    /// Commons gate for `config` that would wait for the step ([`super::Look::waits_for_step`]).
    /// Its other LAN uses wait on that through [`lan_first_use::may_use`].
    pub(super) fn install_step_to_come(config: PathBuf) {
        let record = aterm_db::network_state::directory(&config);
        let here = OnceLock::new();
        lan_first_use::install_step_to_come(move || {
            let here = here.get_or_init(super::here);
            look(
                &config,
                &record,
                here.as_deref(),
                Host::Window { runs_setup: true },
            )
            .waits_for_step()
        });
    }

    /// Say once, in the log, at the runtime's first LAN use, why the gate let it go.
    fn opened(why: &str) {
        let _ = aterm_log::wait_for_logger(crate::EARLY_LOG_WAIT);
        aterm_log::info!("Commons: first local network use: {why}");
    }

    /// Keep, in `record`, that a window of this app on this Mac lived through macOS's question.
    fn keep_asked_here(record: &Path, here: &str) {
        if let Err(error) = super::keep_asked_here(record, here) {
            aterm_log::warn!(
                "Commons: could not keep that macOS asked aterm about Local Network here \
                 ({error}); the next launch asks under the dialog slot again"
            );
        }
    }

    /// Whether this Mac has an IPv4 LAN address the Commons transport would use. It reads the
    /// interfaces only (`getifaddrs`): no socket, so no question.
    fn has_lan() -> bool {
        aterm_net::lan::Listener::candidates()
            .is_ok_and(|addresses| addresses.iter().any(std::net::SocketAddr::is_ipv4))
    }
}

/// What one look of the gate reads, from aterm's records, Commons' own and this process.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct Look {
    /// What aterm's records say of macOS's Local Network question.
    first: aterm_cli::macos_doctor::LocalNetworkFirstUse,
    /// A window of this app on this Mac lived through the question's hold ([`asked_here`]).
    asked_here: bool,
    /// An earlier aterm used the LAN here ([`used_before`]).
    used_before: bool,
    /// Why no System State window can ask macOS for this process ([`Host::no_window`]).
    no_window: Option<&'static str>,
}

#[cfg(target_os = "macos")]
impl Look {
    /// Whether this look waits for System State's Local Network step to ask macOS first (pure):
    /// the step is still to come for this process (a window here runs it), on a Mac where no
    /// earlier aterm used the LAN ([`used_before`]) and no window of this app lived through the
    /// question ([`asked_here`]). [`turn`] holds for the step exactly there.
    const fn waits_for_step(self) -> bool {
        matches!(
            self.first,
            aterm_cli::macos_doctor::LocalNetworkFirstUse::StepToCome
        ) && !self.asked_here
            && self.no_window.is_none()
            && !self.used_before
    }
}

/// The turn one look gives (pure): open where a window of this app on this Mac lived through the
/// question; wait for System State's step while it is still to come for this process on a Mac
/// where no earlier aterm used the LAN; otherwise (aterm's records say macOS answered or asked
/// aterm and no window here lived through the question, an earlier aterm used the LAN here, or no
/// step will run for this process) ask by itself once this Mac has a LAN to ask about and no other
/// macOS question of this process is up, keeping the dialog `slot` it took (`keep`) for the
/// runtime's first LAN use and the question's hold from it ([`FirstUse`]). A kept answer opens no
/// LAN without the slot: it may be another Mac's or another app's, whose question macOS never put
/// to this one.
#[cfg(target_os = "macos")]
fn turn<G>(
    look: Look,
    has_lan: impl FnOnce() -> bool,
    slot: impl FnOnce() -> Option<G>,
    keep: impl FnOnce(G),
) -> aterm_db::runtime::LanTurn {
    use aterm_cli::macos_doctor::LocalNetworkFirstUse;
    use aterm_db::runtime::LanTurn;
    if look.asked_here {
        return LanTurn::Open(
            "macOS asked aterm about Local Network before, and a window here lived through it",
        );
    }
    let why = match look.first {
        LocalNetworkFirstUse::Answered => {
            "aterm's records hear macOS's Local Network answer, and no window of this app on this \
             Mac lived through its question"
        }
        _ if look.waits_for_step() => return LanTurn::Held(STEP_FIRST),
        LocalNetworkFirstUse::Asked => {
            "macOS asked aterm about Local Network before, with no answer heard here since"
        }
        LocalNetworkFirstUse::StepToCome => look
            .no_window
            .unwrap_or("an earlier aterm used the local network on this Mac"),
        LocalNetworkFirstUse::NoStep(why) => why,
    };
    if !has_lan() {
        return LanTurn::Held("No usable local network interface yet");
    }
    match slot() {
        Some(guard) => {
            keep(guard);
            LanTurn::Open(why)
        }
        None => LanTurn::Held(QUESTION_UP),
    }
}

/// What the gate, or the runtime's first LAN use ([`FirstUse::ready`]), waits for while another
/// macOS question of this process is up.
#[cfg(target_os = "macos")]
const QUESTION_UP: &str = "Waits for another macOS question of aterm's to end";

/// What the gate, or a first LAN use that must take the slot again ([`macos::wired`]), waits for
/// while System State's Local Network step is still to come and asks first
/// ([`Look::waits_for_step`]).
#[cfg(target_os = "macos")]
const STEP_FIRST: &str = "Waits for System State to ask macOS for Local Network access";

/// Hold this process's dialog `slot` for `bound` from a thread of its own, so the owner goes on:
/// macOS says neither when the Local Network alert closes nor how it was answered, so the next
/// macOS question of this process waits the bound out. A process that lives through the bound
/// runs `lived_through` (a window keeps its record of the question, [`keep_asked_here`]), and
/// only then lets the slot go.
#[cfg(target_os = "macos")]
fn hold<G: Send + 'static, F: FnOnce() + Send + 'static>(
    slot: G,
    bound: std::time::Duration,
    lived_through: Option<F>,
) {
    #[cfg(test)]
    HELD_FOR.set(Some(bound));
    let spawned = std::thread::Builder::new()
        .name("commons-lan-question-hold".into())
        .spawn(move || {
            std::thread::sleep(bound);
            if let Some(lived_through) = lived_through {
                lived_through();
            }
            drop(slot);
        });
    if let Err(error) = spawned {
        aterm_log::warn!(
            "Commons: the Local Network question's hold could not start ({error}); the next \
             macOS question may come beside it"
        );
    }
}

/// The runtime's first LAN use, which puts macOS's Local Network question, between the gate that
/// opened for it and that use. The runtime makes it right after the gate opens unless another
/// process owns the home's observer or its Commons state errs before its transport, and then it
/// may come much later, or never. So the dialog slot the gate took (`G`) is held for the use, and
/// the question's hold runs from the use: the slot is held for `hold` after it and then let go,
/// and a window's record that it lived through the question (`lived_through`, [`keep_asked_here`])
/// is kept only then. One rule holds the slot until then: a slot held for the use goes `wait`
/// after it was taken or last readied for a use ([`Self::ready`]), and at once when the use
/// reached no LAN socket ([`Self::missed`]), so the process's other macOS questions never wait on
/// a use that did not come, and a use that goes is never let go under (it binds within seconds of
/// its ready). A use that reached the LAN (a socket bound, or failed only once it was made) is
/// the use, and its hold runs. The use takes a slot that went again.
#[cfg(target_os = "macos")]
struct FirstUse<G> {
    state: std::sync::Mutex<Asking<G>>,
    changed: std::sync::Condvar,
    wait: std::time::Duration,
    hold: std::time::Duration,
    lived_through: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

/// Where a [`FirstUse`] is.
#[cfg(target_os = "macos")]
struct Asking<G> {
    slot: UseSlot<G>,
    /// Why the gate opened, said in the log at the use.
    why: Option<&'static str>,
    /// How many times a slot was held for the use: each hold's own wait lets go only that hold
    /// ([`FirstUse::ran_out`]).
    armed: u64,
}

/// The dialog slot a [`FirstUse`] holds for the use.
#[cfg(target_os = "macos")]
enum UseSlot<G> {
    /// None of Commons' own: the gate is still closed, or opened with none (a window here lived
    /// through the question, or System State's step asked it and holds its own).
    Unheld,
    /// Taken by the gate, or again by the use, and held for the use until the wait `armed` (the
    /// latest, from the take or the use's last ready) ran out.
    Held { slot: G, armed: u64 },
    /// Let go with no use made (its wait ran out, the use reached no LAN socket, or the gate's wait
    /// could not start): the use takes it again.
    LetGo,
    /// The use was made.
    Used,
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// No thread of its own starts on this thread while set (tests of a failed start).
    static NO_THREADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The bound of the latest [`hold`] this thread started (tests, which cannot wait out a
    /// question's 30 s).
    static HELD_FOR: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(target_os = "macos")]
impl<G: Send + 'static> FirstUse<G> {
    fn new(
        wait: std::time::Duration,
        hold: std::time::Duration,
        lived_through: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            state: std::sync::Mutex::new(Asking {
                slot: UseSlot::Unheld,
                why: None,
                armed: 0,
            }),
            changed: std::sync::Condvar::new(),
            wait,
            hold,
            lived_through,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Asking<G>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What a hold runs once it lived through the question, as [`hold`] takes it.
    fn lived_through(&self) -> Option<impl FnOnce() + Send + 'static> {
        self.lived_through.clone().map(|kept| move || kept())
    }

    /// The gate opened, for `why`.
    fn opened(&self, why: &'static str) {
        self.lock().why = Some(why);
    }

    /// The gate took this process's dialog `slot`: hold it for the use. Where its wait cannot
    /// start, it goes at once, since nothing would let it go before a use that may not come; the
    /// use takes it again ([`Self::ready`]).
    fn take(self: &std::sync::Arc<Self>, slot: G) {
        let mut state = self.lock();
        if let Err(error) = self.hold_for_use(&mut state, slot) {
            state.slot = UseSlot::LetGo;
            aterm_log::warn!(
                "Commons: the wait for the first local network use could not start ({error}); \
                 the dialog slot goes, and the use takes it again"
            );
        }
    }

    /// Hold `slot` for the use, from now: a thread of its own lets it go once `wait` passed with
    /// the slot still held from now ([`Self::ran_out`]). An earlier hold's wait ends unheard.
    fn hold_for_use(
        self: &std::sync::Arc<Self>,
        state: &mut Asking<G>,
        slot: G,
    ) -> std::io::Result<()> {
        state.armed = state.armed.wrapping_add(1);
        let armed = state.armed;
        state.slot = UseSlot::Held { slot, armed };
        self.changed.notify_all();
        #[cfg(test)]
        if NO_THREADS.get() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let first = self.clone();
        std::thread::Builder::new()
            .name("commons-lan-first-use".into())
            .spawn(move || {
                let state = first.lock();
                let (state, _) = first
                    .changed
                    .wait_timeout_while(state, first.wait, |state| {
                        matches!(state.slot, UseSlot::Held { armed: held, .. } if held == armed)
                    })
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(state);
                first.ran_out(armed);
            })
            .map(drop)
    }

    /// The wait of the hold `armed` ran out: let the slot go if that hold still holds it. A
    /// later hold (the use readied it again) is not this wait's.
    fn ran_out(&self, armed: u64) {
        let mut state = self.lock();
        if matches!(state.slot, UseSlot::Held { armed: held, .. } if held == armed) {
            state.slot = UseSlot::LetGo;
        }
    }

    /// Whether the runtime's first LAN use may go now (`None`) or what it waits for
    /// ([`aterm_db::runtime::LanGate::before_use`]). A held slot is held again from now, so its
    /// wait cannot run out under the use; a slot the gate let go is taken again, from `slot`, and
    /// held for the use, unless `waits` says what the use waits for first (the gate's decision is
    /// stale by then: System State's Local Network step may have come to be due since, as on a
    /// network it can name now), and with another macOS question of this process up, the use
    /// waits. With none of Commons' own, or the use made, it goes, and `waits` is not read. Where
    /// the wait cannot start the slot stays held: the use follows at once, and [`Self::made`] or
    /// [`Self::missed`] ends it.
    fn ready(
        self: &std::sync::Arc<Self>,
        waits: impl FnOnce() -> Option<&'static str>,
        slot: impl FnOnce() -> Option<G>,
    ) -> Option<&'static str> {
        let mut state = self.lock();
        let held = match std::mem::replace(&mut state.slot, UseSlot::LetGo) {
            UseSlot::Held { slot, .. } => slot,
            UseSlot::LetGo => {
                if let Some(why) = waits() {
                    return Some(why);
                }
                match slot() {
                    Some(slot) => slot,
                    None => return Some(QUESTION_UP),
                }
            }
            other => {
                state.slot = other;
                return None;
            }
        };
        if let Err(error) = self.hold_for_use(&mut state, held) {
            aterm_log::warn!(
                "Commons: the wait for the first local network use could not start ({error}); \
                 the dialog slot is held until the use ends"
            );
        }
        None
    }

    /// The use [`Self::ready`] let go reached no LAN socket, so macOS asked nothing: let the slot go
    /// at once, and the next try takes it again.
    fn missed(&self) {
        let mut state = self.lock();
        if matches!(state.slot, UseSlot::Held { .. }) {
            state.slot = UseSlot::LetGo;
        }
        self.changed.notify_all();
    }

    /// The runtime made its first LAN use: hold the slot held for it for `hold` from now, run
    /// `lived_through` once the hold ran out, and then let the slot go ([`hold`]). Why the gate
    /// opened the first time, for the log; `None` after.
    fn made(&self) -> Option<&'static str> {
        let mut state = self.lock();
        match std::mem::replace(&mut state.slot, UseSlot::Used) {
            UseSlot::Used => return None,
            UseSlot::Held { slot, .. } => hold(slot, self.hold, self.lived_through()),
            UseSlot::Unheld | UseSlot::LetGo => {}
        }
        self.changed.notify_all();
        state.why
    }
}

/// The record, in the Commons runtime's own directory (`aterm_db::network_state::directory`),
/// of the Macs and apps where a window lived through macOS's Local Network question: one line
/// each, [`here`]'s. It names the Mac and the app because a home folder can be shared by Macs
/// (a network home, a migrated one) and a Mac can run several apps of aterm (a dev bundle), and
/// macOS asks each app on each Mac.
#[cfg(target_os = "macos")]
const ASKED_HERE: &str = "local-network-asked";

/// The record, beside [`ASKED_HERE`] and named as it is, of the Macs and apps where a window's
/// Commons runtime used the LAN ([`use_keeper`]).
#[cfg(target_os = "macos")]
const USED_HERE: &str = "local-network-used";

/// How many Macs and apps each of [`ASKED_HERE`] and [`USED_HERE`] keeps: the latest.
#[cfg(target_os = "macos")]
const HERE_KEPT: usize = 16;

/// The most each of [`ASKED_HERE`] and [`USED_HERE`] is read for.
#[cfg(target_os = "macos")]
const HERE_MAX: usize = 64 * 1024;

/// This Mac and this app, as [`ASKED_HERE`] names them: the Mac's hardware UUID and the path of
/// the app this executable runs in (the executable's own path outside an app). `None` when the
/// hardware UUID or the executable cannot be read, or the path holds a line break.
#[cfg(target_os = "macos")]
fn here() -> Option<String> {
    let host = aterm_containment::consent::host_uuid()?;
    let exe = std::env::current_exe().ok()?;
    let app = aterm_containment::consent::app_bundle_root(&exe).unwrap_or(exe);
    let here = format!("{host} {}", app.display());
    (!here.contains(['\n', '\r'])).then_some(here)
}

/// The lines `file` ([`ASKED_HERE`] or [`USED_HERE`]) in `record` keeps; none when it is missing
/// or cannot be read.
#[cfg(target_os = "macos")]
fn here_lines(record: &Path, file: &str) -> Vec<String> {
    aterm_uds::private_file::read_private(record, file, HERE_MAX)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a window of the app `here` names, on the Mac it names, lived through macOS's Local
/// Network question ([`ASKED_HERE`] in `record`).
#[cfg(target_os = "macos")]
fn asked_here(record: &Path, here: &str) -> bool {
    here_lines(record, ASKED_HERE)
        .iter()
        .any(|line| line == here)
}

/// Whether a window of the app `here` names, on the Mac it names, used the LAN ([`USED_HERE`] in
/// `record`).
#[cfg(target_os = "macos")]
fn used_here(record: &Path, here: &str) -> bool {
    here_lines(record, USED_HERE)
        .iter()
        .any(|line| line == here)
}

/// Whether an earlier aterm used the LAN here, as a look reads it: an aterm with no gate (0.145.0)
/// left its observation history with `config`'s home ([`aterm_db::network_state::lan_used`]), or
/// a window of the app `here` names, on its Mac, used it ([`USED_HERE`] in `record`). Another
/// host's use counts for nothing: a terminal session lane's or `aterm node serve`'s keeps no line
/// ([`use_keeper`]), and another Mac's or another app's line is not this one's.
#[cfg(target_os = "macos")]
fn used_before(config: &Path, record: &Path, here: Option<&str>) -> bool {
    aterm_db::network_state::lan_used(config) || here.is_some_and(|here| used_here(record, here))
}

/// Keep, in [`ASKED_HERE`] in `record`, that a window of the app `here` names lived through
/// macOS's question on its Mac ([`keep_here`]).
///
/// # Errors
///
/// `record` cannot be made a private directory, or the record cannot be written.
#[cfg(target_os = "macos")]
fn keep_asked_here(record: &Path, here: &str) -> std::io::Result<()> {
    keep_here(record, ASKED_HERE, here)
}

/// Keep `here` in `file` ([`ASKED_HERE`] or [`USED_HERE`]) in `record`, last, with the latest
/// [`HERE_KEPT`] lines.
///
/// # Errors
///
/// `record` cannot be made a private directory, or the record cannot be written.
#[cfg(target_os = "macos")]
fn keep_here(record: &Path, file: &str, here: &str) -> std::io::Result<()> {
    let mut lines: Vec<String> = here_lines(record, file)
        .into_iter()
        .filter(|line| line != here)
        .collect();
    lines.push(here.to_owned());
    let kept = &lines[lines.len().saturating_sub(HERE_KEPT)..];
    aterm_uds::private_file::create_private_dir(record)?;
    aterm_uds::private_file::write_private(record, file, (kept.join("\n") + "\n").as_bytes())
}

/// What keeps `host`'s LAN use for its app's later launches on this Mac
/// ([`aterm_db::runtime::LanGate::keeping_use`]): a window's, in [`USED_HERE`] in `record` under
/// `here`'s name, which a later look reads as an earlier use ([`used_before`]). None for a
/// terminal session lane or `aterm node serve` ([`Host::keeps_its_ask`]): macOS puts a session
/// lane's question to the app that holds its terminal, and whom it puts `aterm node serve`'s to is
/// unverified, so neither use is aterm.app's to read as its own. With no name for here (`here`
/// says `None`) nothing can be kept, and it says it is done.
#[cfg(target_os = "macos")]
fn use_keeper(
    record: PathBuf,
    host: Host,
    here: fn() -> Option<String>,
) -> Option<impl Fn() -> bool + Send + 'static> {
    host.keeps_its_ask().then(move || {
        let named = std::sync::OnceLock::new();
        move || {
            named
                .get_or_init(here)
                .as_deref()
                .is_none_or(|here| keep_here(&record, USED_HERE, here).is_ok())
        }
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{Host, Look};
    use aterm_cli::macos_doctor::LocalNetworkFirstUse;
    use aterm_db::runtime::LanTurn;
    use std::cell::Cell;
    use std::time::Duration;

    /// The dialog slot a look may take, as a token the test counts.
    struct Slot;

    /// A window that runs first-run setup, whose System State runs the step.
    const WINDOW: Host = Host::Window { runs_setup: true };

    const OFF: &str = "a [privacy] switch is off";
    const STEP: LanTurn =
        LanTurn::Held("Waits for System State to ask macOS for Local Network access");
    const BUSY: LanTurn = LanTurn::Held("Waits for another macOS question of aterm's to end");
    const NO_LAN: LanTurn = LanTurn::Held("No usable local network interface yet");
    const ASKED: &str =
        "macOS asked aterm about Local Network before, with no answer heard here since";
    const ANSWERED: &str = "aterm's records hear macOS's Local Network answer, and no window of \
                            this app on this Mac lived through its question";
    const LIVED: LanTurn = LanTurn::Open(
        "macOS asked aterm about Local Network before, and a window here lived through it",
    );

    fn looking(first: LocalNetworkFirstUse, used_before: bool) -> Look {
        Look {
            first,
            asked_here: false,
            used_before,
            no_window: None,
        }
    }

    /// One look with `has_lan` and the slot `free` or busy: its turn, how many slots it asked
    /// for, and how many guards it kept.
    fn look(reads: Look, has_lan: bool, free: bool) -> (LanTurn, usize, usize) {
        let (asked, kept) = (Cell::new(0), Cell::new(0));
        let turn = super::turn(
            reads,
            || has_lan,
            || {
                asked.set(asked.get() + 1);
                free.then_some(Slot)
            },
            |Slot| kept.set(kept.get() + 1),
        );
        (turn, asked.get(), kept.get())
    }

    /// The Commons runtime's first LAN use comes after macOS's Local Network question, never
    /// before it: open where a window here lived through the question (no slot of its own); wait
    /// for System State's step while it is to come on a Mac where no earlier aterm used the LAN;
    /// else ask by itself, only with a LAN to ask about and the process's dialog slot free,
    /// keeping it. A kept answer, or a kept "asked" with no answer heard since, and no window here
    /// that lived through the question (an alert left unanswered at a quit, another Mac's or
    /// another app's record on a shared home, which macOS never put to this app here) asks under
    /// the slot too.
    #[test]
    fn the_first_lan_use_turn() {
        for used_before in [false, true] {
            let answered = looking(LocalNetworkFirstUse::Answered, used_before);
            assert_eq!(
                look(answered, true, true),
                (LanTurn::Open(ANSWERED), 1, 1),
                "a kept answer no window here lived through: under the slot"
            );
            assert_eq!(
                look(answered, true, false),
                (BUSY, 1, 0),
                "another app's answer opens nothing beside another macOS question"
            );
            assert_eq!(look(answered, false, true), (NO_LAN, 0, 0));
            for first in [
                LocalNetworkFirstUse::Answered,
                LocalNetworkFirstUse::Asked,
                LocalNetworkFirstUse::StepToCome,
                LocalNetworkFirstUse::NoStep(OFF),
            ] {
                let reads = Look {
                    asked_here: true,
                    ..looking(first, used_before)
                };
                assert_eq!(
                    look(reads, true, true),
                    (LIVED, 0, 0),
                    "{first:?}: a window here lived through the question, no slot of its own"
                );
            }
            let asked = looking(LocalNetworkFirstUse::Asked, used_before);
            assert_eq!(
                look(asked, true, true),
                (LanTurn::Open(ASKED), 1, 1),
                "asked, with no answer and no window here that lived through it: under the slot"
            );
            assert_eq!(look(asked, true, false), (BUSY, 1, 0));
            assert_eq!(look(asked, false, true), (NO_LAN, 0, 0));
        }
        assert_eq!(
            look(looking(LocalNetworkFirstUse::StepToCome, false), true, true),
            (STEP, 0, 0),
            "the step is to come and asks first"
        );
        assert_eq!(
            look(looking(LocalNetworkFirstUse::StepToCome, true), true, true),
            (
                LanTurn::Open("an earlier aterm used the local network on this Mac"),
                1,
                1
            )
        );
        for used_before in [false, true] {
            let no_step = looking(LocalNetworkFirstUse::NoStep(OFF), used_before);
            assert_eq!(
                look(no_step, true, true),
                (LanTurn::Open(OFF), 1, 1),
                "no step: it asks by itself under the slot"
            );
            assert_eq!(
                look(no_step, true, false),
                (BUSY, 1, 0),
                "never beside another macOS question"
            );
            assert_eq!(
                look(no_step, false, true),
                (NO_LAN, 0, 0),
                "nothing to ask about yet"
            );
        }
        let legacy = looking(LocalNetworkFirstUse::StepToCome, true);
        assert_eq!(look(legacy, true, false), (BUSY, 1, 0));
        assert_eq!(look(legacy, false, true), (NO_LAN, 0, 0));
    }

    /// A window's other first LAN uses (the Secrets stage's vault probe and join, Tab Control's
    /// sweep) wait for System State's Local Network step exactly where the window's Commons gate
    /// holds for it: the step still to come, on a Mac where no earlier aterm used the LAN, with no
    /// window here that lived through the question and a window to run the step. A recorded
    /// answer or ask, a `[privacy]` or `[swarm]` switch off, Permissions out or a network the step
    /// cannot name, an earlier use here and a process no window serves each let them go.
    #[test]
    fn a_windows_other_lan_uses_wait_where_its_gate_holds_for_the_step() {
        const AWAY: &str = "this terminal session runs away from this Mac's console";
        let fresh = looking(LocalNetworkFirstUse::StepToCome, false);
        assert!(
            fresh.waits_for_step(),
            "the step is still to come and asks first"
        );
        for (reads, why) in [
            (looking(LocalNetworkFirstUse::Asked, false), "macOS asked"),
            (
                looking(LocalNetworkFirstUse::Answered, false),
                "macOS answered",
            ),
            (
                looking(LocalNetworkFirstUse::NoStep(OFF), false),
                "no step runs",
            ),
            (
                looking(LocalNetworkFirstUse::StepToCome, true),
                "an upgraded Mac",
            ),
            (
                Look {
                    asked_here: true,
                    ..fresh
                },
                "a window here lived through it",
            ),
            (
                Look {
                    no_window: Some(AWAY),
                    ..fresh
                },
                "no window runs the step",
            ),
        ] {
            assert!(!reads.waits_for_step(), "{why}: the use goes on");
        }
        for first in [
            LocalNetworkFirstUse::Answered,
            LocalNetworkFirstUse::Asked,
            LocalNetworkFirstUse::StepToCome,
            LocalNetworkFirstUse::NoStep(OFF),
        ] {
            for (used_before, asked_here, no_window) in [false, true]
                .into_iter()
                .flat_map(|a| [false, true].map(|b| (a, b)))
                .flat_map(|(a, b)| [None, Some(AWAY)].map(|c| (a, b, c)))
            {
                let reads = Look {
                    asked_here,
                    no_window,
                    ..looking(first, used_before)
                };
                assert_eq!(
                    reads.waits_for_step(),
                    look(reads, true, true).0 == STEP,
                    "{reads:?}: the uses wait where the gate holds for the step"
                );
            }
        }
    }

    /// A terminal session lane or `aterm node serve` away from this Mac's console (an ssh login,
    /// no window server, nobody or another login at the screen) has no System State window to
    /// run the step for it, and a window that runs no first-run setup (started over ssh or away
    /// from the console, under a private state root, outside the person's scope, running a
    /// command) takes no step, so each asks by itself under its own slot where a window's
    /// runtime would wait, and never holds for the step; at the console, or in a window that runs
    /// first-run setup, the step is to come.
    #[test]
    fn a_host_no_window_serves_asks_by_itself() {
        for away in [false, true] {
            assert_eq!(WINDOW.no_window(away), None, "a window runs the step");
        }
        let setup_less = Host::Window { runs_setup: false };
        for (host, away) in [
            (Host::SessionLane, true),
            (Host::NodeServe, true),
            (setup_less, false),
            (setup_less, true),
        ] {
            if host != setup_less {
                assert_eq!(host.no_window(false), None, "{host:?} at the console");
            }
            let why = host
                .no_window(away)
                .expect("no window runs the step for it");
            let reads = Look {
                no_window: Some(why),
                ..looking(LocalNetworkFirstUse::StepToCome, false)
            };
            assert!(!reads.waits_for_step(), "{host:?}: nothing waits on a step");
            assert_eq!(
                look(reads, true, true),
                (LanTurn::Open(why), 1, 1),
                "{host:?}: it asks by itself under its slot, never held for the step"
            );
            assert_eq!(look(reads, true, false), (BUSY, 1, 0));
            assert_eq!(look(reads, false, true), (NO_LAN, 0, 0));
        }
    }

    /// Hold `slot` for no time with `host`'s record of the question in `record`, and return once
    /// the hold let the slot go (a one-minute hang detector).
    fn hold_through(record: &std::path::Path, host: Host, here: &str) {
        struct Let(std::sync::mpsc::Sender<()>);
        impl Drop for Let {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (sent, gone) = std::sync::mpsc::channel();
        let (record, here) = (record.to_owned(), here.to_owned());
        super::hold(
            Let(sent),
            Duration::ZERO,
            host.keeps_its_ask()
                .then_some(move || super::keep_asked_here(&record, &here).unwrap()),
        );
        gone.recv_timeout(Duration::from_secs(60)).unwrap();
    }

    /// A window that asks by itself (a `[privacy]` switch or `[swarm]` off, Permissions left out,
    /// an earlier aterm's LAN use) and lives through the question's hold keeps a record of it,
    /// for this Mac and this app, so later launches open with no slot instead of holding the
    /// window's dialog slot for 30 s on every launch. A terminal session lane or `aterm node
    /// serve` keeps none (macOS asked another app, or nobody knows whom), and another Mac's or
    /// another app's line on a shared home is not this one's.
    #[test]
    fn a_window_that_lived_through_its_ask_opens_later_launches_with_no_slot() {
        const HERE: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm.app";
        for host in [WINDOW, Host::SessionLane, Host::NodeServe] {
            let root = aterm_tempfile::tempdir().unwrap();
            let record = root.path().join("network-state");
            let launch = |first| Look {
                asked_here: super::asked_here(&record, HERE),
                ..looking(first, false)
            };
            assert_eq!(
                look(launch(LocalNetworkFirstUse::NoStep(OFF)), true, true),
                (LanTurn::Open(OFF), 1, 1),
                "{host:?}: the first launch asks under the slot"
            );
            hold_through(&record, host, HERE);
            let later = look(launch(LocalNetworkFirstUse::NoStep(OFF)), true, true);
            if host == WINDOW {
                assert_eq!(later, (LIVED, 0, 0), "a later launch opens with no slot");
            } else {
                assert_eq!(
                    later,
                    (LanTurn::Open(OFF), 1, 1),
                    "{host:?} keeps no record"
                );
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        for other in [
            "0E2C5A41-8D7B-4F0E-9A63-2B5D7C1E4F90 /Applications/aterm.app",
            "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm (dev).app",
        ] {
            hold_through(&record, WINDOW, other);
            assert!(
                !super::asked_here(&record, HERE),
                "{other}: not this Mac's app"
            );
        }
        for n in 0..super::HERE_KEPT + 4 {
            hold_through(&record, WINDOW, &format!("{n} /Applications/aterm.app"));
        }
        hold_through(&record, WINDOW, HERE);
        hold_through(&record, WINDOW, HERE);
        let lines = super::here_lines(&record, super::ASKED_HERE);
        assert_eq!(lines.len(), super::HERE_KEPT, "bounded, no duplicate");
        assert_eq!(
            lines.last().map(String::as_str),
            Some(HERE),
            "the latest last"
        );
    }

    /// A token for a dialog slot, which says on `gone` when it is let go.
    struct Token(std::sync::mpsc::Sender<()>);
    impl Drop for Token {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    fn token() -> (Token, std::sync::mpsc::Receiver<()>) {
        let (sent, gone) = std::sync::mpsc::channel();
        (Token(sent), gone)
    }

    /// A use's `waits` read ([`super::FirstUse::ready`]) that says it waits for nothing.
    const fn no_wait() -> Option<&'static str> {
        None
    }

    /// macOS asks its Local Network question at the runtime's first LAN use, not when the gate
    /// opens, and an open owner may make none for a while, or ever (another process owns the
    /// home's observer, or its Commons state errs before its transport). So the slot the gate took
    /// is held for that use, and the question's hold, and a window's record that it lived through
    /// the question, run from the use: a window that never used the LAN keeps no line. A slot held
    /// with no use within its wait goes, and the use takes it again, waiting while another macOS
    /// question of the process is up.
    #[test]
    fn the_questions_hold_runs_from_the_first_lan_use_not_from_the_gate() {
        use super::{FirstUse, QUESTION_UP, UseSlot};
        use std::sync::Arc;
        const HERE: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm (dev).app";
        const WHY: &str = "aterm's records hear macOS's Local Network answer";
        const HANG: Duration = Duration::from_secs(60);
        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        let lived = {
            let record = record.clone();
            Arc::new(move || super::keep_asked_here(&record, HERE).unwrap())
                as Arc<dyn Fn() + Send + Sync>
        };

        let unused = Arc::new(FirstUse::new(
            Duration::ZERO,
            Duration::ZERO,
            Some(lived.clone()),
        ));
        let (slot, gone) = token();
        unused.take(slot);
        unused.opened(WHY);
        gone.recv_timeout(HANG).unwrap();
        assert!(
            !super::asked_here(&record, HERE),
            "the gate opened and no use came: no question lived through"
        );
        assert_eq!(
            unused.ready(no_wait, || None),
            Some(QUESTION_UP),
            "the use takes the slot again, never beside another macOS question"
        );

        let first = Arc::new(FirstUse::new(HANG, Duration::ZERO, Some(lived.clone())));
        let (slot, gone) = token();
        first.take(slot);
        first.opened(WHY);
        assert!(gone.try_recv().is_err(), "held for the use");
        assert_eq!(
            first.ready(no_wait, || panic!("the gate's slot is held")),
            None,
            "the use goes under the gate's slot"
        );
        assert!(!super::asked_here(&record, HERE), "no use yet");
        assert_eq!(first.made(), Some(WHY), "said once, at the use");
        gone.recv_timeout(HANG).unwrap();
        assert!(
            super::asked_here(&record, HERE),
            "the hold from the use ran out"
        );
        assert_eq!(first.made(), None, "once");
        assert_eq!(first.ready(no_wait, || panic!("used")), None);

        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        let later = Arc::new(FirstUse::new(HANG, Duration::ZERO, {
            let record = record.clone();
            Some(
                Arc::new(move || super::keep_asked_here(&record, HERE).unwrap())
                    as Arc<dyn Fn() + Send + Sync>,
            )
        }));
        later.opened(WHY);
        later.lock().slot = UseSlot::LetGo;
        assert_eq!(later.ready(no_wait, || None), Some(QUESTION_UP));
        let (slot, gone) = token();
        assert_eq!(
            later.ready(no_wait, || Some(slot)),
            None,
            "taken again for the use"
        );
        assert!(gone.try_recv().is_err(), "held for the use");
        assert_eq!(later.made(), Some(WHY));
        gone.recv_timeout(HANG).unwrap();
        assert!(super::asked_here(&record, HERE));

        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        let free = Arc::new(FirstUse::<Token>::new(HANG, Duration::ZERO, {
            let record = record.clone();
            Some(
                Arc::new(move || super::keep_asked_here(&record, HERE).unwrap())
                    as Arc<dyn Fn() + Send + Sync>,
            )
        }));
        assert_eq!(
            free.ready(no_wait, || panic!("no slot of Commons' own")),
            None,
            "a gate that opened with no slot (a window here lived through it, or the step asked)"
        );
        free.opened(WHY);
        assert_eq!(free.made(), Some(WHY));
        assert!(
            !super::asked_here(&record, HERE),
            "no slot taken: no hold, and no question lived through"
        );
    }

    /// A slot readied for the use is held through that use and its hold: the use's ready holds it
    /// again from then, so the wait the gate's take started cannot let it go under the use while
    /// macOS's question comes up, and the hold runs from the use. A use that reached no LAN socket
    /// lets it go at once, and the next try takes it again.
    #[test]
    fn a_slot_readied_for_the_use_is_held_through_it() {
        use super::{FirstUse, QUESTION_UP};
        use std::sync::Arc;
        const HERE: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm (dev).app";
        const HANG: Duration = Duration::from_secs(60);
        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        let lived = {
            let record = record.clone();
            Arc::new(move || super::keep_asked_here(&record, HERE).unwrap())
                as Arc<dyn Fn() + Send + Sync>
        };
        let first = Arc::new(FirstUse::new(HANG, Duration::ZERO, Some(lived.clone())));
        let (slot, gone) = token();
        first.take(slot);
        let gates = first.lock().armed;
        assert_eq!(
            first.ready(no_wait, || panic!("the gate's slot is held")),
            None
        );
        first.ran_out(gates);
        assert!(
            gone.try_recv().is_err(),
            "the gate's wait ran out under the use: the slot stays held for it"
        );
        first.made();
        gone.recv_timeout(HANG).unwrap();
        assert!(
            super::asked_here(&record, HERE),
            "the question's hold ran from the use"
        );

        let missed = Arc::new(FirstUse::new(HANG, Duration::ZERO, Some(lived)));
        let (slot, gone) = token();
        missed.take(slot);
        assert_eq!(
            missed.ready(no_wait, || panic!("the gate's slot is held")),
            None
        );
        missed.missed();
        assert!(
            gone.try_recv().is_ok(),
            "no LAN socket bound: it goes at once"
        );
        assert_eq!(
            missed.ready(no_wait, || None),
            Some(QUESTION_UP),
            "the next try takes it again, never beside another macOS question"
        );
        let (slot, gone) = token();
        assert_eq!(missed.ready(no_wait, || Some(slot)), None);
        assert!(gone.try_recv().is_err(), "held for the use");
        missed.made();
        gone.recv_timeout(HANG).unwrap();
    }

    /// A gate whose wait for the use cannot start (no thread for it) lets its slot go at once,
    /// since nothing else would before a use that may not come; the use takes it again and, with
    /// its wait failing too, holds it until the use ends, which follows at once.
    #[test]
    fn a_gate_whose_wait_cannot_start_lets_its_slot_go() {
        use super::{FirstUse, NO_THREADS};
        use std::sync::Arc;
        const HANG: Duration = Duration::from_secs(60);
        let first = Arc::new(FirstUse::new(HANG, Duration::ZERO, None));
        NO_THREADS.set(true);
        let (slot, gone) = token();
        first.take(slot);
        assert!(gone.try_recv().is_ok(), "no wait: the slot goes at once");
        let (slot, gone) = token();
        assert_eq!(
            first.ready(no_wait, || Some(slot)),
            None,
            "taken again for the use"
        );
        assert!(gone.try_recv().is_err(), "held for the use");
        NO_THREADS.set(false);
        first.made();
        gone.recv_timeout(HANG).unwrap();
    }

    /// The macOS gate's wiring ([`super::macos::wired`]), driven as the runtime drives it: a slot
    /// the gate took is held for the first LAN use, the question's hold and a window's record of
    /// it run from the use, not from the gate; a slot no use came for within its wait goes with no
    /// record, and the use takes it again; a use that reached no LAN socket lets it go and keeps
    /// nothing. Built as the macOS gate builds its use ([`super::macos::first_use`]), the slot is
    /// held for the use for the question's hold at most and the question's 30 s hold runs from
    /// the use; where System State's step handed over the LAN, the gate takes no slot and the
    /// hold runs from the hand-over.
    #[test]
    fn the_gate_holds_its_slot_for_the_use_and_the_question_from_it() {
        use super::FirstUse;
        use aterm_types::macos_setup::LOCAL_NETWORK_QUESTION_HOLD;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        const HANG: Duration = Duration::from_secs(60);
        type Lived = Option<Arc<dyn Fn() + Send + Sync>>;
        struct Wired {
            gate: aterm_db::runtime::LanGate,
            first: Arc<FirstUse<Token>>,
            gone: Arc<Mutex<Vec<std::sync::mpsc::Receiver<()>>>>,
            lived: Arc<AtomicUsize>,
            kept: Arc<Mutex<Vec<Option<&'static str>>>>,
        }
        impl Wired {
            fn new(wait: Duration, hold: Duration) -> Self {
                Self::on(|lived| Arc::new(FirstUse::new(wait, hold, lived)), false)
            }
            /// The gate around the use `first` makes, where System State's step `handed` over
            /// the LAN or not.
            fn on(first: impl FnOnce(Lived) -> Arc<FirstUse<Token>>, handed: bool) -> Self {
                let lived = Arc::new(AtomicUsize::new(0));
                let first = first({
                    let lived = lived.clone();
                    Some(Arc::new(move || {
                        lived.fetch_add(1, Ordering::SeqCst);
                    }) as Arc<dyn Fn() + Send + Sync>)
                });
                let gone = Arc::new(Mutex::new(Vec::new()));
                let kept = Arc::new(Mutex::new(Vec::new()));
                let gate = super::macos::wired(
                    first.clone(),
                    {
                        let gone = gone.clone();
                        move || {
                            let (slot, left) = token();
                            gone.lock().unwrap().push(left);
                            Some(slot)
                        }
                    },
                    super::macos::Reads {
                        handed: move || handed.then_some(()),
                        look: || looking(LocalNetworkFirstUse::Answered, false),
                        has_lan: || true,
                    },
                    {
                        let kept = kept.clone();
                        move |why| {
                            kept.lock().unwrap().push(why);
                            true
                        }
                    },
                );
                Self {
                    gate,
                    first,
                    gone,
                    lived,
                    kept,
                }
            }
            fn taken(&self) -> usize {
                self.gone.lock().unwrap().len()
            }
            fn gone(&self, slot: usize) -> bool {
                self.gone.lock().unwrap()[slot].try_recv().is_ok()
            }
            fn let_go(&self, slot: usize) {
                self.gone.lock().unwrap()[slot].recv_timeout(HANG).unwrap();
            }
            fn lived(&self) -> usize {
                self.lived.load(Ordering::SeqCst)
            }
        }

        let mut used = Wired::new(HANG, HANG);
        assert_eq!(used.gate.look_now(), Some(LanTurn::Open(ANSWERED)));
        assert_eq!(used.taken(), 1, "the gate took the slot");
        super::HELD_FOR.take();
        assert_eq!(used.gate.use_now(true), Ok(true));
        assert_eq!(
            super::HELD_FOR.take(),
            Some(HANG),
            "the question's whole hold, from the use"
        );
        assert_eq!(used.taken(), 1, "the use went under the gate's slot");
        assert_eq!(
            *used.kept.lock().unwrap(),
            [Some(ANSWERED)],
            "kept, why said once"
        );
        assert!(!used.gone(0), "held from the use, for the question's hold");
        assert_eq!(used.lived(), 0, "the hold has not run out");

        let mut ran_out = Wired::new(HANG, Duration::ZERO);
        assert_eq!(ran_out.gate.look_now(), Some(LanTurn::Open(ANSWERED)));
        assert_eq!(ran_out.gate.use_now(true), Ok(true));
        ran_out.let_go(0);
        assert_eq!(
            ran_out.lived(),
            1,
            "the question's hold ran from the use, then went"
        );

        let mut unused = Wired::new(Duration::ZERO, HANG);
        assert_eq!(unused.gate.look_now(), Some(LanTurn::Open(ANSWERED)));
        unused.let_go(0);
        assert_eq!(
            unused.lived(),
            0,
            "no use within the wait: gone, no question lived through"
        );
        assert_eq!(unused.gate.use_now(true), Ok(true));
        assert_eq!(unused.taken(), 2, "the use took the slot again");

        let mut missed = Wired::new(HANG, Duration::ZERO);
        assert_eq!(missed.gate.look_now(), Some(LanTurn::Open(ANSWERED)));
        assert_eq!(missed.gate.use_now(false), Ok(false));
        assert!(missed.gone(0), "no LAN socket bound: the slot goes at once");
        assert!(missed.kept.lock().unwrap().is_empty(), "nothing kept");
        assert_eq!(missed.gate.use_now(true), Ok(true));
        assert_eq!(missed.taken(), 2, "the next try took it again");
        missed.let_go(1);
        assert_eq!(missed.lived(), 1);

        // The use as the macOS gate makes it ([`super::macos::first_use`]): a slot held for the
        // use for the question's hold at most, and the question's whole hold from the use.
        let mut macos = Wired::on(super::macos::first_use, false);
        super::HELD_FOR.take();
        assert_eq!(
            (macos.first.wait, macos.first.hold),
            (LOCAL_NETWORK_QUESTION_HOLD, LOCAL_NETWORK_QUESTION_HOLD)
        );
        assert_eq!(macos.gate.look_now(), Some(LanTurn::Open(ANSWERED)));
        assert_eq!(macos.taken(), 1, "the gate took the slot");
        assert_eq!(super::HELD_FOR.take(), None, "no hold at the gate");
        assert_eq!(macos.gate.use_now(true), Ok(true));
        assert_eq!(
            super::HELD_FOR.take(),
            Some(LOCAL_NETWORK_QUESTION_HOLD),
            "the question's 30 s hold, from the use"
        );
        assert_eq!(macos.taken(), 1, "the use went under the gate's slot");
        assert!(!macos.gone(0), "held from the use");
        assert_eq!(macos.lived(), 0);

        // System State's step asked macOS and holds its own slot: the gate opens with none of
        // Commons', and the question's hold runs from the hand-over.
        let mut handed = Wired::on(
            |lived| Arc::new(FirstUse::new(HANG, Duration::ZERO, lived)),
            true,
        );
        assert_eq!(
            handed.gate.look_now(),
            Some(LanTurn::Open(
                "System State's Local Network step asked macOS"
            ))
        );
        assert_eq!(super::HELD_FOR.take(), Some(Duration::ZERO));
        let until = std::time::Instant::now() + HANG;
        while handed.lived() == 0 {
            assert!(
                std::time::Instant::now() < until,
                "the hand-over's hold ran out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(handed.gate.use_now(true), Ok(true));
        assert_eq!(handed.taken(), 0, "no slot of Commons' own");
        assert_eq!(super::HELD_FOR.take(), None, "no second hold at the use");
        assert_eq!(
            *handed.kept.lock().unwrap(),
            [Some("System State's Local Network step asked macOS")]
        );
    }

    /// A first LAN use that must take the dialog slot again (the gate's went with no use made)
    /// reads the look again first: where System State's Local Network step has come to be due
    /// since the gate opened (it opened on a network the step could not name, the try reached no
    /// LAN socket, and the Mac moved to one the step can name), the use waits for the step and
    /// takes no slot; once the step handed the LAN over, or the look waits for nothing, it takes
    /// the slot as before. A slot the gate still holds for the use goes with no such read, so a
    /// use the gate holds a slot for is never deferred.
    #[test]
    fn a_use_that_takes_the_slot_again_waits_for_a_step_now_due() {
        use super::FirstUse;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        const HANG: Duration = Duration::from_secs(60);
        const STEP_WHY: &str = "Waits for System State to ask macOS for Local Network access";
        const NO_LAN_WHY: &str = "this Mac's network has no private or link-local IPv4 subnet";

        let held = Arc::new(FirstUse::new(HANG, Duration::ZERO, None));
        let (slot, _gone) = token();
        held.take(slot);
        assert_eq!(
            held.ready(|| panic!("a held slot reads no wait"), || panic!("held")),
            None,
            "the gate's slot is held for the use: it goes"
        );
        held.missed();
        assert_eq!(
            held.ready(
                || Some(STEP_WHY),
                || panic!("no slot taken while the step is due")
            ),
            Some(STEP_WHY)
        );
        let (slot, gone) = token();
        assert_eq!(held.ready(no_wait, || Some(slot)), None, "taken again");
        assert!(gone.try_recv().is_err(), "held for the use");
        held.made();

        let first = super::macos::first_use::<Token>(None);
        let reads = Arc::new(Mutex::new(looking(
            LocalNetworkFirstUse::NoStep(NO_LAN_WHY),
            false,
        )));
        let handed = Arc::new(AtomicBool::new(false));
        let taken = Arc::new(AtomicUsize::new(0));
        let mut gate = super::macos::wired(
            first,
            {
                let taken = taken.clone();
                move || {
                    taken.fetch_add(1, Ordering::SeqCst);
                    Some(token().0)
                }
            },
            super::macos::Reads {
                handed: {
                    let handed = handed.clone();
                    move || handed.load(Ordering::SeqCst).then_some(())
                },
                look: {
                    let reads = reads.clone();
                    move || *reads.lock().unwrap()
                },
                has_lan: || true,
            },
            |_| true,
        );
        assert_eq!(
            gate.look_now(),
            Some(LanTurn::Open(NO_LAN_WHY)),
            "a network the step cannot name: the gate asks by itself"
        );
        assert_eq!(taken.load(Ordering::SeqCst), 1);
        assert_eq!(
            gate.use_now(false),
            Ok(false),
            "the try reached no LAN socket"
        );
        reads.lock().unwrap().first = LocalNetworkFirstUse::StepToCome;
        assert_eq!(
            gate.use_now(true),
            Err(STEP_WHY),
            "on a network the step can name, the use waits for it"
        );
        assert_eq!(taken.load(Ordering::SeqCst), 1, "no slot taken");
        handed.store(true, Ordering::SeqCst);
        assert_eq!(
            gate.use_now(false),
            Ok(false),
            "the step handed the LAN over"
        );
        assert_eq!(taken.load(Ordering::SeqCst), 2);
        handed.store(false, Ordering::SeqCst);
        reads.lock().unwrap().first = LocalNetworkFirstUse::Answered;
        assert_eq!(gate.use_now(true), Ok(true), "the look waits for nothing");
        assert_eq!(taken.load(Ordering::SeqCst), 3);
    }

    /// Only a window's runtime says that macOS refused aterm the local network when aterm's
    /// records hear it: a terminal session lane's sends fail under the switch of the app that
    /// holds its terminal, not aterm's, and `aterm node serve`'s attribution is unverified, so
    /// their failed sends say only that the local network sockets are unavailable. Every host's
    /// runtime holds its first LAN use for its question ([`super::FirstUse`]).
    #[test]
    fn only_a_window_hears_atermapps_refusal() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        for host in [
            WINDOW,
            Host::Window { runs_setup: false },
            Host::SessionLane,
            Host::NodeServe,
        ] {
            assert_eq!(
                super::refusal(host).is_some(),
                matches!(host, Host::Window { .. }),
                "{host:?}"
            );
            let gate = format!("{:?}", super::gate(&config, host));
            assert_eq!(
                gate.contains("hears_refusal: true"),
                matches!(host, Host::Window { .. }),
                "{host:?}: {gate}"
            );
            assert!(gate.contains("readies_use: true"), "{host:?}: {gate}");
            assert!(gate.contains("keeps_use: true"), "{host:?}: {gate}");
            assert!(gate.contains("misses_use: true"), "{host:?}: {gate}");
        }
    }

    /// A window's LAN use is kept for later launches of its app on its Mac, which read it as an
    /// earlier use; a terminal session lane's (macOS puts its question to the app that holds its
    /// terminal) and `aterm node serve`'s (whose attribution is unverified) keep none, so a session
    /// lane that used the LAN on a fresh Mac leaves a window's look waiting for System State's
    /// step. Another Mac's or another app's use on a shared home is not this one's, and with no
    /// name for here nothing is kept.
    #[test]
    fn only_a_windows_lan_use_reads_as_its_apps_earlier_use_here() {
        const HERE: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm.app";
        let here = || Some(HERE.to_owned());
        for host in [WINDOW, Host::SessionLane, Host::NodeServe] {
            let root = aterm_tempfile::tempdir().unwrap();
            let config = root.path().join("aterm.toml");
            let record = aterm_db::network_state::directory(&config);
            let keep = super::use_keeper(record.clone(), host, here);
            if let Some(keep) = &keep {
                assert!(keep(), "{host:?}: kept");
            }
            let window = Look {
                used_before: super::used_before(&config, &record, Some(HERE)),
                ..looking(LocalNetworkFirstUse::StepToCome, false)
            };
            if host == WINDOW {
                assert_eq!(
                    look(window, true, true),
                    (
                        LanTurn::Open("an earlier aterm used the local network on this Mac"),
                        1,
                        1
                    ),
                    "a window of this app here used the LAN"
                );
            } else {
                assert_eq!(
                    look(window, true, true),
                    (STEP, 0, 0),
                    "{host:?}'s use is not aterm.app's: the window waits for its step"
                );
            }
            assert_eq!(keep.is_some(), host == WINDOW, "{host:?}");
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let record = aterm_db::network_state::directory(&config);
        for other in [
            "0E2C5A41-8D7B-4F0E-9A63-2B5D7C1E4F90 /Applications/aterm.app",
            "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 /Applications/aterm (dev).app",
        ] {
            super::keep_here(&record, super::USED_HERE, other).unwrap();
            assert!(
                !super::used_before(&config, &record, Some(HERE)),
                "{other}: not this Mac's app"
            );
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let record = root.path().join("network-state");
        assert!(
            super::use_keeper(record.clone(), WINDOW, || None).unwrap()(),
            "no name for here: nothing to keep"
        );
        assert!(!record.exists());
    }
}

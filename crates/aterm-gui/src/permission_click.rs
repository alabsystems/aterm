// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A person's click on a Claude Code tab's off permission chip (`○ yolo`), as the window holds
//! it (`docs/DESIGN-yolo-binary-2026-11-04.md` §2, §3): the App's table of standing asks, one a
//! tab ([`Clicks`]), and the lane that looks at each on its own thread through the harness's one
//! look ([`harness::look`]), paced as the ⬆️ press lane is. Nothing is typed and nothing is on
//! disk while an ask stands; the restart it makes is the harness's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::permission_click::{self as harness, Click, End, Look, Wait};

use crate::App;

/// Whether this host has the restart a click asks (`docs/DESIGN-yolo-binary-2026-11-04.md` §2: no
/// click on a platform with no restart). The harness types the restart's line in the tab's zsh,
/// bash or fish (`aterm_agent::harness::upgrade::Dialect`) and ends the agent by a unix signal, so
/// on any other host the light is drawn with no click: no `click:` in its title, and a press on it
/// does nothing.
pub(crate) const RESTARTS: bool = cfg!(unix);

/// How long a standing click holds the loop's own act at a rest ([`Hold`]): three of its lane's
/// calm paces, so a lane that looks at every rest is not starved by a prompt typed over it, and a
/// lane stuck on a wait costs Cruise one bound.
pub(crate) const CLICK_HOLD: Duration = Duration::from_secs(30);

/// Why a click can never be made here, as the light's title says it ([`harness::Refusal`], its
/// free word dropped for a plain reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ClickRefusal {
    /// The agent is no shell's job.
    NotAShellJob,
    /// Its shell types no restart.
    Shell,
    /// No restart lane reads the agent.
    NoLane,
    /// The agent's command line cannot be read.
    ArgvUnread,
    /// Another supervisor claims the tab.
    Claimed,
    /// The tab is a credit claim's worker.
    ClaimWorker,
    /// The conversation's record is under native coordination: its agent must end first.
    Coordinated,
    /// Any other refusal of the restart (an unknown permission flag's spelling, a one-shot line).
    Line,
}

impl ClickRefusal {
    /// The plain reason for `why`.
    pub(crate) const fn of(why: &harness::Refusal) -> Self {
        match why {
            harness::Refusal::NotAShellJob => Self::NotAShellJob,
            harness::Refusal::Shell => Self::Shell,
            harness::Refusal::NoLane => Self::NoLane,
            harness::Refusal::ArgvUnread => Self::ArgvUnread,
            harness::Refusal::Claimed => Self::Claimed,
            harness::Refusal::ClaimWorker => Self::ClaimWorker,
            harness::Refusal::Coordinated => Self::Coordinated,
            harness::Refusal::Other(_) => Self::Line,
        }
    }

    /// What the light's title says of it.
    pub(crate) const fn says(self) -> &'static str {
        match self {
            Self::NotAShellJob => "not started from a shell",
            Self::Shell => "its shell types no restart",
            Self::NoLane => "no restart for this agent",
            Self::ArgvUnread => "its command line can't be read",
            Self::Claimed => "another supervisor has this tab",
            Self::ClaimWorker => "a credit claim's worker",
            Self::Coordinated => "its agent must end first",
            Self::Line => "its command line can't be rewritten",
        }
    }
}

/// The words of what a standing click waits for, as its title says them after `restarts`:
/// the ⬆️'s own words for a turn, a dialog, a draft, a hold, a limit and a login
/// ([`crate::claude_footer::up_wait`]).
fn wait_words(wait: Option<&Wait>) -> &'static str {
    let word = match wait {
        None => return "when it can",
        Some(Wait::Turn) => "busy",
        Some(Wait::Box) => "box",
        Some(Wait::Draft) => "draft",
        Some(Wait::Held) => "held",
        Some(Wait::Limit) => "limited",
        Some(Wait::Login) => "login",
        Some(Wait::Pressed) => return "after the update",
        Some(Wait::Restart) => return "after the restart under way",
        Some(Wait::Relaunch) => return "once its agent is back",
        Some(Wait::Other(word)) => word,
    };
    match crate::claude_footer::up_wait(word) {
        "" => "when it can",
        words => words,
    }
}

/// A standing click as a frame draws it ([`Clicks::shown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ClickShown {
    /// What it waits for ([`wait_words`]); `None` once its restart was signalled, when it can no
    /// longer be taken back.
    pub(crate) when: Option<&'static str>,
}

/// The bound a standing click holds the loop's own act by (design Appendix, "Hold"): it runs
/// only at a rest, from the first rest the loop asks at; a look finding a box or a held tab starts
/// it again, a draft does not (so the loop's own rule sends the draft), and a look finding a turn
/// clears it unless it has run out, when it stays out for that ask. A wall (a usage limit, a
/// login) holds nothing while it stands, the loop's own wait being the wall's; the next turn, box
/// or held tab a look finds lifts it.
#[derive(Debug, Clone, Copy, Default)]
struct Hold {
    until: Option<Instant>,
    out: bool,
    walled: bool,
}

impl Hold {
    /// Whether it holds at a rest at `now`.
    fn holds(&mut self, now: Instant) -> bool {
        if self.out || self.walled {
            return false;
        }
        if *self.until.get_or_insert(now + CLICK_HOLD) > now {
            return true;
        }
        self.out = true;
        false
    }

    /// A look found `wait` at `now`.
    fn looked(&mut self, wait: &Wait, now: Instant) {
        if self.out {
            return;
        }
        match wait {
            Wait::Box | Wait::Held => {
                self.walled = false;
                self.until = Some(now + CLICK_HOLD);
            }
            Wait::Turn if self.until.is_some_and(|until| until <= now) => self.out = true,
            Wait::Turn => {
                self.walled = false;
                self.until = None;
            }
            // At a wall the loop's own wait is the wall's: nothing is held while it stands, and
            // the bound starts again at the first rest after it.
            Wait::Limit | Wait::Login => {
                self.walled = true;
                self.until = None;
            }
            Wait::Draft | Wait::Pressed | Wait::Restart | Wait::Relaunch | Wait::Other(_) => {}
        }
    }
}

/// One tab's standing click.
#[derive(Debug, Clone)]
pub(crate) struct Standing {
    /// The ask the lane looks at.
    pub(crate) click: Click,
    /// The pill read `yolo` at the click.
    pub(crate) from_yolo: bool,
    /// This ask among the table's ([`Clicks::stand`]): a look answers the ask it was made for.
    pub(crate) id: u64,
    /// Its lane's look read the fence toward the signal ([`Clicks::stands`]): a take-back now
    /// marks it ([`Self::taken_back`]) rather than removing it.
    pub(crate) fenced: bool,
    /// Its restart was signalled ([`Look::InFlight`]).
    pub(crate) signalled: bool,
    /// What the last look waited for.
    pub(crate) wait: Option<Wait>,
    hold: Hold,
    /// A click took it back while its lane's look was past the fence ([`Clicks::take_back`]): the
    /// window holds it no more, the fence's next read finds it gone, and the look's answer ends it
    /// unless that answer is its signal ([`Clicks::note`]).
    taken_back: bool,
}

/// What a look did to the ask it was made for ([`Clicks::note`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Noted {
    /// The ask it was made for no longer stands: taken back, its tab closed, or another ask.
    Stale,
    /// It stands, as the look before left it.
    Same,
    /// It stands changed, or it ended.
    Changed,
}

#[derive(Debug, Default)]
struct Table {
    tabs: BTreeMap<String, Standing>,
    ids: u64,
    /// The tabs a lane steps now: one lane a tab.
    laned: BTreeSet<String>,
}

impl Table {
    fn current(&mut self, tab: &str, id: u64) -> Option<&mut Standing> {
        self.tabs.get_mut(tab).filter(|s| s.id == id)
    }
}

/// The window's standing clicks, one a tab by its sid: owned by the App and shared with the
/// host's workers ([`crate::harness_host::Acts::clicks`]), whose loop it holds at a rest
/// ([`Clicks::holds`]). Removed at the tab's close, at its lane's end and when its conversation
/// leaves the tab.
#[derive(Debug, Clone, Default)]
pub(crate) struct Clicks(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
    table: Mutex<Table>,
    /// How many asks the table holds, stored under its lock after every change ([`Clicks::with`]),
    /// so that a frame's read of each Claude Code pane ([`Clicks::shown`]) and the loop's at a rest
    /// ([`Clicks::holds`]) take no lock while none stands, the common case. A read that races a
    /// change on another thread sees the count before it; a stand is made on the main thread, which
    /// the frames read on, and asks for a frame after it.
    asks: AtomicUsize,
}

impl Clicks {
    fn with<T>(&self, f: impl FnOnce(&mut Table) -> T) -> T {
        let mut table = self.0.table.lock().unwrap_or_else(PoisonError::into_inner);
        let out = f(&mut table);
        self.0.asks.store(table.tabs.len(), Ordering::Release);
        out
    }

    /// Whether any ask stands, read with no lock ([`Shared::asks`]).
    fn any(&self) -> bool {
        self.0.asks.load(Ordering::Acquire) != 0
    }

    /// Tab `tab`'s standing click, not taken back.
    pub(crate) fn get(&self, tab: &str) -> Option<Standing> {
        if !self.any() {
            return None;
        }
        self.with(|t| t.tabs.get(tab).filter(|s| !s.taken_back).cloned())
    }

    /// Tab `tab`'s standing click as a frame draws it.
    pub(crate) fn shown(&self, tab: &str) -> Option<ClickShown> {
        if !self.any() {
            return None;
        }
        self.with(|t| {
            t.tabs
                .get(tab)
                .filter(|s| !s.taken_back)
                .map(|s| ClickShown {
                    when: (!s.signalled).then(|| wait_words(s.wait.as_ref())),
                })
        })
    }

    /// Stand `click`, asked from a pill that read `yolo` or not: its ask's generation.
    pub(crate) fn stand(&self, click: Click, from_yolo: bool) -> u64 {
        self.with(|t| {
            t.ids += 1;
            let id = t.ids;
            t.tabs.insert(
                click.tab.clone(),
                Standing {
                    click,
                    from_yolo,
                    id,
                    fenced: false,
                    signalled: false,
                    wait: None,
                    hold: Hold::default(),
                    taken_back: false,
                },
            );
            id
        })
    }

    /// Take tab `tab`'s click back, until its restart is signalled (design §2): at once where no
    /// look of its lane is past the fence, else marked, so the fence's read just before the signal
    /// finds it gone and that look's answer ends it ([`Self::note`]); a signal already past that
    /// read keeps it. The ask taken back, if one was.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "TakeBack",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    pub(crate) fn take_back(&self, tab: &str) -> Option<Standing> {
        self.with(|t| {
            let s = t
                .tabs
                .get_mut(tab)
                .filter(|s| !s.signalled && !s.taken_back)?;
            if s.fenced {
                s.taken_back = true;
                return Some(s.clone());
            }
            t.tabs.remove(tab)
        })
    }

    /// THE FENCE the harness reads with no I/O before it takes the hand and again just before the
    /// signal ([`harness::look`]'s `stands`): whether the ask `id` of `tab` still stands, not taken
    /// back and its one restart not signalled yet (a look past its signal reaches the fence only
    /// where its own record no longer stands, superseded or gone, and must not signal again). A
    /// yes fences it, so a take-back from here on only marks it ([`Self::take_back`]); the look's
    /// own answer lifts the fence where it signalled nothing ([`Self::note`]).
    pub(crate) fn stands(&self, tab: &str, id: u64) -> bool {
        self.with(|t| {
            t.current(tab, id).is_some_and(|s| {
                if s.signalled || s.taken_back {
                    return false;
                }
                s.fenced = true;
                true
            })
        })
    }

    /// Whether tab `tab`'s standing click holds the loop's own act at a rest at `now` ([`Hold`]).
    pub(crate) fn holds(&self, tab: &str, now: Instant) -> bool {
        if !self.any() {
            return false;
        }
        self.with(|t| {
            t.tabs
                .get_mut(tab)
                .is_some_and(|s| !s.taken_back && s.hold.holds(now))
        })
    }

    /// Fold the look `look` made for the ask `id` of `tab` in, at `now`: a wait noted, the
    /// restart signalled, the ask re-keyed to the conversation its own restart afresh started, or
    /// the ask ended (landed, failed, refused, its conversation gone, taken back).
    pub(crate) fn note(&self, tab: &str, id: u64, look: &Look, now: Instant) -> Noted {
        // A take-back that came while the look was past the fence lands with its answer, unless
        // that answer is the signal.
        if !matches!(look, Look::InFlight(_))
            && self.with(|t| t.current(tab, id).is_some_and(|s| s.taken_back))
        {
            return self.end(tab, id);
        }
        match look {
            Look::Waits(wait) => self.with(|t| {
                let Some(s) = t.current(tab, id) else {
                    return Noted::Stale;
                };
                s.fenced = false;
                s.hold.looked(wait, now);
                if s.wait.as_ref() == Some(wait) {
                    return Noted::Same;
                }
                s.wait = Some(wait.clone());
                Noted::Changed
            }),
            Look::InFlight(_) => self.signal(tab, id),
            Look::Moved(conversation) => self.with(|t| {
                let Some(s) = t.current(tab, id) else {
                    return Noted::Stale;
                };
                s.fenced = false;
                s.click.conversation.clone_from(conversation);
                Noted::Changed
            }),
            Look::Done | Look::Failed(_) => self.land(tab, id),
            Look::Refused(_) | Look::Ended(End::TakenBack) => self.end(tab, id),
            Look::Ended(End::NotClaude | End::ConversationLeft) => self.leave(tab, id),
        }
    }

    /// The ask `id` of `tab`'s restart was signalled: it stands until the restart lands, a
    /// take-back that came after the fence's last read included.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Signal",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn signal(&self, tab: &str, id: u64) -> Noted {
        self.with(|t| match t.current(tab, id) {
            Some(s) if s.signalled => Noted::Same,
            Some(s) => {
                s.signalled = true;
                if std::mem::take(&mut s.taken_back) {
                    aterm_log::info!(
                        "harness @{tab}: the permission click was taken back after its signal: \
                         the restart goes"
                    );
                }
                Noted::Changed
            }
            None => Noted::Stale,
        })
    }

    /// The ask `id` of `tab`'s restart landed or failed: it ends there, whatever the pill reads.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Land",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn land(&self, tab: &str, id: u64) -> Noted {
        self.end(tab, id)
    }

    /// The conversation of the ask `id` of `tab` left the tab, or no Claude Code is in it: the
    /// ask ends.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Leave",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn leave(&self, tab: &str, id: u64) -> Noted {
        self.end(tab, id)
    }

    fn end(&self, tab: &str, id: u64) -> Noted {
        self.with(|t| {
            if t.current(tab, id).is_none() {
                return Noted::Stale;
            }
            t.tabs.remove(tab);
            Noted::Changed
        })
    }

    /// Tab `tab` closed: its ask ends with it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "CloseTab",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    pub(crate) fn close_tab(&self, tab: &str) {
        self.with(|t| t.tabs.remove(tab));
    }

    /// Every standing click not taken back, as a seamless update's Commit drops them: nothing of
    /// one is on disk to carry (`docs/DESIGN-yolo-binary-2026-11-04.md` §9).
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only unix hands off across a seamless update")
    )]
    pub(crate) fn standing(&self) -> Vec<Standing> {
        self.with(|t| t.tabs.values().filter(|s| !s.taken_back).cloned().collect())
    }

    /// Whether a lane starts for `tab`: none steps it yet.
    fn lane_starts(&self, tab: &str) -> bool {
        self.with(|t| t.laned.insert(tab.to_owned()))
    }

    /// The ask a lane for `tab` looks at next, or `None`, which ends the lane in the same hold.
    fn lane_next(&self, tab: &str) -> Option<(Click, u64)> {
        self.with(|t| match t.tabs.get(tab) {
            Some(s) => Some((s.click.clone(), s.id)),
            None => {
                t.laned.remove(tab);
                None
            }
        })
    }

    /// The lane for `tab` did not start.
    fn lane_ended(&self, tab: &str) {
        self.with(|t| t.laned.remove(tab));
    }

    /// Run tab `tab`'s hold out, as its bound passing would.
    #[cfg(test)]
    pub(crate) fn run_hold_out(&self, tab: &str) {
        self.with(|t| {
            if let Some(s) = t.tabs.get_mut(tab) {
                s.hold.until = Some(Instant::now());
            }
        });
    }
}

/// One look of a click's lane: [`harness::look`]'s, with `relaunch_due` and the fence `stands`.
type LookFn = dyn Fn(&Click, bool, &dyn Fn() -> bool) -> Look + Send + Sync;

/// What looks at a standing click ([`App::start_click_lane`]).
#[derive(Clone)]
pub(crate) struct Clicker(Arc<LookFn>);

impl Clicker {
    /// Wrap a look.
    pub(crate) fn new(
        f: impl Fn(&Click, bool, &dyn Fn() -> bool) -> Look + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f))
    }

    fn look(&self, click: &Click, relaunch_due: bool, stands: &dyn Fn() -> bool) -> Look {
        (self.0)(click, relaunch_due, stands)
    }
}

/// With no home or harness state to restart in, a click is refused.
impl Default for Clicker {
    fn default() -> Self {
        Self::new(|_, _, _| Look::Refused(harness::Refusal::Other("no-harness-state".into())))
    }
}

impl std::fmt::Debug for Clicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clicker")
    }
}

/// Production [`Clicker`] over the user's home and harness state.
pub(crate) fn live_clicker(sock: Option<String>) -> Clicker {
    match aterm_primer::home_dir().zip(crate::harness_host::harness_state()) {
        Some((home, state)) => clicker_at(home, state, sock),
        None => Clicker::default(),
    }
}

/// [`Clicker`] over `home`, `state` and control socket `sock`: the window's drive opts, the
/// relaunch line arming the usage tap as the host's own step does; [`harness::look`] narrows them
/// to the clicked tab at grace 0.
fn clicker_at(home: PathBuf, state: PathBuf, sock: Option<String>) -> Clicker {
    Clicker::new(move |click, relaunch_due, stands| {
        let opts = crate::harness_host::window_drive_opts_with_tap(
            home.clone(),
            state.clone(),
            sock.clone(),
            crate::harness_host::usage_tap_on(),
        );
        harness::look(&opts, click, relaunch_due, stands)
    })
}

/// The App's clicks: the table and what looks at it.
#[derive(Debug, Default)]
pub(crate) struct ClickView {
    pub(crate) clicks: Clicks,
    clicker: Clicker,
}

impl ClickView {
    /// A view over `clicks`, the table the host's workers share, looked at through `clicker`.
    pub(crate) const fn with(clicks: Clicks, clicker: Clicker) -> Self {
        Self { clicks, clicker }
    }

    /// What looks at a click from here on, as a test scripts it.
    #[cfg(all(test, unix))]
    pub(crate) fn set_clicker(&mut self, clicker: Clicker) {
        self.clicker = clicker;
    }
}

/// Journal what `look` found for tab `tab`'s click where it moved the ask.
fn journal(tab: &str, look: &Look, noted: Noted) {
    if noted == Noted::Changed {
        aterm_log::info!("harness @{tab}: the permission click's lane: {look:?}");
    }
}

/// Look at tab `tab`'s standing click through `clicker` until none stands, handing each look
/// that moved it to `deliver` and pausing `pace` between looks.
fn run_click_lane(
    tab: &str,
    clicks: &Clicks,
    clicker: &Clicker,
    relaunch_due: impl Fn() -> bool,
    pace: impl Fn(Duration) -> Duration,
    deliver: impl Fn(Look),
) {
    let began = Instant::now();
    while let Some((click, id)) = clicks.lane_next(tab) {
        let look = clicker.look(&click, relaunch_due(), &|| clicks.stands(tab, id));
        let noted = clicks.note(tab, id, &look, Instant::now());
        journal(tab, &look, noted);
        if noted == Noted::Changed {
            deliver(look);
        }
        std::thread::sleep(pace(began.elapsed()));
    }
}

impl App {
    /// Whether the host relaunches tab `tab`'s agent when it ends ([`harness::look`]'s
    /// `relaunch_due`).
    fn click_relaunch_due(&self, tab: &str) -> bool {
        self.harness.as_ref().is_some_and(|h| h.relaunch_due(tab))
    }

    /// Starts the lane of tab `tab`'s standing click off the winit thread, where no lane looks
    /// at it yet: it looks until no click stands there, each look that moved the ask coming back
    /// as `Wake::PermissionClick`. Without an event loop (a test's app) one look runs here.
    pub(crate) fn start_click_lane(&mut self, tab: &str) {
        let Some(proxy) = self.proxy.clone() else {
            self.step_click(tab);
            return;
        };
        let clicks = self.permission_clicks.clicks.clone();
        if !clicks.lane_starts(tab) {
            return;
        }
        let clicker = self.permission_clicks.clicker.clone();
        let host = self.harness.clone();
        let lane = tab.to_owned();
        let started = crate::qos::spawn_lane_thread(
            "aterm-permission-click".to_string(),
            crate::qos::Role::Responsive,
            move || {
                let due = || host.as_ref().is_some_and(|h| h.relaunch_due(&lane));
                run_click_lane(
                    &lane,
                    &clicks,
                    &clicker,
                    due,
                    crate::upgrade_host::press_pace,
                    |look| {
                        let _ = proxy.send_event(crate::Wake::PermissionClick {
                            tab: lane.clone(),
                            look,
                        });
                    },
                );
            },
        );
        if let Err(e) = started {
            aterm_log::warn!("harness @{tab}: the permission click's lane did not start: {e}");
            self.permission_clicks.clicks.lane_ended(tab);
        }
    }

    /// One look at tab `tab`'s standing click, here: a test's step, and the lane's where no event
    /// loop runs it.
    pub(crate) fn step_click(&mut self, tab: &str) {
        let Some(standing) = self.permission_clicks.clicks.get(tab) else {
            return;
        };
        let clicks = self.permission_clicks.clicks.clone();
        let id = standing.id;
        let look = self.permission_clicks.clicker.look(
            &standing.click,
            self.click_relaunch_due(tab),
            &|| clicks.stands(tab, id),
        );
        let noted = clicks.note(tab, id, &look, Instant::now());
        journal(tab, &look, noted);
        if noted == Noted::Changed {
            self.apply_click_look(tab, look);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::supervise::policy::turn_end::PermissionTo;

    fn click(tab: &str) -> Click {
        Click {
            tab: tab.to_owned(),
            conversation: "c-1".to_owned(),
            to: PermissionTo::Bypass,
            at: 1,
        }
    }

    /// A TAKE-BACK PAST THE FENCE LANDS UNLESS THE SIGNAL WENT (design §2: a click on `◐` "takes
    /// it back, until the signal goes"): once the lane's look read the ask standing, before the
    /// hand, a click on it still offered as one to take back marks it, the window draws and holds
    /// it no more, the fence's read just before the signal finds it gone, and the look's answer
    /// that it waits ends it rather than leaving it to signal at the next rest. NEGATIVE CONTROLS:
    /// a signal already past the fence's last read keeps the ask, and nothing takes it back after;
    /// with no look under way the first click takes it back at once, and a look for it moves
    /// nothing.
    #[test]
    fn a_take_back_past_the_fence_lands_unless_the_signal_went() {
        let clicks = Clicks::default();
        let now = Instant::now();
        let id = clicks.stand(click("s-a"), false);
        assert!(clicks.stands("s-a", id), "the look's read before the hand");
        assert_eq!(
            clicks.shown("s-a"),
            Some(ClickShown {
                when: Some("when it can")
            }),
            "past the fence, unsignalled: still offered to take back"
        );
        assert!(
            clicks.take_back("s-a").is_some(),
            "taken back past the fence"
        );
        assert_eq!(clicks.shown("s-a"), None, "drawn as taken back");
        assert!(clicks.get("s-a").is_none());
        assert!(!clicks.holds("s-a", now), "it holds nothing");
        assert!(
            !clicks.stands("s-a", id),
            "the read before the signal finds it gone"
        );
        assert_eq!(
            clicks.note("s-a", id, &Look::Waits(Wait::Other("changed".into())), now),
            Noted::Changed
        );
        assert!(
            clicks.lane_next("s-a").is_none(),
            "the look's wait ended it, and its lane with it"
        );

        let id = clicks.stand(click("s-c"), false);
        assert!(clicks.stands("s-c", id));
        assert!(
            clicks.take_back("s-c").is_some(),
            "after the fence's last read"
        );
        assert_eq!(
            clicks.note("s-c", id, &Look::InFlight("signalled".into()), now),
            Noted::Changed
        );
        assert!(
            clicks.get("s-c").is_some_and(|s| s.signalled),
            "control: the signal went first"
        );
        assert!(clicks.take_back("s-c").is_none(), "control: signalled");

        let id = clicks.stand(click("s-b"), false);
        assert!(clicks.take_back("s-b").is_some(), "no look under way");
        assert!(!clicks.stands("s-b", id));
        assert_eq!(
            clicks.note("s-b", id, &Look::InFlight("x".into()), now),
            Noted::Stale,
            "a look for an ask taken back moves nothing"
        );
    }

    /// ONE RESTART A CLICK (the model's `OneRestartPerClick`): once its restart was signalled, a
    /// later look that waits (its record superseded by another restart's, which it waits behind)
    /// and then reaches the restart again reads the fence false, so it ends with nothing signalled
    /// a second time. NEGATIVE CONTROL: an ask not yet signalled reads it standing after a wait.
    #[test]
    fn a_signalled_click_never_passes_the_fence_again() {
        let clicks = Clicks::default();
        let now = Instant::now();
        let id = clicks.stand(click("s-a"), false);
        assert!(clicks.stands("s-a", id));
        assert_eq!(
            clicks.note("s-a", id, &Look::InFlight("signalled".into()), now),
            Noted::Changed
        );
        assert_eq!(
            clicks.note("s-a", id, &Look::Waits(Wait::Restart), now),
            Noted::Changed
        );
        assert!(
            !clicks.stands("s-a", id),
            "signalled once: no second signal"
        );
        let id = clicks.stand(click("s-b"), false);
        assert!(clicks.stands("s-b", id));
        clicks.note("s-b", id, &Look::Waits(Wait::Turn), now);
        assert!(clicks.stands("s-b", id), "control: not signalled yet");
    }

    /// THE HOLD (design Appendix, "Hold"): it starts at the first rest asked, a box or a held tab
    /// starts it again, a draft does not, a turn clears it while it has time left (so a later
    /// rest starts it again) and leaves it out once it ran out, and a wall lets it go while it
    /// stands, the turn after it holding again. NEGATIVE CONTROL: a click taken back holds nothing.
    #[test]
    fn a_standing_click_holds_the_loops_act_for_its_own_bound() {
        let clicks = Clicks::default();
        let id = clicks.stand(click("s-h"), false);
        let now = Instant::now();
        assert!(clicks.holds("s-h", now), "from the first rest");
        clicks.run_hold_out("s-h");
        assert!(!clicks.holds("s-h", Instant::now()), "its bound");
        let id2 = clicks.stand(click("s-h"), false);
        assert!(id2 > id, "a new ask");
        assert!(clicks.holds("s-h", Instant::now()));
        clicks.run_hold_out("s-h");
        clicks.note("s-h", id2, &Look::Waits(Wait::Box), Instant::now());
        assert!(clicks.holds("s-h", Instant::now()), "a box starts it again");
        clicks.run_hold_out("s-h");
        clicks.note("s-h", id2, &Look::Waits(Wait::Draft), Instant::now());
        assert!(!clicks.holds("s-h", Instant::now()), "a draft does not");
        let gen3 = clicks.stand(click("s-h"), false);
        let rest = Instant::now();
        assert!(clicks.holds("s-h", rest));
        clicks.note("s-h", gen3, &Look::Waits(Wait::Turn), rest);
        assert!(
            clicks.holds("s-h", rest + CLICK_HOLD * 2),
            "a turn with time left cleared it: a later rest starts it again"
        );
        clicks.note("s-h", gen3, &Look::Waits(Wait::Held), Instant::now());
        clicks.run_hold_out("s-h");
        clicks.note("s-h", gen3, &Look::Waits(Wait::Turn), Instant::now());
        assert!(!clicks.holds("s-h", Instant::now()), "run out, out");
        clicks.note("s-h", gen3, &Look::Waits(Wait::Box), Instant::now());
        assert!(!clicks.holds("s-h", Instant::now()), "and out for this ask");
        let gen4 = clicks.stand(click("s-h"), false);
        clicks.note("s-h", gen4, &Look::Waits(Wait::Limit), Instant::now());
        assert!(!clicks.holds("s-h", Instant::now()), "a wall's own wait");
        clicks.note("s-h", gen4, &Look::Waits(Wait::Turn), Instant::now());
        assert!(
            clicks.holds("s-h", Instant::now()),
            "the turn after the wall: the next rest holds again"
        );
        clicks.stand(click("s-h"), false);
        assert!(clicks.take_back("s-h").is_some());
        assert!(!clicks.holds("s-h", Instant::now()), "nothing stands");
    }
}

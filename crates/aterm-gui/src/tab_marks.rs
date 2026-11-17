// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO ACTED ON A TAB, AND WHY (the Workers as subagents design (2026-11-04) §7): the scope
//! an act opens on the main thread while it places or closes a tab, and the choke points that
//! write each `tab` mark ([`crate::session_log::SessionLog::open_tab`], `keep_tab`, `close_tab`).
//!
//! An act names its actor and its reason by an [`ActScope`] it enters (aterm's own: a credit
//! claim's worker `by=harness why=claim-credits`; a control client's `spawn` or `tab new`, by the
//! caller `crate::control::control_session::caller_actor_of` names). A gesture's own act (a
//! Summarize press, a sign-in, an install) is whoever drives the gesture ([`ActScope::gesture`]):
//! the close attribution's actor a control client's `invoke` opened, else the person. Where no
//! scope is open, a close attribution's actor stands (a control client's `tab` gesture), and where
//! neither is, the act is the person's own gesture (`by=human`), its reason where the tab landed
//! (`new-tab`, `split`, `new-window`). A spawn that lands later carries the scope it was booked
//! under (`crate::tab_spawn`'s in-flight record) and re-enters it as it installs.
//!
//! A close's process totals come from one read of the tab's process tree as its terminal is about
//! to close ([`tree_spent`], `App::note_tab_spent`): until the terminal hangs up, its processes
//! still answer the kernel's lifetime counters.

use std::cell::RefCell;
use std::sync::Arc;

use crate::session_store::ExitActor;

/// An act under way: who acts, and why (a word of the `why=` vocabulary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Acting {
    pub(crate) by: ExitActor,
    pub(crate) why: Option<&'static str>,
    /// The request's own line (`spawn why=`), which a tab it opens records as `why_text=`.
    pub(crate) asked: Option<Arc<str>>,
    /// Who on another Mac asked, for a fleet drive's act: a tab it opens records the requester's
    /// program, model and account (`agent=`, `model=`, `account=`).
    pub(crate) requester: Option<Arc<aterm_types::control_verbs::Requester>>,
    /// What a spawn said of the session it opens: the first open under the act takes it
    /// ([`take_birth`]) and writes its birth claims.
    pub(crate) birth: Option<Arc<crate::session_log::Birth>>,
    /// The act closes a tab (`unstarted`): its `why=` is the close's, never an open's.
    closes: bool,
}

thread_local! {
    // Boxed: inline it was 112 bytes of every thread's thread-local template
    // (`tests/thread_local_template.rs`). The box is made on a thread's first act or read.
    static ACTING: Box<RefCell<Option<Acting>>> = Box::new(RefCell::new(None));
}

/// RAII scope naming the act under way on this thread; the outermost scope wins.
pub(crate) struct ActScope {
    opened: bool,
}

impl ActScope {
    /// An act by `by` that places tabs, for `why`.
    #[must_use = "the act lasts exactly as long as this guard lives"]
    pub(crate) fn enter(by: ExitActor, why: &'static str) -> Self {
        Self::resume(Some(Acting {
            by,
            why: Some(why),
            asked: None,
            requester: None,
            birth: None,
            closes: false,
        }))
    }

    /// [`Self::enter`] for a spawn that said what it opens in `birth` (its `why=`, which a tab it
    /// opens records as `why_text=`, its task's line, `package=` and `from=`), from `requester`
    /// on another Mac where a fleet drive asked it.
    #[must_use = "the act lasts exactly as long as this guard lives"]
    pub(crate) fn asking(
        by: ExitActor,
        why: &'static str,
        birth: crate::session_log::Birth,
        requester: Option<aterm_types::control_verbs::Requester>,
    ) -> Self {
        Self::resume(Some(Acting {
            by,
            why: Some(why),
            asked: birth.matters.as_deref().map(Arc::from),
            requester: requester.map(Arc::new),
            birth: Some(Arc::new(birth)),
            closes: false,
        }))
    }

    /// A gesture's act, for `why`: by whoever drives the gesture, the actor of the close
    /// attribution a control client's `invoke` opened ([`crate::session_store::CloseAttribution`]),
    /// else the person.
    #[must_use = "the act lasts exactly as long as this guard lives"]
    pub(crate) fn gesture(why: &'static str) -> Self {
        let (_, by) = crate::session_store::current_close_attribution();
        let by = if by == ExitActor::Unknown {
            ExitActor::Human
        } else {
            by
        };
        Self::enter(by, why)
    }

    /// An act by `by` that closes a tab, for `why`.
    #[must_use = "the act lasts exactly as long as this guard lives"]
    pub(crate) fn closing(by: ExitActor, why: &'static str) -> Self {
        Self::resume(Some(Acting {
            by,
            why: Some(why),
            asked: None,
            requester: None,
            birth: None,
            closes: true,
        }))
    }

    /// Re-enter `acting`, the act a spawn was booked under ([`acting`]); nothing for none.
    #[must_use = "the act lasts exactly as long as this guard lives"]
    pub(crate) fn resume(acting: Option<Acting>) -> Self {
        let opened = acting.is_some_and(|acting| {
            ACTING.with(|slot| {
                let mut slot = slot.borrow_mut();
                slot.is_none() && {
                    *slot = Some(acting);
                    true
                }
            })
        });
        Self { opened }
    }
}

impl Drop for ActScope {
    fn drop(&mut self) {
        if self.opened {
            ACTING.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// The act under way on this thread: an [`ActScope`]'s, else the open close attribution's actor
/// with no reason of its own, else none (a person's gesture).
#[must_use]
pub(crate) fn acting() -> Option<Acting> {
    ACTING.with(|slot| slot.borrow().clone()).or_else(|| {
        let (_, by) = crate::session_store::current_close_attribution();
        (by != ExitActor::Unknown).then_some(Acting {
            by,
            why: None,
            asked: None,
            requester: None,
            birth: None,
            closes: false,
        })
    })
}

/// TAKE THE BIRTH of the act under way on this thread: the first open under the act takes it, and
/// a later open under the same act gets none, since a birth is one session's. An async spawn's
/// booked copy of the act ([`acting`]) carries its own birth to the land, whose open takes it.
pub(crate) fn take_birth() -> Option<Arc<crate::session_log::Birth>> {
    ACTING.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .filter(|acting| !acting.closes)
            .and_then(|acting| acting.birth.take())
    })
}

/// A close's `why=`: the reaper's `concluded`, else the closing act's own (`unstarted`).
#[must_use]
pub(crate) fn close_why(reason: crate::session_store::ExitReason) -> Option<&'static str> {
    if reason == crate::session_store::ExitReason::Reaper {
        return Some("concluded");
    }
    ACTING
        .with(|slot| slot.borrow().clone())
        .filter(|acting| acting.closes)
        .and_then(|acting| acting.why)
}

/// The open's actor, reason, request line and requester: the act under way's, its reason else
/// `place_why`; a person's gesture, asking nothing, where none is.
fn open_by(place_why: &'static str) -> (ExitActor, &'static str, crate::session_log::TabAsked) {
    match acting() {
        Some(acting) if !acting.closes => (
            acting.by,
            acting.why.unwrap_or(place_why),
            crate::session_log::TabAsked {
                why_text: acting.asked,
                requester: acting.requester,
            },
        ),
        _ => (
            ExitActor::Human,
            place_why,
            crate::session_log::TabAsked::default(),
        ),
    }
}

/// The most processes one read of a tab's tree visits ([`tree_spent`]).
pub(crate) const TREE_MAX: usize = 512;

/// Who a Keep as Tab is by: the act it runs under, else the person.
pub(crate) fn keep_actor() -> ExitActor {
    acting()
        .filter(|acting| !acting.closes)
        .map_or(ExitActor::Human, |acting| acting.by)
}

/// The kernel reads [`tree_spent`] takes, injected so a test's table stands in for the host's.
#[derive(Clone, Copy)]
pub(crate) struct TreeProbe {
    /// A process's children ([`aterm_sysprobe::jobs::child_pids`]).
    pub(crate) children: fn(u32) -> Option<Vec<u32>>,
    /// A process's lifetime counters ([`aterm_sysprobe::jobs::counters_of`]).
    pub(crate) counters: fn(u32) -> Option<aterm_sysprobe::jobs::Counters>,
}

impl TreeProbe {
    /// This host's kernel.
    pub(crate) const HOST: Self = Self {
        children: aterm_sysprobe::jobs::child_pids,
        counters: aterm_sysprobe::jobs::counters_of,
    };

    /// A probe that reads nothing: every total it backs is `unavailable`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) const INERT: Self = Self {
        children: |_| None,
        counters: |_| None,
    };
}

/// The processes of the tree under `shell` and each one's counters, as one read finds them: the
/// shell first, then each process once, at most [`TREE_MAX`]; a process whose counters are
/// refused is left out, and nothing at all where the shell's own are (it is gone, or refused).
pub(crate) fn tree_counters(
    shell: i32,
    probe: &TreeProbe,
) -> Vec<(u32, aterm_sysprobe::jobs::Counters)> {
    let Some(root) = u32::try_from(shell).ok().filter(|pid| *pid > 1) else {
        return Vec::new();
    };
    let Some(first) = (probe.counters)(root) else {
        return Vec::new();
    };
    let mut read = vec![(root, first)];
    let mut seen = std::collections::BTreeSet::from([root]);
    let mut next: Vec<u32> = (probe.children)(root).unwrap_or_default();
    while let Some(pid) = next.pop() {
        if seen.len() >= TREE_MAX || !seen.insert(pid) {
            continue;
        }
        if let Some(c) = (probe.counters)(pid) {
            read.push((pid, c));
        }
        next.extend((probe.children)(pid).unwrap_or_default());
    }
    read
}

/// What the process tree under `shell` spent over its life, as one read finds it
/// ([`tree_counters`]).
///
/// CPU is each live process's own time plus what it reaped of its children: the kernel adds a
/// reaped child's time, its own reaped children's included, to its parent's, so every process that
/// ran in the tab counts once, alive or reaped by a live member. A process that reaps its children
/// unwaited is charged their time twice by the kernel, and an orphan that left the tree took its
/// time with it: the figure is what the tree's counters hold. Peak memory is the largest lifetime
/// footprint of a process alive at the read, a floor of the tab's own peak. Disk written is what
/// the processes alive at the read wrote, a floor too: the kernel adds no reaped child's writes
/// to its parent's. Each is `None` where the shell's own counters cannot be read (it is gone, or
/// refused), or where no process's counters hold it.
pub(crate) fn tree_spent(shell: i32, probe: &TreeProbe) -> crate::session_log::TreeSpent {
    let read = tree_counters(shell, probe);
    if read.is_empty() {
        return crate::session_log::TreeSpent::default();
    }
    let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    };
    read.iter().fold(
        crate::session_log::TreeSpent {
            cpu_ns: Some(0),
            ..crate::session_log::TreeSpent::default()
        },
        |spent, (_, c)| crate::session_log::TreeSpent {
            cpu_ns: sum(
                spent.cpu_ns,
                Some(c.cpu_ns.saturating_add(c.child_cpu_ns.unwrap_or(0))),
            ),
            peak_bytes: match (spent.peak_bytes, c.peak_footprint) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
            written_bytes: sum(spent.written_bytes, c.disk_written),
        },
    )
}

/// A worker's task as its close says it: `<state>` or `<state>:<why>`.
pub(crate) fn task_end(tl: &crate::session_timeline::SessionTimeline) -> Option<String> {
    tl.task().map(|r| match r.why {
        Some(why) => format!("{}:{why}", r.state.as_str()),
        None => r.state.as_str().to_string(),
    })
}

impl crate::App {
    /// THE OPEN CHOKE POINT: session `local` took its place in `window` (parked under its Workers
    /// tab or not), opened from `parent`; `place_why` the gesture the place says where no act
    /// names its own. Nothing for a shell a seamless update handed over: its tab did not open
    /// here, and its log carried the place its open named (`tab v=1`).
    pub(crate) fn note_tab_open(
        &self,
        local: u64,
        window: crate::WindowId,
        parked: bool,
        parent: Option<&aterm_session::SessionId>,
        place_why: &'static str,
    ) {
        // A shell a seamless update handed over keeps the place its carried log names.
        let Some(ctx) = self
            .pool
            .get(local)
            .filter(|s| s.handoff_local_id.is_none())
            .map(|s| Arc::clone(&s.ctx))
        else {
            return;
        };
        let birth = take_birth();
        let (by, why, asked) = open_by(place_why);
        // Resolved before the new session's timeline is locked: the store's lock and the
        // creator's timeline lock are each taken alone.
        let born = birth
            .as_ref()
            .map(|birth| self.birth_of(birth, &by, asked.requester.is_some()));
        let mut tl = ctx
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = tl.slog().now();
        let place = crate::session_log::TabPlace {
            opened_wall: at.wall_ms,
            window: window.0,
            parked,
            parent: parent.and_then(|p| aterm_slog::ShortId::new(p.as_str())),
        };
        let by = crate::session_log::exit_actor(&by);
        tl.slog_mut().open_tab(place, why, &asked, by, at);
        if let (Some(birth), Some(born)) = (birth.as_deref(), born.as_ref()) {
            let _ = tl.slog_mut().claim_birth(&born.claims(birth), at);
        }
    }

    /// WHAT AN OPEN MAKES OF `birth`, its act by `by` (asked over a fleet drive where `fleet`):
    /// the creator its `from=` names as the store resolves it ([`Self::resolved_from`]), who its
    /// claims are by ([`crate::session_log::BirthAuthor::of`]), and, where it gave no `why=`,
    /// joined no request whose why was given and no fleet drive asked, the creator's standing
    /// reason copied once, under the creator's timeline lock alone (so the creator's timeline is
    /// never locked for a reason that would not be written). A later change of the creator's
    /// reason is the creator's to state.
    pub(crate) fn birth_of(
        &self,
        birth: &crate::session_log::Birth,
        by: &ExitActor,
        fleet: bool,
    ) -> crate::session_log::Born {
        let resolved = birth.from.as_ref().map(|from| self.resolved_from(from));
        let creator = resolved
            .as_ref()
            .and_then(|r| crate::session_log::sid_ident(&r.sid));
        let author = crate::session_log::BirthAuthor::of(by, creator, fleet);
        let copied = match resolved.as_ref().and_then(|r| r.ctx.as_ref()) {
            Some(ctx) if birth.matters.is_none() && birth.request_why.is_none() && !fleet => {
                let tl = ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let now_wall = tl.slog().now().wall_ms;
                aterm_slog::shown(tl.slog().log().state(), "matters", now_wall)
                    .map(|s| s.claim.value.clone())
            }
            _ => None,
        };
        crate::session_log::Born {
            author,
            creator: resolved.map(|r| r.sid),
            copied,
        }
    }

    /// The launch's first tab (session 0 of window 0, built before the App was): opened by
    /// whoever launched aterm (`by=human why=new-window`), or, where a restore manifest is to
    /// place it as its window's first leaf, by aterm's restore (`by=harness why=restored`), as
    /// a cold restore's other new shells are. Nothing for a shell a seamless update handed over.
    pub(crate) fn note_launch_tab(&self, restoring: bool) {
        let _act = restoring.then(|| ActScope::enter(ExitActor::Harness, "restored"));
        self.note_tab_open(0, crate::WindowId(0), false, None, "new-window");
    }

    /// The sessions of `tab`, restored into `wid`, that no open placed (a cold restore's new
    /// shell, a held or crash-restored pane) open now, by aterm's restore (`why=restored`),
    /// parked as the restore placed their tab. A session an open already placed (a window's
    /// bootstrap shell the leaf took, one a seamless update carried) keeps its own.
    pub(crate) fn note_restored_opens(&self, wid: crate::WindowId, tab: &crate::tab_model::Tab) {
        let _act = ActScope::enter(ExitActor::Harness, "restored");
        tab.root.visit(&mut |&view| {
            let Some(crate::tab_model::View::Terminal(terminal)) =
                self.view_store.get(view).copied()
            else {
                return;
            };
            let placed = self.pool.get(terminal.session).is_some_and(|session| {
                session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .slog()
                    .tab_placed()
            });
            if !placed {
                self.note_tab_open(terminal.session, wid, tab.parked, None, "restored");
            }
        });
    }

    /// THE KEEP CHOKE POINT: the worker tab holding `sessions` became the person's (Keep as Tab):
    /// each session of it writes its keep.
    pub(crate) fn note_tab_keep(&self, sessions: &[u64]) {
        let by = crate::session_log::exit_actor(&keep_actor());
        for local in sessions {
            let Some(ctx) = self.pool.get(*local).map(|s| Arc::clone(&s.ctx)) else {
                continue;
            };
            // Keeping a plain fleet shell adopts it without typing or raising a hold.
            crate::control::forget_driven_shell(&ctx.self_id);
            let mut tl = ctx
                .timeline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let at = tl.slog().now();
            tl.slog_mut().keep_tab(by, at);
        }
    }

    /// Every terminal session of `wid`'s tabs stands in `wid`, parked as its tab is: where a move,
    /// a restore's parking or a Keep as Tab put it, which its close names
    /// ([`crate::session_log::SessionLog::place_tab`]).
    pub(crate) fn settle_tab_places(&self, wid: crate::WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        for tab in ws.tab_set.tabs() {
            tab.root.visit(&mut |&view| {
                let Some(crate::tab_model::View::Terminal(terminal)) =
                    self.view_store.get(view).copied()
                else {
                    return;
                };
                let Some(session) = self.pool.get(terminal.session) else {
                    return;
                };
                session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .slog_mut()
                    .place_tab(wid.0, tab.parked);
            });
        }
    }

    /// Read session `local`'s process tree for its tab's close totals ([`tree_spent`]), while its
    /// terminal is still open: the last view of it is about to detach, or aterm quits. Nothing for
    /// a session no open placed.
    pub(crate) fn note_tab_spent(&self, local: u64) {
        let Some(session) = self.pool.get(local) else {
            return;
        };
        let placed = session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slog()
            .tab_placed();
        if !placed {
            return;
        }
        let spent = tree_spent(session.pid, &self.tree_probe);
        session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slog_mut()
            .note_tree_spent(spent);
    }

    /// AT A QUIT, every tab still placed closes (`act=close out=app-quit`) with its totals, by
    /// whoever asked for the quit ([`crate::App::quit_by`], `-` where nothing said): a quit hangs
    /// up its terminals past the close's choke point, so its tabs would otherwise end with no
    /// close. A seamless update's handover never comes here; its tabs go on in the successor, as
    /// the tabs of `carried` go on in the next launch, their programs left running in the keeper's
    /// custody (`App::carry_developer_tools_install_at_exit`), so they close nothing.
    ///
    /// Returns the row each session it closes would leave in the machine's session table
    /// (`out=app-quit`, [`crate::session_table::row_at_close`]): the run's end writes those of the
    /// sessions its layout does not bring back (`App::retire_tasks_at_quit`).
    pub(crate) fn close_tabs_at_quit(&self, carried: &[u64]) -> Vec<crate::session_table::Row> {
        let actor = self.quit_by.clone().unwrap_or(ExitActor::Unknown);
        let by = crate::session_log::exit_actor(&actor);
        let mut rows = Vec::new();
        for local in self.pool.ids() {
            if carried.contains(&local) {
                continue;
            }
            self.note_tab_spent(local);
            let Some((ctx, term)) = self
                .pool
                .get(local)
                .map(|s| (Arc::clone(&s.ctx), Arc::clone(&s.term)))
            else {
                continue;
            };
            let at_close = crate::session_table::AtClose::read(&term, &ctx);
            // A program that had exited left its code with the store, which a quit does not
            // deregister from (`SessionStore::note_exit_code`).
            let exit_code = self
                .store
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .noted_exit_code(local);
            let mut tl = ctx
                .timeline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let at = tl.slog().now();
            let task = task_end(&tl);
            let tab = tl.slog_mut().close_tab(
                crate::session_log::TabClose {
                    out: aterm_slog::SessionOut::AppQuit,
                    by,
                    why: None,
                    task: task.as_deref(),
                },
                at,
            );
            let ended = crate::session_store::Ended {
                local,
                out: crate::session_store::ExitReason::AppQuit,
                by: actor.clone(),
                why: None,
                exit_code,
                ended_ms: at.wall_ms,
                started_ms: tl.slog().opened_wall(),
                tab,
                ep: tl.slog().log().ep(),
                task: tl.task().cloned(),
            };
            drop(tl);
            rows.push(crate::session_table::row_at_close(
                &ctx.self_id,
                &ended,
                &at_close,
            ));
        }
        rows
    }

    /// THE QUIT'S CLOSES, in the order every close takes (`SessionStore::deregister_closing`):
    /// each tab still placed closes first, but the tabs of `carried`, whose programs the quit
    /// leaves to the keeper ([`Self::close_tabs_at_quit`]), then each live session's log
    /// closes `out=app-quit` (`session_log::close_logs_at_quit`), which on unix reaches the bus
    /// before the bridge hears `bye quit` (`fabric_launch::bye_at_quit`, fix unit Q1). A
    /// session's close is the last record its log takes (a mark after it is refused,
    /// `Refused::SessionClosed`), so a tab close written after it would be lost, from the
    /// journal and the bus alike.
    ///
    /// The rows the tabs' closes leave are noted for the run's end ([`Self::close_tabs_at_quit`],
    /// `task_launch::QuitClosed::note_row`).
    pub(crate) fn close_sessions_at_quit(&mut self, carried: &[u64]) {
        for row in self.close_tabs_at_quit(carried) {
            self.quit_closed_tasks.note_row(row);
        }
        #[cfg(unix)]
        crate::fabric_launch::bye_at_quit(&self.store);
        #[cfg(not(unix))]
        let _ = crate::session_log::close_logs_at_quit(&self.store);
    }
}

#[cfg(test)]
#[path = "tab_marks_tests.rs"]
mod tests;

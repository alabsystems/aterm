// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `end <token>…` (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.4, §6.1): ends each leftover
//! process the `sys left` view offers, named by its token, one at a time on the control worker.
//! Every signal goes through [`end_proven`], which reads the process's identity again just before
//! it and asks this module's re-proof: the one pid read and judged again on fresh state
//! ([`crate::vitals_left::reread`]), which must still offer the same token from the same source,
//! and, unless a run's folders vouch for it on both readings, the same executable. Nothing here
//! ends a row it was not named, ends anything by name or pattern, takes a pid without its token,
//! or ends another token once its caller hung up.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;

use aterm_messages::Message;
use aterm_uds::process::{EndControl, EndRefusal, Identity, ProcessPresence, end_proven};
use aterm_vitals::left::{LeftRow, LeftView, Proof, Source, State, Tier, Why};

use crate::message_reporters::{EndOutcome, end_record};
use crate::vitals_left::Reread;

/// The most tokens one request names.
pub(crate) const MAX_TOKENS: usize = 16;

/// The answer to a request that names anything but one to [`MAX_TOKENS`] distinct tokens.
pub(crate) const USAGE: &str = "ERR usage: end <token>… (each a `sys left` row's end= token, \
     each once, at most 16; there is no all, no wildcard and no pid without its token)\n";

/// Who asks: the request's target session, and whether the connection is owner-class.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Caller<'a> {
    /// An owner-class connection, which may end any row the view offers.
    pub(crate) owner: bool,
    /// The target session's key.
    pub(crate) key: u64,
    /// The target session's id.
    pub(crate) sid: &'a str,
}

/// What one request reads and acts through: the view it is judged against, the re-proof's read,
/// the kernel's signals, and where its records go. Live, [`cmd_end`] fills it; a test, fakes.
pub(crate) struct World<'a, C> {
    /// Tokens are read in this kernel's shape: with a pid version where it keeps one
    /// ([`aterm_uds::process::KEEPS_PID_VERSIONS`]).
    pub(crate) versioned: bool,
    /// The leftover view, fresh within the `left` view's freshness default; read only once the
    /// request parsed.
    pub(crate) view: &'a mut dyn FnMut() -> Option<Arc<LeftView>>,
    /// `pid` read again on fresh state and judged as a reading made now would judge it; `None`
    /// when it is gone or no candidate.
    pub(crate) reread: &'a dyn Fn(u32) -> Option<LeftView>,
    /// Current session/Signal grant and hold permission, checked again before each signal.
    /// The callback releases every host lock before returning.
    pub(crate) authorized: &'a dyn Fn() -> bool,
    /// Whether any process holds `pid`, for a token the view has no row of.
    pub(crate) presence: &'a dyn Fn(i32) -> ProcessPresence,
    /// The identity reads and signals [`end_proven`] makes.
    pub(crate) control: C,
    /// How long a `SIGTERM` is given before the `SIGKILL` ([`aterm_uds::process::END_GRACE`]).
    pub(crate) grace: Duration,
    /// Now, unix ms, for the records' words.
    pub(crate) now_ms: u64,
    /// Where each signalled end's `proc.end` record goes.
    pub(crate) post: &'a mut dyn FnMut(Message),
    /// Whether the caller hung up: no token after it is ended
    /// ([`aterm_control::hangup::caller_hung_up`]).
    pub(crate) hung_up: &'a dyn Fn() -> bool,
}

/// Why a token was not ended: §6.1's closed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The process the token named is gone, or its identity or executable moved.
    Stale,
    /// Nothing read proves it a leftover `end` may end.
    NotProven,
    /// macOS's program check holds it.
    Held,
    /// It is in a live session's foreground group: `signal`'s to end.
    Foreground,
    /// It is daemon-shaped.
    Daemon,
    /// Another user's process holds the pid.
    AnotherUid,
    /// It must never be ended from here, or the kernel will not let aterm signal it.
    Never,
}

impl Refused {
    /// The `why=` word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::NotProven => "not-proven",
            Self::Held => "held",
            Self::Foreground => "foreground",
            Self::Daemon => "daemon",
            Self::AnotherUid => "another-uid",
            Self::Never => "never",
        }
    }
}

/// What became of one token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// Signalled: `SIGTERM`, and a `SIGKILL` when `killed`.
    Ended(EndOutcome),
    /// Not signalled.
    Refused(Refused),
}

/// What a fresh judgment says of the row an end set out to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proved {
    /// It still offers the same token from the same source (and executable).
    Holds,
    /// Its identity or its executable moved: another program now.
    Moved,
    /// Its tier or source is lost, or it is no candidate now.
    Lost,
}

/// The tokens `rest` names, each with its identity in this kernel's shape (`versioned`); `None`
/// for anything else: no word, a word that is no token (`all`, `*`, a bare pid, a pattern), a
/// token named twice, or more than [`MAX_TOKENS`].
fn tokens(rest: &str, versioned: bool) -> Option<Vec<(&str, Identity)>> {
    let mut named: Vec<(&str, Identity)> = Vec::new();
    for word in rest.split_whitespace() {
        let id = Identity::parse_shaped(word, versioned)?;
        if named.len() == MAX_TOKENS || named.iter().any(|(_, seen)| *seen == id) {
            return None;
        }
        named.push((word, id));
    }
    (!named.is_empty()).then_some(named)
}

/// Whether `caller` may end `row` of `view`: an owner-class connection any row; a Signal edge only
/// a row whose session is its target (W-session's live tab, or a verify run whose `sid=` it is).
/// A closed tab's rows, and rows nothing ties, are the owner's.
fn mine(caller: &Caller<'_>, view: &LeftView, row: &LeftRow) -> bool {
    caller.owner
        || match &row.from {
            Source::Tab(key) => *key == caller.key,
            Source::Run(id) => {
                !caller.sid.is_empty()
                    && view
                        .runs
                        .iter()
                        .any(|run| run.id == *id && run.sid.as_deref() == Some(caller.sid))
            }
            Source::Closed(_) | Source::Unknown => false,
        }
}

/// Why `row`, which offers no token, is not ended, from its facts (§6.1's closed set).
fn refusal_of(row: &LeftRow) -> Refused {
    if row.tier == Tier::Never {
        if row.foreground {
            Refused::Foreground
        } else {
            Refused::Never
        }
    } else if row.state == State::ProgramCheck {
        Refused::Held
    } else if row.why == Why::Daemon {
        Refused::Daemon
    } else {
        Refused::NotProven
    }
}

/// Whether `fresh`, the row read and judged again now, still proves what `offered` offered: the
/// same token, a tier an end takes, the same source, and the same executable unless C-tree
/// vouches for it on both readings (§4.4 step 1: where the kernel keeps no pid version an exec
/// keeps the identity, and only the run's roots or the executable say it moved). The judge decides
/// what proves it: a witness lost while another still does (a dead run's member whose group leader
/// was ended first keeps W-run) holds.
fn proved(offered: &LeftRow, fresh: Option<&LeftRow>) -> Proved {
    let Some(fresh) = fresh else {
        return Proved::Lost;
    };
    if fresh.token != offered.token {
        return Proved::Moved;
    }
    let tree = |row: &LeftRow| row.proof.contains(&Proof::Tree);
    if !(tree(offered) && tree(fresh))
        && (fresh.exe_judged.is_none() || fresh.exe_judged != offered.exe_judged)
    {
        return Proved::Moved;
    }
    if matches!(fresh.tier, Tier::Offer | Tier::Auto) && fresh.from == offered.from {
        Proved::Holds
    } else {
        Proved::Lost
    }
}

/// Why a token the view has no row of is refused: its process lives and is this user's, but no
/// reading offers it (`not-proven`); another user's process holds the pid (`another-uid`); or
/// the process the token named is gone (`stale`).
fn absent<C: EndControl>(
    id: Identity,
    control: &mut C,
    presence: &dyn Fn(i32) -> ProcessPresence,
) -> Refused {
    match control.identity(id.pid) {
        Some(now) if now == id => Refused::NotProven,
        Some(_) => Refused::Stale,
        None if presence(id.pid) == ProcessPresence::Present => Refused::AnotherUid,
        None => Refused::Stale,
    }
}

/// Ends the one process `token` names, as `view`'s `row` offers it (`None`: no row of its pid).
fn end_one<C: EndControl>(
    caller: &Caller<'_>,
    token: &str,
    id: Identity,
    view: &LeftView,
    row: Option<&LeftRow>,
    w: &mut World<'_, C>,
) -> Outcome {
    let Some(row) = row else {
        return Outcome::Refused(absent(id, &mut w.control, w.presence));
    };
    if row.token.as_deref() != Some(token) {
        // A row offered no token is refused for its facts, once the kernel says it is still the
        // process the token names; any other is another process by now.
        return Outcome::Refused(
            if row.token.is_none() && w.control.identity(id.pid) == Some(id) {
                refusal_of(row)
            } else {
                Refused::Stale
            },
        );
    }
    if !matches!(row.tier, Tier::Offer | Tier::Auto) {
        return Outcome::Refused(refusal_of(row));
    }
    let reread = w.reread;
    let authorized = w.authorized;
    let moved = Cell::new(false);
    // The row as the last re-proof that held judged it: what the record says was ended.
    let proven: RefCell<Option<(LeftRow, LeftView)>> = RefCell::new(None);
    let still = |now: &Identity| {
        let fresh = u32::try_from(now.pid).ok().and_then(reread);
        let fresh_row = fresh
            .as_ref()
            .and_then(|view| view.rows.iter().find(|r| r.pid == row.pid));
        let allowed = authorized()
            && fresh
                .as_ref()
                .zip(fresh_row)
                .is_some_and(|(view, row)| mine(caller, view, row));
        let verdict = if allowed {
            proved(row, fresh_row)
        } else {
            Proved::Lost
        };
        moved.set(verdict == Proved::Moved);
        if verdict == Proved::Holds {
            *proven.borrow_mut() = fresh_row.cloned().zip(fresh);
        }
        verdict == Proved::Holds
    };
    match end_proven(id, w.grace, &mut w.control, still) {
        Ok(ended) if ended.termed => {
            let outcome = EndOutcome {
                killed: ended.killed,
                gone: ended.gone,
            };
            let proven = proven.into_inner();
            let (record_row, record_view) = proven.as_ref().map_or((row, view), |(r, v)| (r, v));
            (w.post)(end_record(record_row, record_view, outcome, w.now_ms));
            Outcome::Ended(outcome)
        }
        Ok(_) | Err(EndRefusal::Stale) => Outcome::Refused(Refused::Stale),
        Err(EndRefusal::NotProven) => Outcome::Refused(if moved.get() {
            Refused::Stale
        } else {
            Refused::NotProven
        }),
        Err(EndRefusal::SignalFailed) => {
            Outcome::Refused(if w.control.identity(id.pid) == Some(id) {
                Refused::Never
            } else {
                Refused::Stale
            })
        }
    }
}

/// The host's current roster, independent of the cached offer. Closed-window birth/horizon
/// certificates are immutable historical evidence, usable only while the authoritative close
/// index still confirms that exact key and sid, or, for one an earlier aterm recorded
/// ([`aterm_vitals::left::ClosedTab::restored`]), whose close this process's index never held,
/// under a key no session of this process has. A newly closed session with no certificate is
/// unknown, never kept live by an older offer.
fn fresh_context(
    store: &crate::session_store::Store,
    offered: &LeftView,
    readers: &crate::vitals_left::LeftReaders,
) -> Option<HostContext> {
    fresh_context_with(
        store,
        offered,
        &|identity| {
            let before = readers.kernel.birth(identity.pid)?;
            if !identity.matches_kernel_birth(identity.pid, before.0) {
                return None;
            }
            let lineage = readers.kernel.lineage(identity.pid)?;
            (readers.kernel.birth(identity.pid)? == before).then_some(lineage.uniqueid)
        },
        &|handle| {
            // The cloned handle's sink owns this master throughout the read; no fd number is
            // borrowed from a closed/reused session. No store or metadata guard is held here.
            u32::try_from(crate::quit_safety::foreground_pgrp(
                handle.ctx.sink.master(),
            ))
            .ok()
            .filter(|group| *group > 0)
        },
    )
}

type HostContext = (
    Vec<crate::session_store::SessionHandle>,
    Vec<aterm_vitals::left::LiveTab>,
    Vec<aterm_vitals::left::ClosedTab>,
);

fn fresh_context_with(
    store: &crate::session_store::Store,
    offered: &LeftView,
    shell: &dyn Fn(aterm_pty::ShellIdentity) -> Option<u64>,
    foreground: &dyn Fn(&crate::session_store::SessionHandle) -> Option<u32>,
) -> Option<HostContext> {
    let (handles, closed) = {
        let current = store.read().unwrap_or_else(|p| p.into_inner());
        let closed = offered
            .closed
            .iter()
            .filter(|certificate| {
                certificate.restored
                    || current.closed_logs().exits_since(0).any(|exit| {
                        exit.local_id == certificate.key && exit.sid.as_str() == certificate.sid
                    })
            })
            .cloned()
            .collect();
        (current.snapshot(), closed)
    };
    let mut live = Vec::with_capacity(handles.len());
    for handle in &handles {
        let shell_pid = u32::try_from(handle.shell_identity.pid)
            .ok()
            .filter(|pid| *pid > 0)?;
        let uniqueid = shell(handle.shell_identity);
        let agent_spans = offered
            .live
            .iter()
            .find(|old| {
                old.key == handle.local_id
                    && old.sid == handle.sid.as_str()
                    && old.shell_pid == shell_pid
                    && uniqueid.is_some()
                    && old.shell_uniqueid == uniqueid
            })
            .map(|old| old.agent_spans.clone())
            .unwrap_or_default();
        live.push(aterm_vitals::left::LiveTab {
            key: handle.local_id,
            sid: handle.sid.as_str().to_owned(),
            shell_pid,
            shell_uniqueid: uniqueid,
            fg_pgid: Some(foreground(handle)?),
            agent_spans,
        });
    }
    same_roster(store, &handles).then_some((handles, live, closed))
}

fn same_roster(
    store: &crate::session_store::Store,
    handles: &[crate::session_store::SessionHandle],
) -> bool {
    let current = store.read().unwrap_or_else(|p| p.into_inner());
    let snapshot = current.snapshot();
    snapshot.len() == handles.len()
        && handles.iter().zip(&snapshot).all(|(old, now)| {
            now.local_id == old.local_id
                && now.sid == old.sid
                && now.nonce == old.nonce
                && now.shell_identity == old.shell_identity
                && now.state == old.state
                && Arc::ptr_eq(&now.ctx, &old.ctx)
        })
}

type CandidateRead<'a> =
    dyn Fn(&[aterm_vitals::left::LiveTab], &[aterm_vitals::left::ClosedTab]) -> Reread + 'a;

/// Bracket expensive process/record reads with the host's current shell and foreground
/// context. A roster check alone misses a foreground handoff on the same live PTY. A context
/// that could not be captured, or that changed, makes the read [`Reread::Unread`], which refuses
/// a signal; every capture and read releases its guards first.
fn reread_current(
    store: &crate::session_store::Store,
    capture: &dyn Fn() -> Option<HostContext>,
    read: &CandidateRead<'_>,
) -> Reread {
    let Some((handles, live, closed)) = capture() else {
        return Reread::Unread;
    };
    let fresh = read(&live, &closed);
    let Some((_, after_live, after_closed)) = capture() else {
        return Reread::Unread;
    };
    if live == after_live && closed == after_closed && same_roster(store, &handles) {
        fresh
    } else {
        Reread::Unread
    }
}

/// Answers `end <token>…` for `caller` in `w`: every token parsed and gated before any signal,
/// then each ended or refused in the order named, until the caller hangs up: the reply then
/// stops at the tokens taken before it did.
pub(crate) fn run<C: EndControl>(caller: &Caller<'_>, rest: &str, mut w: World<'_, C>) -> String {
    let Some(named) = tokens(rest, w.versioned) else {
        return USAGE.to_owned();
    };
    let Some(view) = (w.view)() else {
        return "ERR unavailable end: no leftover reading (sys left)\n".to_owned();
    };
    let row_of = |id: &Identity| {
        u32::try_from(id.pid)
            .ok()
            .and_then(|pid| view.rows.iter().find(|r| r.pid == pid))
    };
    if let Some((token, _)) = named
        .iter()
        .find(|(_, id)| row_of(id).is_some_and(|row| !mine(caller, &view, row)))
    {
        return format!("ERR denied end: {token} is not this session's\n");
    }
    let (mut term, mut gone, mut killed, mut refused) = (0, 0, 0, 0);
    let mut lines = Vec::with_capacity(named.len());
    for (token, id) in &named {
        if (w.hung_up)() {
            break;
        }
        match end_one(caller, token, *id, &view, row_of(id), &mut w) {
            Outcome::Ended(o) => {
                term += 1;
                gone += usize::from(o.gone);
                killed += usize::from(o.killed);
                let signal = if o.killed { "kill" } else { "term" };
                lines.push(format!(
                    "ended {token} signal={signal} gone={}",
                    u8::from(o.gone)
                ));
            }
            Outcome::Refused(why) => {
                refused += 1;
                lines.push(format!("refused {token} why={}", why.word()));
            }
        }
    }
    format!(
        "OK {} end term={term} gone={gone} killed={killed} refused={refused}\n{}\n",
        lines.len(),
        lines.join("\n")
    )
}

/// `end` on the control socket for `caller`: the view as fresh as `sys left`'s default asks
/// (pulled when older), each re-proof a fresh read of the one pid ([`crate::vitals_left::reread`]),
/// the kernel's signals ([`aterm_uds::process::EndControlLive`]), and each record posted to the
/// window's message center.
pub(crate) fn cmd_end(
    caller: &Caller<'_>,
    rest: &str,
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    store: &crate::session_store::Store,
    authorized: &dyn Fn() -> bool,
) -> String {
    use std::cell::RefCell;
    let readers = crate::vitals_left::LeftReaders::kernel();
    let read: RefCell<Option<Arc<LeftView>>> = RefCell::new(None);
    let now_ms = || aterm_time::unix_millis().unwrap_or(0);
    let mut view = || {
        let fresh =
            aterm_vitals::sys::SysQuery::parse("left").map_or(2_000, |q| q.freshness_default_ms());
        let (held, _) = crate::sys_host::SYS.fresh_left(Duration::from_millis(fresh), false);
        let snap = held?.snap;
        let left = snap.left.clone()?;
        *read.borrow_mut() = Some(Arc::clone(&left));
        Some(left)
    };
    let reread = |pid: u32| {
        let got = read.borrow();
        let offered = got.as_ref()?;
        reread_current(
            store,
            &|| fresh_context(store, offered, &readers),
            &|live, closed| {
                crate::vitals_left::reread(
                    &readers,
                    live,
                    closed,
                    std::process::id(),
                    pid,
                    now_ms(),
                )
            },
        )
        .view()
    };
    let mut post = |record: Message| {
        let _ = proxy.send_event(crate::Wake::Record(record));
    };
    run(
        caller,
        rest,
        World {
            versioned: aterm_uds::process::KEEPS_PID_VERSIONS,
            view: &mut view,
            reread: &reread,
            authorized,
            presence: &aterm_uds::process::process_presence,
            control: aterm_uds::process::EndControlLive,
            grace: aterm_uds::process::END_GRACE,
            now_ms: now_ms(),
            post: &mut post,
            hung_up: &super::caller_hung_up,
        },
    )
}

/// THE AGENT'S LEFT ROWS OF SESSION `sid` AT A YIELD (the harness's `IdleHost::leftover`, read
/// where the repository is): of `view`, the rows [`aterm_vitals::left::agents_left_read`] gives
/// `sid` (the ones `status left=` counts), each re-proved as `end` re-proves a row before its
/// signal ([`proved`]: the same token, a tier an end takes, the same source and, unless C-tree
/// vouches for it on both readings, the same executable) on `reread`'s fresh read and judgment of
/// its one pid, and still `sid`'s agent's there. A row that does not re-prove holds nothing: a
/// moved identity is another process, and a lost one is gone or no longer the agent's. No view, a
/// partial one (which can say a row is left but never that none is), or a row whose read could
/// not be made ([`Reread::Unread`]: whether it still runs is unknown) is unchecked.
pub(crate) fn left_at_yield(
    view: Option<&LeftView>,
    sid: &str,
    reread: &dyn Fn(u32) -> Reread,
) -> aterm_agent::harness::leftover::LeftRead {
    use aterm_agent::harness::leftover::{LeftRead, token_of};
    let Some(rows) = view.and_then(|view| aterm_vitals::left::agents_left_read(view, sid)) else {
        return LeftRead::Unchecked;
    };
    let mut held = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(token) = row.token.as_deref().and_then(token_of) else {
            continue;
        };
        let fresh = match reread(row.pid) {
            Reread::Unread => return LeftRead::Unchecked,
            Reread::Read(None) => continue,
            Reread::Read(Some(fresh)) => fresh,
        };
        let again = aterm_vitals::left::agents_left(&fresh, sid)
            .into_iter()
            .find(|r| r.pid == row.pid);
        if proved(row, again) == Proved::Holds {
            held.push(token);
        }
    }
    LeftRead::Rows(held)
}

/// THE LEFT VIEW A YIELD READS of what `SYS.fresh` answered: only a fresh snapshot's
/// ([`aterm_vitals::sys::Freshness::Fresh`]). A stale one (the sampler did not refresh it in time,
/// or stopped) or a warming one predates any row born since, so it says nothing of the yield.
fn fresh_left(
    (held, freshness): (Option<crate::sys_host::Held>, aterm_vitals::sys::Freshness),
) -> Option<Arc<LeftView>> {
    held.filter(|_| freshness == aterm_vitals::sys::Freshness::Fresh)
        .and_then(|held| held.snap.left.clone())
}

/// [`left_at_yield`] live for session `sid`: the view as fresh as `sys left`'s default asks
/// (pulled when older; none unless fresh, [`fresh_left`]), each row re-proved on a fresh read of
/// its one pid against the host's freshly captured tabs ([`reread_current`], [`fresh_context`]),
/// as [`cmd_end`] re-proves each before a signal. It ends nothing.
pub(crate) fn agents_left_now(
    store: &crate::session_store::Store,
    sid: &str,
) -> aterm_agent::harness::leftover::LeftRead {
    let readers = crate::vitals_left::LeftReaders::kernel();
    let fresh =
        aterm_vitals::sys::SysQuery::parse("left").map_or(2_000, |q| q.freshness_default_ms());
    let Some(view) =
        fresh_left(crate::sys_host::SYS.fresh_left(Duration::from_millis(fresh), false))
    else {
        return aterm_agent::harness::leftover::LeftRead::Unchecked;
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let reread = |pid: u32| {
        reread_current(
            store,
            &|| fresh_context(store, &view, &readers),
            &|live, closed| {
                crate::vitals_left::reread(&readers, live, closed, std::process::id(), pid, now_ms)
            },
        )
    };
    left_at_yield(Some(&view), sid, &reread)
}

#[cfg(test)]
#[path = "control_end_tests.rs"]
mod tests;

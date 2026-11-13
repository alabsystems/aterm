// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's looks at its requests (ATERM-KI-1109-3, step 3 of the requests design): each
//! minute, on the reaper's tick, a fleet drive's leased plain shells, whose request concludes once
//! its requester has not driven it for [`aterm_requests::LEASE_IDLE_MS`] with nothing running in
//! it, or once its requester's Mac left the fleet and it is idle; and once per process, after its
//! restore, the requests a crash left open whose tabs no aterm hosts now. Each look reads the
//! index ([`crate::requests_host::placed`]) and the leased tabs' shells only, and hands what it
//! found to the requests' writer thread, which reads any file it needs (the relay folder that
//! says this window is alone); a concluded lease leaves its pane standing.
//!
//! While a request lives (step 4), or a claim no person has touched stands, the same look reads
//! every live tab's shell blocks since its last (a bounded read per tab, a running block again
//! until it ends) and when a person last typed in it, and hands them to the writer, which judges
//! the folders a request's commands made and a person's later work in them
//! ([`crate::requests_footprint::plan`]); and it has each open request tab's process tree sampled
//! on the footprint thread. Neither, it reads no terminal.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::App;
use crate::requests_footprint::{BlockSpan, FsProbe, MeasureProbe, Places};

/// One look's read of a tab's shell blocks ([`LeaseClock::spans`]): the spans to hand, and the
/// first block the next look reads.
pub(crate) struct SpanRead {
    pub(crate) spans: Vec<BlockSpan>,
    pub(crate) next: u64,
}

/// What a lease's look reads of the host, injected so a test sets its clock and its shells.
#[derive(Clone, Copy)]
pub(crate) struct LeaseClock {
    /// How long a leased shell stands undriven and idle before its request concludes.
    pub(crate) idle_after: Duration,
    /// Whether something runs in a session's shell: on the host, a command in its foreground.
    /// Its children cannot say, since a prompt's helpers (a theme's git status daemon) are the
    /// children of a shell standing idle, and a lease's end stops nothing that runs.
    pub(crate) runs: fn(&crate::Session) -> bool,
    /// Whether this process is the only aterm on its state home that hosts tabs, which a
    /// relaunch sweep needs: another's tabs are not this one's to judge gone. It reads the
    /// filesystem, so the requests' writer thread asks it, never the main thread.
    pub(crate) alone: fn() -> bool,
    /// A session's shell blocks from block id `from`, those that ended before `since_ms` (Unix
    /// ms) skipped: at most [`crate::session_status::BLOCK_READ_MAX`], and never past a block
    /// still running or not yet run.
    pub(crate) spans: fn(&crate::Session, u64, u64) -> SpanRead,
    /// When a person last typed in a session, Unix ms.
    pub(crate) typed: fn(&crate::Session, u64) -> Option<u64>,
    /// This Mac's home, whose direct children a birth scan lists, and aterm's state home, which
    /// no scan claims.
    pub(crate) places: fn() -> Places,
    /// The filesystem reads a birth scan makes.
    pub(crate) fs: FsProbe,
    /// The reads a measure of a claimed folder makes.
    pub(crate) measure: MeasureProbe,
}

impl LeaseClock {
    /// This host's: 30 minutes, the shell's own word on its foreground, and the other windows
    /// this Mac's fleet relays name.
    pub(crate) const HOST: Self = Self {
        idle_after: Duration::from_millis(aterm_requests::LEASE_IDLE_MS),
        runs: App::session_busy,
        alone: sole_window_on_state,
        spans: host_spans,
        typed: host_typed,
        places: Places::host,
        fs: FsProbe::HOST,
        measure: MeasureProbe::HOST,
    };

    /// A headless test app's: every shell runs and another window may live, so no look ends a
    /// lease or sweeps, and none reads this process's relay folder, until a test says otherwise.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) const INERT: Self = Self {
        idle_after: Duration::from_millis(aterm_requests::LEASE_IDLE_MS),
        runs: |_| true,
        alone: || false,
        spans: |_, from, _| SpanRead {
            spans: Vec::new(),
            next: from,
        },
        typed: |_, _| None,
        places: Places::default,
        fs: FsProbe {
            list: |_| Vec::new(),
            born: |_| None,
        },
        measure: MeasureProbe::INERT,
    };
}

/// Whether no other live aterm window shares this one's state home, as the folder of its fleet
/// relays says (every window with a control socket keeps one there); `false` where it cannot say
/// (a folder it cannot read, or one without this window's own relay), which a non-Unix build
/// never can.
fn sole_window_on_state() -> bool {
    #[cfg(unix)]
    {
        crate::fleet_listen::node_serve::other_live_windows() == Some(0)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The host's read of a session's shell blocks ([`LeaseClock::spans`]): each block that ran a
/// command, from its start to its end (a block left executing when the next block came ends at
/// that block's prompt, else where its input or command began), with when its input began beside
/// it. The read stops at the block still running, or one not yet run, which the next look reads
/// again.
fn host_spans(session: &crate::Session, from: u64, since_ms: u64) -> SpanRead {
    use aterm_types::BlockState;
    let term = crate::term_lock(&session.term);
    let current = term.current_block().map(|block| block.id);
    let mut past: Vec<&aterm_types::OutputBlock> = term
        .all_blocks()
        .rev()
        .take_while(|block| block.id >= from)
        .collect();
    past.reverse();
    let mut read = SpanRead {
        spans: Vec::new(),
        next: from,
    };
    for (at, block) in past.iter().enumerate() {
        if read.spans.len() >= crate::session_status::BLOCK_READ_MAX {
            break;
        }
        let next_prompt = past.get(at + 1).and_then(|next| {
            next.prompt_time_ms
                .or(next.command_input_start_time_ms)
                .or(next.command_exec_start_time_ms)
        });
        let current_block = Some(block.id) == current;
        let Some(start) = block.command_exec_start_time_ms else {
            if current_block {
                break;
            }
            read.next = block.id + 1;
            continue;
        };
        let to_ms = match block.state {
            BlockState::Complete => block.command_end_time_ms.or(next_prompt),
            BlockState::Executing if current_block => None,
            _ => next_prompt,
        };
        let ended = to_ms.is_some() || !current_block;
        if !to_ms.is_some_and(|to| to < since_ms) {
            read.spans.push(BlockSpan {
                sid: String::new(),
                blk: block.id,
                cwd: block.working_directory.as_deref().map(str::to_owned),
                input_ms: block.command_input_start_time_ms,
                span: aterm_requests::claim::Span {
                    from_ms: start,
                    to_ms,
                },
            });
        }
        if !ended {
            break;
        }
        read.next = block.id + 1;
    }
    read
}

/// When a person last typed in `session` ([`crate::human_input`]), as Unix ms at `now_ms`.
fn host_typed(session: &crate::Session, now_ms: u64) -> Option<u64> {
    let ago = session
        .ctx
        .human_input
        .typed_ms_since(crate::metrics::now_us())?;
    Some(now_ms.saturating_sub(ago))
}

impl App {
    /// The minute's look at this process's requests at `now_ms` (Unix ms): the leased plain
    /// shells' lifetime, a living request's footprint, and once the restore settled, the
    /// relaunch sweep.
    pub(crate) fn look_at_requests(&mut self, now_ms: u64) {
        let handles: Vec<crate::session_store::SessionHandle> = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store.live_handles().cloned().collect()
        };
        self.look_at_footprints(&handles, now_ms);
        let mut tabs = BTreeMap::new();
        for handle in &handles {
            // Only the tab the request names: a shell a cold restore stood in for it is no lease
            // (its request is the sweep's).
            let leased =
                crate::requests_host::placed(handle.sid.as_str(), None).is_some_and(|placed| {
                    placed.via == aterm_requests::Via::FleetShell && placed.end.is_none()
                });
            if !leased {
                continue;
            }
            let Some(session) = self.pool.get(handle.local_id) else {
                continue;
            };
            let quiet_since_ms = {
                let term = crate::term_lock(&session.term);
                crate::quit_safety::shell_word(&term).idle_since_ms
            };
            tabs.insert(
                handle.sid.as_str().to_owned(),
                aterm_requests::TabNow {
                    idle: !(self.lease.runs)(session),
                    quiet_since_ms,
                },
            );
        }
        if !tabs.is_empty() {
            let answered = self
                .fleet_glance
                .as_ref()
                .and_then(|glance| crate::fleet_glance::answered_nodes(&glance.copy(), now_ms));
            crate::requests_host::lease_look(
                now_ms,
                aterm_requests::Look {
                    idle_after_ms: u64::try_from(self.lease.idle_after.as_millis())
                        .unwrap_or(u64::MAX),
                    tabs,
                    answered,
                },
            );
        }
        // Each minute until the writer thread found this window alone and swept: it, not this
        // thread, reads the relay folder that says so.
        if !self.requests_swept.load(Ordering::Acquire)
            && !self.restore_pass_queued()
            && !self.incoming_handoff_pending
        {
            let hosted: BTreeSet<String> = handles
                .iter()
                .map(|handle| handle.sid.as_str().to_owned())
                .collect();
            crate::requests_host::sweep(crate::requests_host::Sweep {
                at: now_ms,
                born_ms: self.requests_born_ms,
                hosted,
                alone: self.lease.alone,
                swept: Arc::clone(&self.requests_swept),
            });
        }
    }

    /// While a request lives, or concluded within the last two looks, or a claim stands that no
    /// person has touched (a later cleanup could act on it, so a person's work there must be
    /// seen): each live tab's shell blocks since the last look and its person's typing, handed to
    /// the requests' writer, and each open request tab's tree sampled. Otherwise no terminal is
    /// read and the cursors are dropped, so the next look starts from its tabs' recent blocks.
    fn look_at_footprints(&mut self, handles: &[crate::session_store::SessionHandle], now_ms: u64) {
        const RECENT_MS: u64 = 2 * 60_000;
        if !crate::requests_host::any_live(now_ms.saturating_sub(RECENT_MS))
            && !crate::requests_host::watching()
        {
            self.request_spans.clear();
            return;
        }
        let since_ms = now_ms.saturating_sub(aterm_requests::claim::SPAN_RING_MS);
        let mut spans = Vec::new();
        let mut typed = std::collections::HashMap::new();
        let mut sample = Vec::new();
        let mut cursors = std::collections::HashMap::new();
        for handle in handles {
            let Some(session) = self.pool.get(handle.local_id) else {
                continue;
            };
            let sid = handle.sid.as_str();
            let from = self.request_spans.get(sid).copied().unwrap_or(0);
            let read = (self.lease.spans)(session, from, since_ms);
            cursors.insert(sid.to_owned(), read.next);
            spans.extend(read.spans.into_iter().map(|span| BlockSpan {
                sid: sid.to_owned(),
                ..span
            }));
            if let Some(at) = (self.lease.typed)(session, now_ms) {
                typed.insert(sid.to_owned(), at);
            }
            if crate::requests_host::placed(sid, None).is_some_and(|placed| placed.end.is_none()) {
                sample.push((sid.to_owned(), session.pid));
            }
        }
        // A tab that left takes its cursor with it.
        self.request_spans = cursors;
        crate::requests_footprint::sample(sample, self.tree_probe);
        if !spans.is_empty() {
            crate::requests_host::spans(crate::requests_footprint::Hand {
                at: now_ms,
                places: (self.lease.places)(),
                fs: self.lease.fs,
                measure: self.lease.measure,
                spans,
                typed,
            });
        }
    }
}

#[cfg(test)]
#[path = "app_requests_tests.rs"]
mod tests;

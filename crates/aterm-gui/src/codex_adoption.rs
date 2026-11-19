// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which managed Codex image this foreground claimant actually runs, sampled off the window thread.

use std::sync::{Arc, PoisonError, Weak};
use std::time::Instant;

use aterm_sysprobe::volumes::{FileId, file_id};
use aterm_uds::process::LoadedImage;

use crate::self_claim::Proven;
use crate::session_timeline::SessionMeta;

/// The immutable entry of one numbered Codex install. Product versions need not be unique.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) build: u64,
    pub(crate) file: FileId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    pub(crate) running: Option<Entry>,
    pub(crate) selected: Option<Entry>,
}

impl Reading {
    pub(crate) fn fields(&self) -> String {
        let state = match (self.running, self.selected) {
            (Some(a), Some(b)) if a == b => "matched",
            (Some(_), Some(_)) => "differs",
            _ => "unknown",
        };
        let build =
            |entry: Option<Entry>| entry.map_or_else(|| "-".into(), |e| e.build.to_string());
        format!(
            "codex_adoption={state} codex_running_build={} codex_selected_build={}",
            build(self.running),
            build(self.selected)
        )
    }
}

/// Resolve only a kernel-proven Codex foreground claimant, including its original unscoped claim.
pub(crate) fn claim(meta: &SessionMeta, now_us: u64) -> Option<&Proven> {
    let claim = meta.live_claim(now_us)?;
    let pid = crate::self_claim::holder_pid(&claim.holder)?;
    claim
        .proven
        .as_ref()
        .or_else(|| claim.scoped.as_ref().map(|s| &s.proven))
        .filter(|p| p.pid == pid)
}

/// Capture the same admitted route as the package resolver. A split publication,
/// unreadable ready marker or development link has no managed selection here.
fn selected(layout: &atpkg::Layout) -> Option<Entry> {
    let (build, path) = atpkg::lookup::selected_alab_entry(layout, "codex")?;
    Some(Entry {
        build,
        file: file_id(&path, true)?,
    })
}

/// An image matches a build only if exactly one complete install owns that immutable file.
/// In particular two hard-linked installs do not prove which build began this run.
fn mapped(layout: &atpkg::Layout, id: FileId) -> Option<Entry> {
    let mut matches = atpkg::ops::list_installed_for(layout, "codex")
        .into_iter()
        .filter_map(|build| {
            let path = atpkg::lookup::ready_alab_entry(layout, "codex", build)?;
            (file_id(&path, true) == Some(id)).then_some(Entry { build, file: id })
        });
    let one = matches.next()?;
    matches.next().is_none().then_some(one)
}

fn loaded(pid: u32) -> Option<FileId> {
    let image = LoadedImage::read(i32::try_from(pid).ok()?)?.file_identity()?;
    Some(FileId {
        dev: image.device,
        ino: u128::from(image.inode),
    })
}

/// Bracket the reads with fresh process and selection observations. Only a stable side
/// has a build reading; a selection or image in motion cannot keep a previous match current.
fn sample(layout: Option<&atpkg::Layout>, image: impl Fn() -> Option<FileId>) -> Reading {
    let before = image();
    let target = layout.and_then(selected);
    let running = before.and_then(|id| mapped(layout?, id));
    let mapped_after = image();
    let after = layout.and_then(selected);
    Reading {
        running: running.filter(|_| mapped_after == before),
        selected: target.filter(|_| after == target),
    }
}

/// The deadline is tracked by the ordinary session observation scheduler even at rest.
#[derive(Debug)]
struct Scheduled {
    revision: u64,
    foreground: u64,
    next: Instant,
}

/// A read already queued may no longer belong to the process or foreground run it was queued for.
pub(crate) struct Job {
    ctx: Weak<crate::SessionCtx>,
    source: Proven,
    revision: u64,
    foreground: u64,
}

impl Job {
    fn due(
        scheduled: &mut Option<Scheduled>,
        ctx: &Arc<crate::SessionCtx>,
        now: Instant,
    ) -> Option<Self> {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(source) = claim(&meta, crate::metrics::now_us()) else {
            *scheduled = None;
            return None;
        };
        let revision = meta.supervisor_revision;
        let foreground = ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .adoption_generation();
        if scheduled
            .as_ref()
            .is_some_and(|s| s.revision == revision && s.foreground == foreground && now < s.next)
        {
            return None;
        }
        *scheduled = Some(Scheduled {
            revision,
            foreground,
            next: now + crate::codex_rollout_tap::REST_FAR,
        });
        Some(Self {
            ctx: Arc::downgrade(ctx),
            source: source.clone(),
            revision,
            foreground,
        })
    }

    pub(crate) fn run(self, master: Master) {
        #[cfg(unix)]
        use std::os::fd::AsRawFd as _;
        #[cfg(unix)]
        let handle = master.as_raw_fd();
        #[cfg(not(unix))]
        let handle = master;
        self.run_with(
            Proven::alive,
            || crate::quit_safety::foreground_pgrp(handle),
            || atpkg::store::resolve_for_lookup().ok().flatten(),
            loaded,
        );
    }

    fn run_with(
        self,
        alive: impl Fn(&Proven) -> bool,
        group: impl Fn() -> i32,
        layout: impl FnOnce() -> Option<atpkg::Layout>,
        image: impl Fn(u32) -> Option<FileId>,
    ) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        if !alive(&self.source) || u32::try_from(group()).ok() != Some(self.source.pid) {
            return;
        }
        let reading = sample(layout().as_ref(), || image(self.source.pid));
        let foreground = group();
        if !alive(&self.source) || u32::try_from(foreground).ok() != Some(self.source.pid) {
            return;
        }
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        if meta.supervisor_revision != self.revision
            || claim(&meta, crate::metrics::now_us()) != Some(&self.source)
        {
            return;
        }
        let mut timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        if timeline.adoption_generation() == self.foreground {
            timeline.observe_codex_adoption(self.revision, &self.source, foreground, reading);
        }
    }
}

/// A worker keeps the Unix PTY descriptor alive across both foreground observations.
/// Windows identifies each ConPTY with a process-lifetime registry key.
#[cfg(unix)]
pub(crate) type Master = std::os::fd::OwnedFd;
#[cfg(not(unix))]
pub(crate) type Master = i32;

/// A quiet machine with no proven Codex sessions has no adoption timer. A new claim wakes
/// the window through its existing metadata event; the startup census covers a restored claim.
#[derive(Debug, Default)]
pub(crate) struct Watches {
    sessions: std::collections::HashMap<u64, Scheduled>,
    next: Option<Instant>,
    pool_epoch: Option<u64>,
}

impl Watches {
    pub(crate) fn retire(&mut self, session: u64) {
        self.sessions.remove(&session);
        if self.sessions.is_empty() {
            self.next = None;
        }
    }

    fn arm(&mut self, id: u64, ctx: &Arc<crate::SessionCtx>, master: i32, now: Instant) {
        let mut scheduled = self.sessions.remove(&id);
        // Keep polling a proven claimant while it temporarily leaves the foreground. Both
        // actual PTY reads belong to the worker; returning in front needs no new claim.
        let job = Job::due(&mut scheduled, ctx, now);
        if let Some(watch) = scheduled {
            self.next = Some(self.next.map_or(watch.next, |at| at.min(watch.next)));
            self.sessions.insert(id, watch);
        } else if self.sessions.is_empty() {
            self.next = None;
        }
        if let Some(job) = job {
            #[cfg(unix)]
            let Ok(master) = aterm_pty::dup_fd(master) else {
                return;
            };
            crate::codex_rollout_tap::observe_adoption(job, master);
        }
    }

    fn due(&mut self, now: Instant) -> Vec<u64> {
        if self.next.is_none_or(|next| now < next) {
            return Vec::new();
        }
        self.next = self
            .sessions
            .values()
            .map(|watch| watch.next)
            .filter(|&at| at > now)
            .min();
        self.sessions
            .iter()
            .filter_map(|(id, watch)| (watch.next <= now).then_some(*id))
            .collect()
    }
}

impl crate::App {
    pub(crate) fn observe_codex_claim(&mut self, id: u64, now: Instant) {
        if let Some(session) = self.pool.get(id) {
            self.session_status
                .adoption_watches
                .arm(id, &session.ctx, session.master, now);
        } else {
            self.session_status.adoption_watches.retire(id);
        }
    }

    pub(crate) fn observe_codex_packages(&mut self, now: Instant) -> Option<Instant> {
        let epoch = self.pool.insert_epoch();
        let watches = &mut self.session_status.adoption_watches;
        // Restored and adopted sessions can enter with an existing claim. Between pool
        // changes, inspect only named claims whose sampling deadline is due.
        let due = if watches.pool_epoch != Some(epoch) {
            watches.pool_epoch = Some(epoch);
            watches.next = None;
            self.pool.iter().map(|session| session.id).collect()
        } else {
            watches.due(now)
        };
        for id in due {
            self.observe_codex_claim(id, now);
        }
        self.session_status.adoption_watches.next
    }
}

#[cfg(test)]
#[path = "codex_adoption_tests.rs"]
mod tests;

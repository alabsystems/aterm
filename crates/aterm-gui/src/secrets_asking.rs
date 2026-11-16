// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An admin Mac's record of each Mac asking to join the secrets vault that its approver leaves
//! to a person (`aterm_secrets::client::asking`): no proof of the join password, or one that
//! did not match. Each one is an Info entry in the digest
//! ([`crate::join_entries`], never an alert on screen) naming the asking Mac and its code, and
//! its Approve lets exactly that Mac in (`aterm_secrets::client::approve_request`). Approve is
//! never a row's primary or default button ([`Intent::never_default`]): a person compares the
//! code with the one on the other Mac first. A key the vault let go (`code-only`) is filed the
//! same way, for an admin who compares its code (the approver's code-only standing).

use std::ops::ControlFlow;
use std::os::fd::{AsFd as _, BorrowedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_messages::{Message, Severity, tags};
use aterm_secrets::client::{Asking, Paths};

/// How often the lane reads the approver's record again where no event tells it what changed:
/// the platform's file events did not start, or the approver the record names, which runs, could
/// not be watched for its exit. As often as the approver writes it, as every read was before the
/// lane waited on events.
const READ_EVERY: Duration = Duration::from_secs(2);

/// The record's name in the vault folder: what `aterm_secrets::client::asking` reads.
const RECORD: &str = "asking";

/// The head line of the record that names the approver which wrote it, `<pid>@<birth>` or
/// `<pid>` after it (`aterm_secrets::client`'s `ASKING_BY`): the record stands only while that
/// process runs.
const APPROVER_HEAD: &str = "# approver ";

/// The most bytes of the record's head the lane reads for the approver's pid: the head line is
/// short (a pid and a start time), and the rest of the record is `asking`'s to read.
const HEAD_MOST: usize = 128;

/// The record as the lane last read it.
static ASKING: Mutex<Vec<Asking>> = Mutex::new(Vec::new());

/// Set when [`ASKING`] changed; the next park reads it.
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Whether the record changed since the last look ([`spawn_lane`]).
pub(crate) fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::AcqRel)
}

/// The record as the lane last read it.
pub(crate) fn current() -> Vec<Asking> {
    ASKING.lock().map(|held| held.clone()).unwrap_or_default()
}

/// Hand `now`, the record as just read, to the digest where it changed ([`take_changed`]).
fn publish(now: Vec<Asking>) {
    if let Ok(mut held) = ASKING.lock()
        && *held != now
    {
        *held = now;
        CHANGED.store(true, Ordering::Release);
    }
}

/// Start reading the approver's record: a windowed macOS instance, off the event loop, again at
/// each change of it and at the exit of the approver it names ([`follow`]), each change handed to
/// the digest at its next park ([`take_changed`]). Local files only: the approver (`aterm secrets
/// watch` on an admin Mac) reads the vault's queue. Whether the thread started.
pub(crate) fn spawn_lane(headless: bool) -> bool {
    if headless || !cfg!(target_os = "macos") {
        return false;
    }
    let spawned =
        crate::qos::spawn_lane_thread("aterm-secrets-asking", crate::qos::Role::Background, || {
            let Ok(paths) = aterm_secrets::client::Paths::live() else {
                return;
            };
            let bell = match aterm_uds::ready::Doorbell::new() {
                Ok(bell) => Arc::new(bell),
                Err(error) => {
                    aterm_log::info!(
                        "secrets vault: no doorbell for the join entries ({error}), so their \
                         record is read every {READ_EVERY:?}"
                    );
                    loop {
                        publish(aterm_secrets::client::asking(&paths));
                        std::thread::sleep(READ_EVERY);
                    }
                }
            };
            let watch = watch(&paths, Arc::clone(&bell));
            if let Err(error) = &watch {
                aterm_log::info!(
                    "secrets vault: no file watch on the join entries' record ({error}), so it \
                     is read every {READ_EVERY:?}"
                );
            }
            let _ = follow(&paths, watch.is_ok(), &bell, &mut publish, &mut wait_ready);
        });
    match spawned {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("secrets vault: the join entries did not start: {error}");
            false
        }
    }
}

/// The record `aterm_secrets::client::asking` reads under `paths`.
fn record(paths: &Paths) -> PathBuf {
    paths.dir.join(RECORD)
}

/// The watch on the approver's record, ringing `bell` at each change it hears: a write replaces
/// the record by rename and its removal ends it, and a watch on the file hears both through its
/// folder, the folder made after launch through the nearest one above it
/// ([`aterm_uds::file_watch::FileWatch::watch`]). Its first look rings once, folding in a change
/// made before it armed.
///
/// # Errors
/// The platform's file events, or the watch's thread, cannot start.
fn watch(
    paths: &Paths,
    bell: Arc<aterm_uds::ready::Doorbell>,
) -> std::io::Result<aterm_uds::file_watch::FileWatch> {
    let watch = aterm_uds::file_watch::FileWatch::platform(
        "aterm-secrets-asking-watch",
        Box::new(move |_| bell.ring()),
    )?;
    watch.watch(0, record(paths));
    Ok(watch)
}

/// The approver the record under `paths` names in its head ([`APPROVER_HEAD`]), by pid; `None`
/// where there is no record, its head names none (an approver before the head, whose record
/// stands as it is), or it cannot be read. Read by `aterm_secrets::fsx`'s admitted read, as
/// `aterm_secrets::client::asking` reads the record (`O_NOFOLLOW | O_NONBLOCK`, a regular file
/// of this user's that no other user can write), so a symlink or a FIFO there is refused, never
/// followed or waited on.
fn approver_named(paths: &Paths) -> Option<u32> {
    let head = aterm_secrets::fsx::read_owned_head(&record(paths), HEAD_MOST).ok()??;
    let head = String::from_utf8_lossy(&head);
    let named = head.lines().next()?.strip_prefix(APPROVER_HEAD)?;
    let pid = named.split_once('@').map_or(named, |(pid, _)| pid);
    pid.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| pid.parse().ok())
        .flatten()
}

/// How long one round of [`follow`] may wait with no event: `None` (only for an event) while
/// every input the record's reading turns on has one, the record's own file `watching`, and the
/// approver whose running the reading `shows` (it found Macs asking under an approver's head) is
/// watched for its exit (`exit_armed`); [`READ_EVERY`] where either has none.
fn rest(watching: bool, shows: bool, exit_armed: bool) -> Option<Duration> {
    (!watching || (shows && !exit_armed)).then_some(READ_EVERY)
}

/// How [`follow`] waits: on its descriptors until one is ready or the deadline passes, or it breaks to end.
type Wait<'a> = dyn FnMut(&[BorrowedFd<'_>], Option<Instant>) -> ControlFlow<()> + 'a;

/// Read the record under `paths` and hand it to `publish`, again each time a change of it rings
/// `bell` (the watch's, which `watching` says started) or the approver it names exits, then wait
/// with `wait` on those and on the rest [`rest`] gives. The approver's exit is armed before the
/// read that judges it runs, so an approver the read found running cannot exit unheard, and a
/// pid taken by another process before the arm reads as no approver. Each round empties `bell`
/// before it reads, so a ring during the read wakes the next wait. Ends when `wait` breaks.
fn follow(
    paths: &Paths,
    watching: bool,
    bell: &aterm_uds::ready::Doorbell,
    publish: &mut dyn FnMut(Vec<Asking>),
    wait: &mut Wait<'_>,
) -> ControlFlow<()> {
    loop {
        bell.clear();
        let named = approver_named(paths);
        let exit = named.and_then(aterm_uds::exitwatch::exit_fd);
        let now = aterm_secrets::client::asking(paths);
        let shows = named.is_some() && !now.is_empty();
        publish(now);
        let deadline =
            rest(watching, shows, exit.is_some()).and_then(|rest| Instant::now().checked_add(rest));
        let mut fds = vec![bell.fd()];
        fds.extend(exit.as_ref().filter(|_| shows).map(|exit| exit.as_fd()));
        wait(&fds, deadline)?;
    }
}

/// The lane's wait: until one of `fds` is readable or `deadline` passes, or [`READ_EVERY`] where
/// the wait itself failed. Never breaks.
fn wait_ready(fds: &[BorrowedFd<'_>], deadline: Option<Instant>) -> ControlFlow<()> {
    if let Err(error) = aterm_uds::ready::any(fds, deadline) {
        aterm_log::info!("secrets vault: the join entries' wait failed ({error})");
        std::thread::sleep(READ_EVERY);
    }
    ControlFlow::Continue(())
}

/// The record of an Approve that was refused, the refusal whole in its detail (the command it
/// names among it, which one clipped line would cut off): what a person who pressed it reads in
/// the log of messages, never an alert.
pub(crate) fn refused(host: Option<&str>, why: &str) -> Message {
    let title = match host {
        Some(host) => format!("Couldn't approve {}", crate::join_entries::shown_name(host)),
        None => "Couldn't approve that Mac".to_owned(),
    };
    Message::new(tags::PRIVACY, Severity::Warn, title)
        .sentence(format!("Approve did not let it in: {why}"))
        .record()
}

/// Approve exactly `recipient`, off the event loop: an admin Mac's press of an entry's Approve.
/// The entry ends once the approver's record no longer lists it. A refusal is logged and kept
/// as a record ([`refused`]).
pub(crate) fn approve(recipient: String) -> bool {
    crate::qos::spawn_lane_thread(
        "aterm-secrets-approve",
        crate::qos::Role::Responsive,
        move || {
            let paths = aterm_secrets::client::Paths::live();
            let approved = paths
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|paths| {
                    aterm_secrets::client::approve_request(paths, &recipient)
                        .map_err(|error| error.to_string())
                });
            match approved {
                Ok(host) => aterm_log::info!("secrets vault: approved {host}"),
                Err(error) => {
                    aterm_log::warn!("secrets vault: approving a Mac failed: {error}");
                    let host = paths
                        .map(|paths| aterm_secrets::client::asking(&paths))
                        .unwrap_or_default()
                        .into_iter()
                        .find(|asking| asking.recipient == recipient)
                        .map(|asking| asking.host);
                    crate::message_inbox::queue_message(refused(host.as_deref(), &error));
                }
            }
        },
    )
    .is_ok()
}

#[cfg(test)]
#[path = "secrets_asking_tests.rs"]
mod tests;

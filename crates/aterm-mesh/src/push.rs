// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A push another process of this Mac asks of the node service (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
//! §11 B7b: the claims ring's `push_until` waits for a relay ack). A window's or a session lane's
//! Commons runtime reads no relay ack: the relays run in the node service. So its push asks the
//! node over the egress's wake socket ([`crate::egress::wake_path`]), from a socket of its own
//! beside it, and the node answers once a neighbour acked the table ([`crate::Mesh::push_table`])
//! or the asker's deadline passed. Where no node answers, or the node moves no tables, the answer
//! is none, and the asker's old path pushes.

use crate::flow::Pushed;
use crate::logs::Mesh;
use crate::stores::Digest;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// The most pushes the node answers at once. One past it is answered at once with none, so its
/// asker's old path pushes; each answering thread ends at its asker's deadline.
const ANSWERING_MAX: usize = 8;

/// The longest a push may ask the node to wait: a longer ask waits this long.
const WITHIN_MAX: Duration = Duration::from_secs(60);

/// How long past its deadline an asker waits for the answer, which the node sends at the deadline
/// at the latest.
const ANSWER_GRACE: Duration = Duration::from_secs(1);

/// The longest an answer may be.
const ANSWER_MAX: usize = 64;

/// The pushes the node answers now.
static ANSWERING: AtomicUsize = AtomicUsize::new(0);

/// The datagram that asks the node to push `table` within `within`.
fn ask(table: &Digest, within: Duration) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(96);
    out.push_str("atm-push 1 ");
    aterm_codec::hex::push_lower(&mut out, table);
    let _ = writeln!(out, " {}", within.as_millis());
    out
}

/// The push `datagram` asks, `None` for anything else: its table and how long its asker waits,
/// at most [`WITHIN_MAX`].
pub(crate) fn read_ask(datagram: &[u8]) -> Option<(Digest, Duration)> {
    let text = std::str::from_utf8(datagram).ok()?.strip_suffix('\n')?;
    let mut words = text.split(' ');
    if words.next()? != "atm-push" || words.next()? != "1" {
        return None;
    }
    let hex = words.next()?;
    let mut table = [0u8; 32];
    if !aterm_codec::hex::is_lower(hex, 64)
        || !aterm_codec::hex::decode_into(hex.as_bytes(), &mut table)
    {
        return None;
    }
    let within = Duration::from_millis(words.next()?.parse().ok()?);
    if words.next().is_some() {
        return None;
    }
    Some((table, within.min(WITHIN_MAX)))
}

const fn answer_of(pushed: Option<Pushed>) -> &'static [u8] {
    match pushed {
        Some(Pushed::Reached) => b"atm-pushed 1 reached\n",
        Some(Pushed::TimedOut) => b"atm-pushed 1 timed-out\n",
        None => b"atm-pushed 1 none\n",
    }
}

/// The answer `datagram` holds: `Some` of what the node answered, `None` for anything else.
fn read_answer(datagram: &[u8]) -> Option<Option<Pushed>> {
    match datagram {
        b"atm-pushed 1 reached\n" => Some(Some(Pushed::Reached)),
        b"atm-pushed 1 timed-out\n" => Some(Some(Pushed::TimedOut)),
        b"atm-pushed 1 none\n" => Some(None),
        _ => None,
    }
}

/// Answer the push of `table` within `within` that `from` asked, by `mesh` (the one whose wake
/// socket `socket` is), on `socket`, from a thread of its own: the wake socket's reader never
/// waits on a push.
pub(crate) fn answer(
    socket: &Arc<UnixDatagram>,
    mesh: &Weak<Mesh>,
    table: Digest,
    within: Duration,
    from: &SocketAddr,
) {
    let Some(pathname) = from.as_pathname() else {
        return;
    };
    if ANSWERING.fetch_add(1, Ordering::AcqRel) >= ANSWERING_MAX {
        ANSWERING.fetch_sub(1, Ordering::AcqRel);
        let _ = socket.send_to(answer_of(None), pathname);
        return;
    }
    let deadline = Instant::now() + within;
    let (reply, reply_to, mesh) = (Arc::clone(socket), pathname.to_path_buf(), mesh.clone());
    let spawned = std::thread::Builder::new()
        .name("aterm-mesh-push".to_owned())
        .spawn(move || {
            let pushed = mesh
                .upgrade()
                .filter(|mesh| !mesh.is_stopped())
                .and_then(|mesh| mesh.push_table(table, deadline));
            let _ = reply.send_to(answer_of(pushed), &reply_to);
            ANSWERING.fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        ANSWERING.fetch_sub(1, Ordering::AcqRel);
        let _ = socket.send_to(answer_of(None), pathname);
    }
}

/// The asker's socket file, removed when the ask ends.
struct Asking(PathBuf);

impl Drop for Asking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Ask the node service whose state dir is `state_dir` to push `table` by `deadline`: what it
/// answered, or `None` where no node answered by the deadline (and a moment more), the node moves
/// no tables or follows no relay now, or this process cannot ask. With `None`, the caller's old
/// path pushes.
#[must_use]
pub fn push_by_node(state_dir: &Path, table: &Digest, deadline: Instant) -> Option<Pushed> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let wake = crate::egress::wake_path(state_dir);
    let path = wake.with_file_name(format!(
        "push-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if path.as_os_str().len() >= crate::logs::SUN_PATH_MAX {
        return None;
    }
    // A socket a crashed asker of this pid left behind.
    let _ = std::fs::remove_file(&path);
    let socket = UnixDatagram::bind(&path).ok()?;
    let _asking = Asking(path);
    let within = deadline.saturating_duration_since(Instant::now());
    socket.send_to(ask(table, within).as_bytes(), &wake).ok()?;
    socket
        .set_read_timeout(Some(within.min(WITHIN_MAX) + ANSWER_GRACE))
        .ok()?;
    let mut buf = [0u8; ANSWER_MAX];
    let n = socket.recv(&mut buf).ok()?;
    read_answer(&buf[..n])?
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;

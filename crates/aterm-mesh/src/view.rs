// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-seat view (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.12, §11 B9): what the node
//! service holds of every current seat, handed to a reader on this Mac over a private stream
//! socket in the mesh folder, `<state_dir>/mesh/view` ([`SOCKET`]).
//!
//! **What it holds** ([`View`]): each current seat's standing, how far it is from what this Mac
//! holds, and its newest presence row, as presence's board reads them ([`Seat`]); and each seat's
//! newest `now/needs` row (`aterm_metrics::row::NeedsRow`) and newest `now/activity` row
//! (`aterm_metrics::row::ActivityRow`), each read from this Mac's `metrics.log` for the answer and
//! judged as presence judges a row: its origin stamp carries its owner's producer id and its
//! envelope verifies under its owner's node key, so a removed Mac's row, or one a relaying Mac
//! forged, is not in it. `aterm fleet`, `aterm status` and the window's Fleet glance
//! read it; each read is one connection and one answer, and it starts nothing.
//!
//! **Why the node serves it.** "Up" is the board's judgement of rows that advanced while presence
//! ran (90 seconds, [`crate::presence::UP_FOR`]); a reader that opened its own tail would read
//! every seat silent until a period passed, and would need the metrics log's guard secret, which
//! never leaves the node service. A reader asks; the node answers from what it holds.
//!
//! **The wire.** The reader writes [`ASK`]; the node answers one frame, a big-endian `u32`
//! length and the view's encoding ([`View::encode`]), and closes. The socket lives in the mesh
//! folder (mode 0700) and the node answers only a peer of its own uid.
//!
//! **One version, no fallback.** An ask names the one version its reader decodes, and a node
//! hangs up on any other, so a reader and a node of different builds fail plainly ("hung up
//! without a view") rather than misread. No release has carried the view yet (version 2 added
//! the activity rows); once one does, a later change to it needs a node that answers each
//! version a released reader asks.

use crate::logs::{Mesh, MeshLog};
use crate::presence::{self, Behind, PresenceRow, Seat, Standing};
use aterm_metrics::row::{ActivityRow, NeedsRow, RowError};
use aterm_metrics::subject::{Node, Now, Subject};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The view's socket, in the mesh folder (`<state_dir>/mesh/`).
pub const SOCKET: &str = "view";

/// What a reader writes to ask for the view.
pub const ASK: &[u8] = b"atm-view 2\n";

/// The one version of the view's encoding this build writes and reads.
pub const VERSION: u8 = 2;

/// The longest answer, in bytes; a view that would be longer is not answered.
pub const FRAME_MAX: usize = 8 << 20;

/// The most seats a view holds, and the most needs rows and activity rows.
pub const SEATS_MAX: usize = 4096;

/// Every Mac's needs row.
pub const NEEDS_FILTER: &str = "/m/*/shared/now/needs";

/// Every Mac's activity row.
pub const ACTIVITY_FILTER: &str = "/m/*/shared/now/activity";

/// The bound on a reader's connect and ask, and on each read and write of its answer: the node
/// answers one reader at a time, each within [`ASK_WITHIN`] for its ask and [`ROWS_WITHIN`] for
/// its two row reads together (one store operation of at most [`ROWS_WITHIN`] past it), so one
/// answer takes at most 1 + 2 x 2 = 5 s and a reader waits out its own answer and the one ahead
/// of it.
pub const WITHIN: Duration = Duration::from_secs(10);

/// How long the node waits for a reader's ask, which a reader writes as soon as it connects.
const ASK_WITHIN: Duration = Duration::from_secs(1);

/// The bound on the node's row reads for one answer (the needs rows, then the activity rows),
/// one deadline both share ([`rows_deadline`]), and on each store operation in them: a read that
/// finds it passed holds none of its rows (`needs: None`, `activity: None`) rather than no view,
/// and the activity read gets only what the needs read left.
const ROWS_WITHIN: Duration = Duration::from_secs(2);

/// The rows one page of a row read asks for.
const ROWS_PAGE: u32 = 256;

/// The pages a row read walks, at most: [`SEATS_MAX`] rows.
const ROWS_PAGES_MAX: usize = SEATS_MAX / ROWS_PAGE as usize + 1;

/// What the node service holds of every current seat, as it answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// This Mac's principal, `n-<id>`.
    pub me: String,
    /// When the node answered, Unix ms of its wall clock.
    pub at_ms: u64,
    /// Whether the membership standings were read: before, the seats are this Mac's alone.
    pub standings: bool,
    /// Every current seat, this Mac's included, by principal.
    pub seats: Vec<Seat>,
    /// Each seat's newest needs row, by principal; `None` where the node could not read them.
    pub needs: Option<BTreeMap<String, NeedsRow>>,
    /// Each seat's newest activity row, by principal; `None` where the node could not read them.
    /// A seat that publishes none (an older build, or a Mac whose windows did not answer) has no
    /// entry.
    pub activity: Option<BTreeMap<String, ActivityRow>>,
}

impl View {
    /// This Mac's own seat.
    #[must_use]
    pub fn own(&self) -> Option<&Seat> {
        self.seats.iter().find(|seat| seat.me)
    }

    /// The canonical encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.seats.len() * 256);
        out.push(VERSION);
        put_principal(&mut out, &self.me);
        out.extend_from_slice(&self.at_ms.to_be_bytes());
        out.push(u8::from(self.standings));
        put_count(&mut out, self.seats.len());
        for seat in self.seats.iter().take(SEATS_MAX) {
            put_principal(&mut out, &seat.principal);
            out.extend_from_slice(&seat.key);
            out.push(u8::from(seat.me));
            match seat.standing {
                Standing::Up => out.push(0),
                Standing::Silent { last_seen_ms: None } => out.push(1),
                Standing::Silent {
                    last_seen_ms: Some(at),
                } => {
                    out.push(2);
                    out.extend_from_slice(&at.to_be_bytes());
                }
            }
            match seat.behind {
                Behind::InStep => out.push(0),
                Behind::Behind { at_least } => {
                    out.push(1);
                    let ms = u64::try_from(at_least.as_millis()).unwrap_or(u64::MAX);
                    out.extend_from_slice(&ms.to_be_bytes());
                }
                Behind::Apart => out.push(2),
                Behind::Unknown => out.push(3),
            }
            match &seat.row {
                None => out.push(0),
                Some((seq, row)) => {
                    out.push(1);
                    out.extend_from_slice(&seq.to_be_bytes());
                    put_bytes(&mut out, &row.encode());
                }
            }
        }
        put_rows(&mut out, self.needs.as_ref(), NeedsRow::encode);
        put_rows(&mut out, self.activity.as_ref(), ActivityRow::encode);
        out
    }

    /// Decode a canonical encoding.
    ///
    /// # Errors
    /// [`ViewError`] for anything [`View::encode`] does not write.
    pub fn decode(bytes: &[u8]) -> Result<Self, ViewError> {
        let mut r = Reader(bytes);
        let version = r.byte()?;
        if version != VERSION {
            return Err(ViewError::Version(version));
        }
        let me = r.principal()?;
        let at_ms = r.u64()?;
        let standings = r.flag()?;
        let count = r.count()?;
        let mut seats = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let principal = r.principal()?;
            let key: [u8; 32] = r.take(32)?.try_into().map_err(|_| ViewError::Truncated)?;
            let me = r.flag()?;
            let standing = match r.byte()? {
                0 => Standing::Up,
                1 => Standing::Silent { last_seen_ms: None },
                2 => Standing::Silent {
                    last_seen_ms: Some(r.u64()?),
                },
                _ => return Err(ViewError::Tag("standing")),
            };
            let behind = match r.byte()? {
                0 => Behind::InStep,
                1 => Behind::Behind {
                    at_least: Duration::from_millis(r.u64()?),
                },
                2 => Behind::Apart,
                3 => Behind::Unknown,
                _ => return Err(ViewError::Tag("behind")),
            };
            let row = match r.byte()? {
                0 => None,
                1 => {
                    let seq = r.u64()?;
                    let row = PresenceRow::decode(r.bytes()?).map_err(ViewError::Presence)?;
                    Some((seq, row))
                }
                _ => return Err(ViewError::Tag("row")),
            };
            seats.push(Seat {
                principal,
                key,
                me,
                standing,
                behind,
                row,
            });
        }
        let needs = r.rows(
            "needs",
            "a needs row named twice",
            NeedsRow::decode,
            ViewError::Needs,
        )?;
        let activity = r.rows(
            "activity",
            "an activity row named twice",
            ActivityRow::decode,
            ViewError::Activity,
        )?;
        if !r.0.is_empty() {
            return Err(ViewError::Trailing);
        }
        Ok(Self {
            me,
            at_ms,
            standings,
            seats,
            needs,
            activity,
        })
    }
}

/// Why bytes are not a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// The bytes end inside a field.
    Truncated,
    /// Bytes after the last field.
    Trailing,
    /// A version this build does not read.
    Version(u8),
    /// A principal that is no node id.
    Principal,
    /// More seats or needs rows than [`SEATS_MAX`].
    TooMany,
    /// A field whose tag this build does not write.
    Tag(&'static str),
    /// A presence row that does not decode.
    Presence(presence::RowError),
    /// A needs row that does not decode.
    Needs(RowError),
    /// An activity row that does not decode.
    Activity(RowError),
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("the view ends inside a field"),
            Self::Trailing => f.write_str("bytes follow the view"),
            Self::Version(v) => write!(f, "view version {v}, this build reads {VERSION}"),
            Self::Principal => f.write_str("a principal in the view is no node id"),
            Self::TooMany => write!(f, "more than {SEATS_MAX} seats in the view"),
            Self::Tag(what) => write!(f, "the view's {what} is not one this build writes"),
            Self::Presence(e) => write!(f, "a presence row in the view: {e}"),
            Self::Needs(e) => write!(f, "a needs row in the view: {e}"),
            Self::Activity(e) => write!(f, "an activity row in the view: {e}"),
        }
    }
}

impl std::error::Error for ViewError {}

fn put_principal(out: &mut Vec<u8>, principal: &str) {
    let bytes = principal.as_bytes();
    out.push(u8::try_from(bytes.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(bytes.get(..usize::from(u8::MAX)).unwrap_or(bytes));
}

fn put_count(out: &mut Vec<u8>, n: usize) {
    let n = u32::try_from(n.min(SEATS_MAX)).unwrap_or(u32::MAX);
    out.extend_from_slice(&n.to_be_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let n = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A map of rows by principal: a tag (0 unread, 1 read), then its count and each principal and
/// row encoding, at most [`SEATS_MAX`] of them.
fn put_rows<T>(out: &mut Vec<u8>, rows: Option<&BTreeMap<String, T>>, encode: fn(&T) -> Vec<u8>) {
    match rows {
        None => out.push(0),
        Some(rows) => {
            out.push(1);
            put_count(out, rows.len());
            for (principal, row) in rows.iter().take(SEATS_MAX) {
                put_principal(out, principal);
                put_bytes(out, &encode(row));
            }
        }
    }
}

/// The strict reader of [`View::decode`].
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ViewError> {
        if self.0.len() < n {
            return Err(ViewError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, ViewError> {
        Ok(self.take(1)?[0])
    }

    fn flag(&mut self) -> Result<bool, ViewError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ViewError::Tag("flag")),
        }
    }

    fn u32(&mut self) -> Result<u32, ViewError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| ViewError::Truncated)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, ViewError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| ViewError::Truncated)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, ViewError> {
        let n = usize::try_from(self.u32()?).map_err(|_| ViewError::TooMany)?;
        if n > SEATS_MAX {
            return Err(ViewError::TooMany);
        }
        Ok(n)
    }

    fn bytes(&mut self) -> Result<&'a [u8], ViewError> {
        let n = usize::try_from(self.u32()?).map_err(|_| ViewError::Truncated)?;
        self.take(n)
    }

    /// A map [`put_rows`] wrote: `what` names its tag, `twice` a principal it names twice.
    fn rows<T>(
        &mut self,
        what: &'static str,
        twice: &'static str,
        decode: fn(&[u8]) -> Result<T, RowError>,
        refused: fn(RowError) -> ViewError,
    ) -> Result<Option<BTreeMap<String, T>>, ViewError> {
        match self.byte()? {
            0 => Ok(None),
            1 => {
                let count = self.count()?;
                let mut rows = BTreeMap::new();
                for _ in 0..count {
                    let principal = self.principal()?;
                    let row = decode(self.bytes()?).map_err(refused)?;
                    if rows.insert(principal, row).is_some() {
                        return Err(ViewError::Tag(twice));
                    }
                }
                Ok(Some(rows))
            }
            _ => Err(ViewError::Tag(what)),
        }
    }

    fn principal(&mut self) -> Result<String, ViewError> {
        let n = usize::from(self.byte()?);
        let text = std::str::from_utf8(self.take(n)?).map_err(|_| ViewError::Principal)?;
        let node = Node::parse(text).ok_or(ViewError::Principal)?;
        let canonical = node.to_string();
        if canonical != text {
            return Err(ViewError::Principal);
        }
        Ok(canonical)
    }
}

/// Why a reader has no view.
#[derive(Debug)]
pub enum Unserved {
    /// No node service answers here: none runs, or it runs no mesh (`MESH` off, no vault, no
    /// node key).
    NoNode,
    /// The node took the ask but its answer failed, or is no view.
    Failed(String),
}

impl fmt::Display for Unserved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNode => f.write_str("no node service serves the mesh here"),
            Self::Failed(why) => write!(f, "the node service's view failed: {why}"),
        }
    }
}

impl std::error::Error for Unserved {}

/// Ask the node service whose mesh folder is `mesh_dir` (`<state_dir>/mesh/`) for its view,
/// each read and write bounded by `within`.
///
/// # Errors
/// [`Unserved::NoNode`] where nothing listens at the socket; [`Unserved::Failed`] where the
/// answer failed or is no view.
pub fn ask(mesh_dir: &Path, within: Duration) -> Result<View, Unserved> {
    let path = mesh_dir.join(SOCKET);
    let stream = match aterm_uds::kept_out(|| UnixStream::connect(&path)) {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::InvalidInput
            ) =>
        {
            return Err(Unserved::NoNode);
        }
        Err(e) => return Err(Unserved::Failed(format!("{}: {e}", path.display()))),
    };
    read_answer(stream, within).map_err(Unserved::Failed)
}

fn read_answer(mut stream: UnixStream, within: Duration) -> Result<View, String> {
    let failed = |e: io::Error| e.to_string();
    stream.set_read_timeout(Some(within)).map_err(failed)?;
    stream.set_write_timeout(Some(within)).map_err(failed)?;
    stream.write_all(ASK).map_err(failed)?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            "the node hung up without a view".to_owned()
        } else {
            e.to_string()
        }
    })?;
    let n = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
    if n > FRAME_MAX {
        return Err(format!("a view of {n} bytes, past {FRAME_MAX}"));
    }
    let mut body = vec![0u8; n];
    stream.read_exact(&mut body).map_err(failed)?;
    View::decode(&body).map_err(|e| e.to_string())
}

/// The view's listener in the node service, bound by presence ([`crate::presence::Presence`]).
pub(crate) struct Server {
    path: PathBuf,
    listener: UnixListener,
}

impl Server {
    /// Bind the view's socket in the mesh folder `dir`, replacing a stale one a previous run
    /// left; `None` where the path is too long for a socket.
    pub(crate) fn bind(dir: &Path) -> io::Result<Option<Self>> {
        let path = dir.join(SOCKET);
        if path.as_os_str().len() >= crate::logs::SUN_PATH_MAX {
            aterm_log::warn!(
                "mesh: the view is not served: {} is too long a path for a socket",
                path.display()
            );
            return Ok(None);
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| {
            use std::os::unix::fs::FileTypeExt as _;
            meta.file_type().is_socket()
        }) {
            std::fs::remove_file(&path)?;
        }
        let listener = aterm_uds::kept_out(|| UnixListener::bind(&path))?;
        Ok(Some(Self { path, listener }))
    }

    /// Answer each reader until `stopped` holds (a connection of [`Server::nudge`]'s wakes the
    /// wait to see it): its view of now, from `view`. A failed accept (out of descriptors, for
    /// one) ends no service: the wait parks a second and accepts again, so the listener this
    /// server holds never stays bound with nobody answering it.
    pub(crate) fn run(&self, stopped: &dyn Fn() -> bool, view: &dyn Fn() -> View) {
        let our_uid = aterm_uds::peer::our_uid();
        let mut failed = false;
        while !stopped() {
            let stream = match aterm_uds::accept(&self.listener) {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if !std::mem::replace(&mut failed, true) {
                        aterm_log::warn!("mesh: the view's socket failed, accepting again: {e}");
                    }
                    std::thread::park_timeout(Duration::from_secs(1));
                    continue;
                }
            };
            failed = false;
            if stopped() {
                return;
            }
            if aterm_uds::peer::peer_uid(&stream) != Some(our_uid) {
                continue;
            }
            if let Err(e) = answer(stream, view) {
                aterm_log::debug!("mesh: a view was not answered: {e}");
            }
        }
    }

    /// Wake [`Server::run`]'s wait, so it sees it is stopped.
    pub(crate) fn nudge(&self) {
        drop(aterm_uds::kept_out(|| UnixStream::connect(&self.path)));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A fixed view served on a thread of its own, as a node service serves its own
/// ([`serve_fixture`]); dropping it stops the thread and removes the socket.
#[cfg(any(test, debug_assertions))]
pub struct Fixture {
    server: std::sync::Arc<Server>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Test seam: serve `view` at the mesh folder `dir` (`<state_dir>/mesh/`) until the returned
/// [`Fixture`] drops, so a reader in another crate asks a node service's socket as it would.
///
/// # Errors
/// The socket cannot be bound, or its path is too long for one.
#[cfg(any(test, debug_assertions))]
pub fn serve_fixture(dir: &Path, view: View) -> io::Result<Fixture> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let server = Arc::new(Server::bind(dir)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "a path too long for a socket")
    })?);
    let stopped = Arc::new(AtomicBool::new(false));
    let thread = {
        let (server, stopped) = (Arc::clone(&server), Arc::clone(&stopped));
        std::thread::Builder::new()
            .name("aterm-mesh-view-fixture".to_owned())
            .spawn(move || server.run(&|| stopped.load(Ordering::Acquire), &|| view.clone()))?
    };
    Ok(Fixture {
        server,
        stopped,
        thread: Some(thread),
    })
}

#[cfg(any(test, debug_assertions))]
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.server.nudge();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read one reader's ask and answer it.
fn answer(mut stream: UnixStream, view: &dyn Fn() -> View) -> io::Result<()> {
    stream.set_read_timeout(Some(ASK_WITHIN))?;
    stream.set_write_timeout(Some(WITHIN))?;
    let mut asked = [0u8; ASK.len()];
    stream.read_exact(&mut asked)?;
    if asked != ASK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a view's ask",
        ));
    }
    let body = view().encode();
    if body.len() > FRAME_MAX {
        return Err(io::Error::other(format!(
            "a view of {} bytes, past {FRAME_MAX}",
            body.len()
        )));
    }
    let n = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&n.to_be_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame)
}

/// The one deadline an answer's row reads share: [`ROWS_WITHIN`] from now.
pub(crate) fn rows_deadline() -> Instant {
    Instant::now() + ROWS_WITHIN
}

/// Every current seat's newest needs row on this Mac's `metrics.log` ([`signed_now_rows`]).
pub(crate) fn needs_rows(mesh: &Mesh, deadline: Instant) -> io::Result<BTreeMap<String, NeedsRow>> {
    signed_now_rows(mesh, NEEDS_FILTER, Now::Needs, NeedsRow::decode, deadline)
}

/// Every current seat's newest activity row on this Mac's `metrics.log` ([`signed_now_rows`]).
pub(crate) fn activity_rows(
    mesh: &Mesh,
    deadline: Instant,
) -> io::Result<BTreeMap<String, ActivityRow>> {
    signed_now_rows(
        mesh,
        ACTIVITY_FILTER,
        Now::Activity,
        ActivityRow::decode,
        deadline,
    )
}

/// Every current seat's newest row under `filter` on this Mac's `metrics.log` whose subject is
/// `want` and whose body `decode` takes, judged as presence judges a row
/// ([`presence::signed_by`]); a row that fails, or a subject this build names otherwise or not
/// at all, is left out. It gives up once `deadline` has passed, before it starts or before a
/// page, each store operation bounded by the time left when it started (at most
/// [`ROWS_WITHIN`], from [`rows_deadline`]).
pub(crate) fn signed_now_rows<T>(
    mesh: &Mesh,
    filter: &str,
    want: Now,
    decode: fn(&[u8]) -> Result<T, RowError>,
    deadline: Instant,
) -> io::Result<BTreeMap<String, T>> {
    let late = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the rows of {filter} took longer than {} s",
                ROWS_WITHIN.as_secs()
            ),
        )
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(late());
    }
    let mut client =
        mesh.hosted(MeshLog::Metrics)
            .client(&mesh.me, &format!("ro:{filter}"), Some(left))?;
    if !client.origin_stamps()? {
        return Err(io::Error::other(
            "the metrics broker stamps no origins on a page",
        ));
    }
    let mut rows = BTreeMap::new();
    let mut cursor = String::new();
    for _ in 0..ROWS_PAGES_MAX {
        if Instant::now() >= deadline {
            return Err(late());
        }
        let (page, _, resume) = client.last_page(filter, &cursor, ROWS_PAGE)?;
        let origins = client.page_origins().to_vec();
        for (at, (_, subject, body)) in page.into_iter().enumerate() {
            let Ok(Subject::Now { node, now }) = Subject::parse(&subject) else {
                continue;
            };
            if now != want {
                continue;
            }
            let principal = node.to_string();
            let origin = origins.get(at).copied().flatten();
            let Ok((_, payload)) = presence::signed_by(mesh, &principal, &subject, origin, &body)
            else {
                continue;
            };
            if let Ok(row) = decode(payload) {
                rows.insert(principal, row);
            }
        }
        if resume.is_empty() || rows.len() >= SEATS_MAX {
            return Ok(rows);
        }
        cursor = resume;
    }
    Err(io::Error::other(format!(
        "the rows of {filter} did not end within {ROWS_PAGES_MAX} pages"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{Build, Held};
    use aterm_metrics::row::{AgentEntry, HeldEntry, ManagerEntry, NeedEntry};
    use std::collections::BTreeSet;

    const ME: &str = "n-00000000000000aa";
    const PEER: &str = "n-00000000000000bb";

    fn row(at_ms: u64) -> PresenceRow {
        PresenceRow {
            at_ms,
            build: Build::new("0.155.0+g0123abcd").expect("a build"),
            addresses: vec!["192.168.1.22:7480".parse().expect("an address")],
            membership: Some(Held::membership(&BTreeSet::from([[1; 32]]))),
            tables: None,
            topics: vec!["build".to_owned()],
        }
    }

    fn needs() -> NeedsRow {
        let sid = aterm_metrics::row::Sid::parse("s-0123456789abcdef0123").expect("a sid");
        NeedsRow {
            at: 1_793_836_800,
            needs: vec![NeedEntry {
                sid,
                word: aterm_metrics::row::Word::new("stalled").expect("a word"),
                harness: false,
                worker_of: None,
                since: Some(1_793_836_700),
            }],
            held: vec![HeldEntry {
                sid,
                hold: aterm_metrics::row::Word::new("person").expect("a word"),
                by: Node::parse(ME),
                since: None,
            }],
            managers: vec![ManagerEntry { sid, workers: 2 }],
        }
    }

    fn activity() -> ActivityRow {
        ActivityRow {
            at: 1_793_836_790,
            busy: 2,
            walled: 1,
            sessions: vec![AgentEntry {
                sid: aterm_metrics::row::Sid::parse("s-0123456789abcdef0123").expect("a sid"),
                word: aterm_metrics::row::Word::new("walled").expect("a word"),
                detail: aterm_metrics::row::Word::new("usage-weekly"),
            }],
            ..ActivityRow::default()
        }
    }

    fn view() -> View {
        View {
            me: ME.to_owned(),
            at_ms: 1_793_836_800_000,
            standings: true,
            seats: vec![
                Seat {
                    principal: ME.to_owned(),
                    key: [7; 32],
                    me: true,
                    standing: Standing::Up,
                    behind: Behind::InStep,
                    row: Some((41, row(1_793_836_790_000))),
                },
                Seat {
                    principal: PEER.to_owned(),
                    key: [8; 32],
                    me: false,
                    standing: Standing::Silent {
                        last_seen_ms: Some(1_793_836_000_000),
                    },
                    behind: Behind::Behind {
                        at_least: Duration::from_secs(300),
                    },
                    row: Some((9, row(1_793_836_000_000))),
                },
                Seat {
                    principal: "n-00000000000000cc".to_owned(),
                    key: [9; 32],
                    me: false,
                    standing: Standing::Silent { last_seen_ms: None },
                    behind: Behind::Unknown,
                    row: None,
                },
            ],
            needs: Some(BTreeMap::from([(PEER.to_owned(), needs())])),
            activity: Some(BTreeMap::from([(PEER.to_owned(), activity())])),
        }
    }

    /// A view reads back as itself, with or without its needs rows and its activity rows, and
    /// nothing it does not write reads as one: a cut, a trailing byte, another version (version
    /// 1's body, which ends at its needs), a principal that is no node id, a tag it never writes,
    /// more seats than it holds, or an activity row named twice.
    #[test]
    fn a_view_reads_back_as_itself_and_nothing_else_reads_as_one() {
        let whole = view();
        assert_eq!(View::decode(&whole.encode()), Ok(whole.clone()));
        let unread = View {
            needs: None,
            ..whole.clone()
        };
        assert_eq!(View::decode(&unread.encode()), Ok(unread));
        let no_activity = View {
            activity: None,
            ..whole.clone()
        };
        assert_eq!(View::decode(&no_activity.encode()), Ok(no_activity.clone()));
        // Version 1 wrote no activity map: its body is this one less the last tag, under 1.
        let mut v1 = no_activity.encode();
        assert_eq!(v1.pop(), Some(0), "the activity map's tag ends it");
        v1[0] = 1;
        assert_eq!(View::decode(&v1), Err(ViewError::Version(1)));
        let twice = {
            let once = View {
                needs: None,
                ..whole.clone()
            }
            .encode();
            let entry = 1 + PEER.len() + 4 + activity().encode().len();
            let (head, row) = once.split_at(once.len() - entry);
            let mut twice = head.to_vec();
            let count_at = twice.len() - 4;
            twice[count_at..].copy_from_slice(&2u32.to_be_bytes());
            twice.extend_from_slice(row);
            twice.extend_from_slice(row);
            twice
        };
        assert_eq!(
            View::decode(&twice),
            Err(ViewError::Tag("an activity row named twice"))
        );
        let bytes = whole.encode();
        for cut in [0, 1, 10, bytes.len() / 2, bytes.len() - 1] {
            assert_eq!(
                View::decode(&bytes[..cut]),
                Err(ViewError::Truncated),
                "cut at {cut}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(View::decode(&trailing), Err(ViewError::Trailing));
        let mut version = bytes.clone();
        version[0] = 3;
        assert_eq!(View::decode(&version), Err(ViewError::Version(3)));
        let mut principal = bytes.clone();
        principal[3] = b'X';
        assert_eq!(View::decode(&principal), Err(ViewError::Principal));
        // The standings flag sits after the version, the principal and the time.
        let flag_at = 1 + 1 + ME.len() + 8;
        let mut flag = bytes.clone();
        flag[flag_at] = 2;
        assert_eq!(View::decode(&flag), Err(ViewError::Tag("flag")));
        let mut many = bytes;
        many[flag_at + 1..flag_at + 5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(View::decode(&many), Err(ViewError::TooMany));
    }

    /// The node answers a reader of its own uid one frame per ask, and ends its wait when
    /// nudged; a stranger's bytes get no view, and the next reader is still answered.
    #[test]
    fn the_server_answers_each_ask_and_stops_when_nudged() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("av")
            .tempdir_in("/tmp")
            .expect("a scratch folder");
        let server = std::sync::Arc::new(
            Server::bind(dir.path())
                .expect("the socket binds")
                .expect("a short path"),
        );
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let serving = {
            let (server, stop) = (std::sync::Arc::clone(&server), std::sync::Arc::clone(&stop));
            std::thread::spawn(move || {
                server.run(&|| stop.load(std::sync::atomic::Ordering::Acquire), &view);
            })
        };
        let hang = Duration::from_secs(60);
        assert_eq!(ask(dir.path(), hang).expect("a view"), view());
        let mut stranger = UnixStream::connect(dir.path().join(SOCKET)).expect("a connection");
        stranger.set_read_timeout(Some(hang)).expect("a read bound");
        stranger.write_all(b"atm-view 1\n").expect("an ask written");
        let mut answered = Vec::new();
        stranger
            .read_to_end(&mut answered)
            .expect("the node hangs up");
        assert!(answered.is_empty(), "an older reader's ask gets no view");
        assert_eq!(ask(dir.path(), hang).expect("a view"), view());
        stop.store(true, std::sync::atomic::Ordering::Release);
        server.nudge();
        serving.join().expect("the server ends");
        drop(server);
        assert!(
            matches!(ask(dir.path(), hang), Err(Unserved::NoNode)),
            "a stopped node serves no view, and its socket is gone"
        );
    }
}

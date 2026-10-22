// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Media-capture verbs: `image` (terminal-content framebuffer PNG), `image read`
//! (structured inline-image payloads, headless/cross-session-safe), `window`
//! (whole-window composited PNG), and `chrome` (native macOS UI readout).
//! The `ImageReq` type and the `AUDIT_SUBSYSTEM` name live in `control.rs`,
//! reached via `super::`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aterm_containment::log_denial;
use aterm_core::grid::extra::{ImageData, ImageRef};
use aterm_core::terminal::Terminal;
use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};

use super::{AUDIT_SUBSYSTEM, ImageReq};
use crate::control_auth;
use crate::image_payload_memo::{ImagePayload, image_payloads};
use crate::platform::AppRt as _;
use crate::{Wake, term_lock};

/// Main-loop turns are normally sub-millisecond, but a debug build's first native
/// document raster can legitimately spend well over two seconds initializing and
/// painting its retained tray. Keep the 30-second wire allowance: it remains a
/// hard bound without misreporting cold work as a wedged event loop. Lane-exact
/// socket admission keeps other peers from waiting invisibly behind every
/// occupied worker.
const MAIN_THREAD_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Why a main-thread hop brought back no answer: a wire-ready reason, which a
/// caller prints with `{}`. Every reason but one is a fixed phrase. The one
/// that is not is the stalled main thread's refusal, which says how long and
/// where ([`crate::watchdog::MainStall`]) and was given before anything was
/// posted, so a retry is always safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainHopError {
    /// A fixed reason: a dialog stands, the event loop is gone, the deadline
    /// passed, the reply was dropped.
    Reason(&'static str),
    /// `main thread stalled <N>s; retry`.
    Stalled(crate::watchdog::MainStall),
}

impl std::fmt::Display for MainHopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reason(reason) => f.write_str(reason),
            Self::Stalled(stall) => std::fmt::Display::fmt(stall, f),
        }
    }
}

/// The refusal of a main-thread request whose client closed its socket before
/// the hop was posted ([`main_thread_refusal`]). It is written to a socket
/// nobody reads, so its words are for a trace, not a driver.
pub(crate) const CLIENT_HUNG_UP: &str = "client hung up before the request reached the main thread";

/// The main-thread hops in flight: posted by [`post_main`], not yet TAKEN by
/// the main thread and not given up on, and since when some have been.
/// [`main_thread_refusal`] reads it. A hop that has waited the stall bar on a
/// main thread that has not beaten in that time means the thread is stuck
/// past its idle park (inside AppKit, say), which the heartbeat alone cannot
/// tell from a thread that is only idle.
///
/// The count alone is not enough. A main thread idle at its park for ten
/// minutes has a ten-minute-old heartbeat, and the hop that wakes it is
/// outstanding for the moment it takes to answer: judged on the heartbeat
/// alone, a second verb arriving in that moment would be refused. So the run
/// of outstanding hops records when it began, and it counts only once it has
/// waited the bar itself (`MainThreadStallRefusal`'s `Buggy` replays the
/// heartbeat-only rule).
///
/// A hop leaves the count when the main thread TAKES it ([`take_hop`], first
/// thing in `user_event`), not when its worker gets the reply. Every hop but
/// one is answered in the turn that takes it. `settings set|unset` is not: the
/// main thread queues the write and the reply comes turns later, from the
/// config worker's completion. Counted until that reply, a healthy thread
/// parked idle behind it would read as stuck, and every other main-thread verb
/// would be refused once the write took the bar (`MainThreadStallRefusal`'s
/// `Buggy` replays that rule too). The run's start can then be older than the
/// hops still counted in it. That never refuses a moving thread: the hops were
/// taken in the order they were posted, so a beat came after that start, and
/// the reader measures from whichever of the two is later.
pub(crate) struct Hops {
    outstanding: AtomicUsize,
    /// When `outstanding` last left zero, on the `crate::metrics::now_ns`
    /// clock; 0 while nothing is outstanding.
    run_since_ns: AtomicU64,
}

impl Hops {
    pub(crate) const fn new() -> Self {
        Self {
            outstanding: AtomicUsize::new(0),
            run_since_ns: AtomicU64::new(0),
        }
    }

    /// Count one hop from `now_ns` until the main thread takes it
    /// ([`HopMark::taken`]) or the guard drops, whichever comes first.
    pub(crate) fn post(&self, now_ns: u64) -> HopGuard<'_> {
        if self.outstanding.fetch_add(1, Ordering::Relaxed) == 0 {
            self.run_since_ns.store(now_ns.max(1), Ordering::Relaxed);
        }
        HopGuard {
            mark: HopMark {
                hops: self,
                counted: Arc::new(AtomicBool::new(true)),
            },
        }
    }

    /// When the current run of outstanding hops began, or `None` when no hop
    /// is outstanding. Two relaxed words: a reader that catches a run ending
    /// as the next begins can read the older start, a stall only if the
    /// heartbeat has not moved for the bar either, or no start, which is no
    /// refusal. Neither refuses a thread that is answering.
    pub(crate) fn since_ns(&self) -> Option<u64> {
        if self.outstanding.load(Ordering::Relaxed) == 0 {
            return None;
        }
        match self.run_since_ns.load(Ordering::Relaxed) {
            0 => None,
            since => Some(since),
        }
    }
}

/// One hop's place in the [`Hops`] count, shared by the worker's
/// [`HopGuard`] and the [`Wake::Hop`] it posts. Whichever of the two comes
/// first counts the hop out: the main thread taking it, or the worker giving
/// up (reply, deadline, event loop gone). The other then finds nothing to
/// count out, so a hop is counted out exactly once.
pub(crate) struct HopMark<'a> {
    hops: &'a Hops,
    /// True while this hop is still in the count.
    counted: Arc<AtomicBool>,
}

impl HopMark<'_> {
    /// The main thread has taken this hop: it is out of the count, whenever
    /// its reply comes.
    pub(crate) fn taken(self) {
        self.count_out();
    }

    fn count_out(&self) {
        if self.counted.swap(false, Ordering::Relaxed)
            && self.hops.outstanding.fetch_sub(1, Ordering::Relaxed) == 1
        {
            self.hops.run_since_ns.store(0, Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for HopMark<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HopMark")
            .field("counted", &self.counted.load(Ordering::Relaxed))
            .finish()
    }
}

/// One posted hop, from its worker's side: it counts itself out of [`Hops`]
/// when it drops (on the reply, on the deadline, and on an event loop that is
/// gone, every path alike) unless the main thread took it first.
pub(crate) struct HopGuard<'a> {
    mark: HopMark<'a>,
}

impl<'a> HopGuard<'a> {
    /// The mark the posted [`Wake::Hop`] carries to the main thread.
    pub(crate) fn mark(&self) -> HopMark<'a> {
        HopMark {
            hops: self.mark.hops,
            counted: Arc::clone(&self.mark.counted),
        }
    }
}

impl Drop for HopGuard<'_> {
    fn drop(&mut self) {
        self.mark.count_out();
    }
}

/// The process's one [`Hops`].
static HOPS: Hops = Hops::new();

/// The request a control worker posted, with its hop counted out: the main
/// thread has taken it. `user_event` calls this first, on every delivered
/// [`Wake`], so what it handles is never a [`Wake::Hop`], and a request whose
/// reply is queued (`settings set`) stops counting as a hop that waits on the
/// main thread the moment the main thread has it.
pub(crate) fn take_hop(ev: Wake) -> Wake {
    match ev {
        Wake::Hop { mark, wake } => {
            mark.taken();
            take_hop(*wake)
        }
        other => other,
    }
}

/// THE ONE main-thread RPC's post. Every verb the control thread cannot answer
/// itself (it cannot touch `App`/AppKit) needs the same hop: refused first,
/// before its request is built ([`main_thread_refusal`]); then a one-shot
/// reply channel and a reply-bearing `Wake`, posted as a counted [`Wake::Hop`].
/// The caller blocks on the returned receiver and holds the guard while it
/// waits: the hop counts from the post until the main thread takes it or the
/// guard drops, however the wait ends, so the next verb can tell a hop that
/// has waited too long for the main thread. [`call_main`] and
/// [`call_main_within`] add a bounded receive; the pixel verbs (`image`,
/// `window`, a `video` recording) keep their own, which cancel publication on
/// a give-up. [`MainPost::ask`] is the same counted post for a lane written
/// against an injected send (`edit`, `await edit`), which asks its refusal
/// itself. `askpass` alone posts its request uncounted
/// (`control_askpass::serve`). `Err` is a wire-ready reason.
pub(crate) fn post_main<T>(
    proxy: &EventLoopProxy<Wake>,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<(std::sync::mpsc::Receiver<T>, HopGuard<'static>), MainHopError> {
    post_counted(
        &HOPS,
        main_thread_refusal(),
        |wake| proxy.send_event(wake).is_ok(),
        make,
    )
}

/// [`post_main`] on a given count, with the refusal already asked and the
/// event loop's `send_event` as `send`, which answers whether the loop took
/// the wake: a test can build no `EventLoopProxy`. A refusal returns before
/// `make` runs, so the request's guards drop unbuilt and nothing is posted or
/// counted. A wake the loop gives back is dropped in `send`, which drops the
/// guards it carries, and its hop counts itself out with the guard.
fn post_counted<T>(
    hops: &'static Hops,
    refusal: Option<MainHopError>,
    send: impl FnOnce(Wake) -> bool,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<(std::sync::mpsc::Receiver<T>, HopGuard<'static>), MainHopError> {
    if let Some(refusal) = refusal {
        return Err(refusal);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let hop = hops.post(crate::metrics::now_ns());
    let delivered = send(Wake::Hop {
        mark: hop.mark(),
        wake: Box::new(make(tx)),
    });
    if !delivered {
        return Err(MainHopError::Reason("event loop gone"));
    }
    Ok((rx, hop))
}

/// The main thread as a lane whose logic is written against an injected send
/// reaches it: a unit test can build no `EventLoopProxy`, so it hands the lane
/// a closure instead. [`Self::ask`] posts a reply-bearing request as
/// [`post_main`]'s counted hop, [`Self::tell`] a notice that waits for
/// nothing (a cancel, a withdrawal) as it is.
pub(crate) struct MainPost<'a> {
    send: Box<dyn FnMut(Wake) -> bool + 'a>,
    hops: &'static Hops,
}

impl<'a> MainPost<'a> {
    /// Through `proxy`'s `send_event`, counted on the process's [`Hops`],
    /// which [`main_thread_refusal`] reads.
    pub(crate) fn new(proxy: &'a EventLoopProxy<Wake>) -> Self {
        Self {
            send: Box::new(|wake| proxy.send_event(wake).is_ok()),
            hops: &HOPS,
        }
    }

    /// Through `send`, which answers whether the main thread took the wake,
    /// counted on `hops`: a test's own of both.
    #[cfg(test)]
    pub(crate) fn injected(send: impl FnMut(Wake) -> bool + 'a, hops: &'static Hops) -> Self {
        Self {
            send: Box::new(send),
            hops,
        }
    }

    /// [`post_counted`] with no refusal: a lane asks [`main_thread_refusal`]
    /// itself, before it builds its request, where its verb is refused at
    /// all. The caller holds the guard while it waits for the reply.
    pub(crate) fn ask<T>(
        &mut self,
        make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
    ) -> Result<(std::sync::mpsc::Receiver<T>, HopGuard<'static>), MainHopError> {
        post_counted(self.hops, None, &mut self.send, make)
    }

    /// Post `wake` uncounted; whether the main thread took it.
    pub(crate) fn tell(&mut self, wake: Wake) -> bool {
        (self.send)(wake)
    }
}

/// [`post_main`] and a bounded wait for the answer, shared by UI-mutating and
/// aux-window verbs (`controls`, `open`, `settings`, `invoke`, `spawn`, `close`,
/// `chrome`). `Err` is a wire-ready reason; callers format `Ok(T)` into their
/// own reply shape.
pub(crate) fn call_main<T>(
    proxy: &EventLoopProxy<Wake>,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<T, MainHopError> {
    let (rx, _hop) = post_main(proxy, make)?;
    // These RPCs answer within one event-loop turn; the deadline only fires when
    // that turn cannot finish within the generous cold-render allowance. Timing
    // out frees THIS fixed worker lane instead of retaining it forever.
    match rx.recv_timeout(MAIN_THREAD_REPLY_TIMEOUT) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // State only what expiring this deadline actually proves: no reply
            // arrived in time. A timeout cannot distinguish a wedged loop from a
            // slow turn from a request nobody ever settles, so it must not name one.
            Err(MainHopError::Reason(
                "main-thread reply did not arrive within 30s",
            ))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(MainHopError::Reason("main-thread reply dropped"))
        }
    }
}

/// [`call_main`] with a caller-chosen deadline instead of the 30 s cold-render
/// allowance. A verb under a tight external budget must not park its worker
/// lane for 30 s when the main thread is momentarily busy: discovery clients
/// probe a peer with a 2 s deadline (the menu-bar fleet scan, `aterm ctl ls`),
/// so a short deadline degrades to the verb's `Err` path (the roster prints
/// `window=- active=- wfocus=-` and still lists the session) instead of timing
/// out the probe.
pub(crate) fn call_main_within<T>(
    proxy: &EventLoopProxy<Wake>,
    within: std::time::Duration,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<T, MainHopError> {
    let (rx, _hop) = post_main(proxy, make)?;
    recv_within(&rx, within).map_err(MainHopError::Reason)
}

/// Why a main-thread verb is refused before anything is posted, if it is. A
/// stalled main thread comes first ([`crate::watchdog::main_stall_now`], at the
/// shipped stall bar in every build), answered at once with
/// `ERR main thread stalled <N>s; retry`, which the harness rides out as it
/// does `ERR control server busy`. Then a standing dialog ([`dialog_refusal`]);
/// a dialog is a designed freeze and never reads as a stall.
pub(crate) fn main_thread_refusal() -> Option<MainHopError> {
    // A client that hung up is asked about before anything else: nothing is
    // posted for a request nobody will read the answer to, so it neither holds
    // a worker lane for the reply deadline nor runs late after its client has
    // asked again. `caller_hung_up` is the blocking verbs' check — the lane's
    // own connection, a full close only, never a half-close.
    // A verb committed to finishing input it began is the one exception
    // ([`crate::control::CommittedInput`]: a `turn` whose text is typed still
    // gets its Enter, or the text sits unsubmitted in the composer).
    if crate::control::caller_hung_up() && !crate::control::input_committed() {
        aterm_log::debug!("control: a main-thread request was dropped; its client hung up");
        return Some(MainHopError::Reason(CLIENT_HUNG_UP));
    }
    if let Some(stall) = crate::watchdog::main_stall_now(HOPS.since_ns()) {
        let returned = if stall.returned { " (returned)" } else { "" };
        aterm_log::debug!(
            "control: main-thread request refused; stalled {}s at {}{returned}",
            stall.stalled.as_secs(),
            stall.root.ident()
        );
        return Some(MainHopError::Stalled(stall));
    }
    dialog_refusal().map(MainHopError::Reason)
}

/// While a dialog stands the main thread is inside AppKit's nested modal
/// loop (`NSAlert runModal`, `NSOpenPanel runModal` — the quit confirmation
/// among them), where no `Wake` is handled until the person answers: a
/// main-thread verb posted now would park its worker for the whole of the
/// reply deadline and then run late, after the answer, against whatever the
/// answer left. It is refused at once instead, and never posted.
fn dialog_refusal() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        dialog_refusal_for(crate::watchdog::current())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// [`dialog_refusal`] for one breadcrumb, pure so it is testable without a
/// dialog: only the modal park refuses.
#[cfg(any(target_os = "macos", test))]
fn dialog_refusal_for(breadcrumb: crate::watchdog::Breadcrumb) -> Option<&'static str> {
    (breadcrumb == crate::watchdog::Breadcrumb::Modal)
        .then_some("aterm is showing a dialog; answer it, then retry")
}

/// The recv half of [`call_main_within`], factored so the deadline behaviour is
/// unit-testable without an event loop: a `Timeout` becomes the degrade signal
/// (any `Err` drives the caller's dashes), a `Disconnected` reports the dropped
/// reply.
fn recv_within<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    within: std::time::Duration,
) -> Result<T, &'static str> {
    match rx.recv_timeout(within) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("main thread did not answer within the placement deadline")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("main-thread reply dropped"),
    }
}

/// Per-image payload cap for the line + JSON image channels. An
/// inline image is user-supplied (the inner TUI emits OSC 1337), so a hostile or
/// careless inner could embed a multi-megabyte image and force a large base64
/// allocation on every `image read` and every styled `cells`/`screen` frame. Above
/// this raw-byte cap the payload is omitted and the image marked `truncated` — the
/// metadata + real `nbytes` still report it, so a consumer learns an image is there
/// and how big it is, then fetches it deliberately, without the per-frame blowup.
pub(crate) const MAX_IMAGE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024; // 4 MiB raw (~5.3 MiB base64)

/// One image's wire payload under the size cap, through the process's payload
/// memo ([`crate::image_payload_memo`]): `png`/`rgba`/`unknown` and the
/// base64 of its bytes, or `truncated` and an empty payload when they exceed
/// [`MAX_IMAGE_PAYLOAD_BYTES`].
pub(crate) fn image_payload(img: &Arc<ImageData>) -> ImagePayload {
    // A batch answers every image it is asked about.
    image_payloads(&[img])
        .pop()
        .unwrap_or(ImagePayload::TRUNCATED)
}

/// Append one `image read` result line, without its newline:
/// `<row> <col> <img_cols> <img_rows> <cell_row> <cell_col> <format> <nbytes> <base64>`.
/// `row`/`col` are the image's TOP-LEFT anchor on the grid; `cell_row`/`cell_col`
/// are the tile of interest (0/0 for a whole-image report, the queried tile in
/// cell mode); `nbytes` is the raw (pre-base64) length; the trailing base64 is the
/// image's full raw payload (PNG bytes etc.), independent of the GUI framebuffer.
/// The base64 is pushed from the payload, never formatted through a temporary.
pub(crate) fn image_read_line(
    out: &mut String,
    anchor_r: usize,
    anchor_c: usize,
    tile_row: u16,
    tile_col: u16,
    img: &ImageData,
    payload: &ImagePayload,
) {
    let _ = write!(
        out,
        "{anchor_r} {anchor_c} {} {} {tile_row} {tile_col} {} {} ",
        img.cols,
        img.rows,
        payload.format(),
        img.bytes.len(),
    );
    out.push_str(payload.b64());
}

/// Bytes an `image read` line takes besides its base64.
const IMAGE_READ_LINE_BYTES: usize = 96;

/// `image read [<r> [<c>]]` -> the structured inline-image payloads (iTerm2 OSC
/// 1337) as base64, readable HEADLESS and CROSS-SESSION (unlike the framebuffer
/// `image` rasterize verb). With no args it reports every DISTINCT image on the
/// grid (deduplicated by payload identity), one line per image at its top-left
/// anchor; `image read <r>` restricts to images intersecting row `r`; `image read
/// <r> <c>` returns the tile of the TOP image covering that exact cell — the one
/// drawn over any others stacked there (`ERR none` if the cell has no image).
/// Framed `OK <nlines>\n` + one line per image.
///
/// GATHER-THEN-SERIALIZE, like the styled-frame path: everything the reply needs
/// (anchors, tile indices, and an `Arc` clone of each payload) is collected under
/// ONE lock hold, then the guard is dropped and the (up to multi-MiB per image)
/// base64 encode runs with the lock RELEASED. That mutex is the same one the PTY
/// reader's `process()` and the renderer's frame snapshot take — holds there are
/// budgeted in tens of microseconds, and an on-lock encode of a 4 MiB image is
/// milliseconds. The bytes are `Arc`-shared and immutable once placed, so encoding
/// after the drop yields byte-identical output.
pub(crate) fn cmd_image_read(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let t = term_lock(term);
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let mut it = rest.split_whitespace();
    let r_tok = it.next();
    let c_tok = it.next();
    if it.next().is_some() {
        return "ERR bad args\n".to_string();
    }

    // Cell mode: the image covering exactly (r, c).
    if let (Some(rs), Some(cs)) = (r_tok, c_tok) {
        let (Ok(r), Ok(c)) = (rs.parse::<usize>(), cs.parse::<usize>()) else {
            return "ERR bad args\n".to_string();
        };
        if r >= rows || c >= cols {
            return format!(
                "ERR out of range: rows 0-{}, cols 0-{}\n",
                rows.saturating_sub(1),
                cols.saturating_sub(1)
            );
        }
        // Gather only: the LAST tile at that column — a cell's layers come
        // bottom first — carried out of the lock scope as an `Arc` clone.
        let hit = t
            .images_row(r)
            .into_iter()
            .rfind(|(col, _)| *col == c)
            .map(|(col, iref)| {
                let anchor_r = r.saturating_sub(iref.cell_row as usize);
                let anchor_c = col.saturating_sub(iref.cell_col as usize);
                (anchor_r, anchor_c, iref.cell_row, iref.cell_col, iref.image)
            });
        drop(t);
        let Some((anchor_r, anchor_c, cell_row, cell_col, img)) = hit else {
            return "ERR none\n".to_string();
        };
        let payload = image_payload(&img);
        let mut out = String::with_capacity(8 + IMAGE_READ_LINE_BYTES + payload.b64().len());
        out.push_str("OK 1\n");
        image_read_line(
            &mut out, anchor_r, anchor_c, cell_row, cell_col, &img, &payload,
        );
        out.push('\n');
        return out;
    }

    // Row mode (one row) or screen mode (all rows): distinct images, anchored.
    // Screen mode reads every row's placements in ONE pass over the extras map
    // (`images_frame`, pinned equal to `images_row` for every row by
    // `images_frame_into_matches_per_row`), not `rows` per-row scans that each
    // probe `cols` cells whenever any extra exists anywhere. Rows keep their
    // index, so the walk order — rows ascending, then columns — is unchanged.
    let placements: Vec<(usize, Vec<(usize, ImageRef)>)> = match r_tok {
        Some(rs) => match rs.parse::<usize>() {
            Ok(r) if r < rows => vec![(r, t.images_row(r))],
            Ok(_) => {
                return format!(
                    "ERR out of range: rows 0-{}, cols 0-{}\n",
                    rows.saturating_sub(1),
                    cols.saturating_sub(1)
                );
            }
            Err(_) => return "ERR bad args\n".to_string(),
        },
        None => t.images_frame(rows).into_iter().enumerate().collect(),
    };
    // Identity dedup by set so tiled screens with many cells sharing a few
    // images stay linear in placed cells.
    let mut seen: std::collections::HashSet<*const ImageData> = std::collections::HashSet::new();
    // Anchor + payload handle per distinct image, in the same walk order the reply
    // uses. Holding the `Arc` keeps each payload alive (and keeps the `seen`
    // pointer-identity dedup valid) after the guard drops.
    let mut hits: Vec<(usize, usize, Arc<ImageData>)> = Vec::new();
    for (r, row) in placements {
        for (col, iref) in row {
            if !seen.insert(std::sync::Arc::as_ptr(&iref.image)) {
                continue;
            }
            let anchor_r = r.saturating_sub(iref.cell_row as usize);
            let anchor_c = col.saturating_sub(iref.cell_col as usize);
            hits.push((anchor_r, anchor_c, iref.image));
        }
    }
    drop(t);
    let images: Vec<&Arc<ImageData>> = hits.iter().map(|(_, _, img)| img).collect();
    let payloads = image_payloads(&images);
    let mut out = String::with_capacity(
        16 + payloads
            .iter()
            .map(|payload| IMAGE_READ_LINE_BYTES + payload.b64().len())
            .sum::<usize>(),
    );
    let _ = writeln!(out, "OK {}", hits.len());
    for ((anchor_r, anchor_c, img), payload) in hits.iter().zip(&payloads) {
        // Whole-image report: anchor + tile 0/0 (the full payload is carried).
        image_read_line(&mut out, *anchor_r, *anchor_c, 0, 0, img, payload);
        out.push('\n');
    }
    out
}

/// `image [path]` -> hand the render to the MAIN thread (it owns the renderer),
/// block on the guarded result, and report `OK <w> <h> <path>\n`. The encode
/// worker transfers that result only after the PNG is fully written; the socket
/// writer revalidates it and retains its identity through the complete response
/// challenge and causal client ACK (or the failed-handoff quarantine).
///
/// PATH SAFETY: the PNG is confined to the `images/` subdir of the per-user
/// socket directory. A bare name (`image shot.png`) lands there; an empty
/// request gets a server-unique path below `images/auto/<process-instance>/`.
/// A path that would escape the subdir (`../`, an absolute path elsewhere, a symlink out) is refused with
/// `ERR path\n` and audited.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ImageOptions {
    want_bytes: bool,
    want_metadata: bool,
    rest: String,
}

fn parse_image_options(rest: &str) -> ImageOptions {
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    ImageOptions {
        want_bytes: tokens
            .iter()
            .any(|token| *token == "--bytes" || *token == "bytes"),
        want_metadata: tokens.contains(&"--meta"),
        rest: tokens
            .into_iter()
            .filter(|token| *token != "--bytes" && *token != "bytes" && *token != "--meta")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

struct CancelCaptureRequestOnDrop(Option<crate::control::CaptureCancellation>);

impl CancelCaptureRequestOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }

    fn cancel_won(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(crate::control::CaptureCancellation::cancel)
    }
}

impl Drop for CancelCaptureRequestOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.cancel();
        }
    }
}

fn recv_capture_reply<T>(
    rx: &std::sync::mpsc::Receiver<Result<crate::control::Retained<T>, String>>,
    cancel_on_drop: &mut CancelCaptureRequestOnDrop,
    timeout: std::time::Duration,
    context: &str,
) -> Result<crate::control::Retained<T>, String> {
    match rx.recv_timeout(timeout) {
        Ok(result) => {
            cancel_on_drop.disarm();
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancel_on_drop.cancel_won() => {
            cancel_on_drop.disarm();
            Err(format!("{context}: timed out; publication cancelled"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // The worker won the one-shot election immediately before the final
            // name operation. Cancellation can no longer make an ERR truthful:
            // wait for the committed result or an actual worker disconnect.
            match rx.recv() {
                Ok(result) => {
                    cancel_on_drop.disarm();
                    result
                }
                Err(_) => {
                    cancel_on_drop.disarm();
                    Err(format!(
                        "{context}: capture worker disconnected after commit authorization"
                    ))
                }
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{context}: capture worker disconnected"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageTargetError {
    AdmissionRefused,
    AutomaticUnavailable,
    InvalidRequested,
}

fn prepare_image_target(
    want_bytes: bool,
    clean: bool,
    requested: &str,
    sock_dir: &std::path::Path,
    budgets: &crate::control::ArtifactBudgets,
    handoff: &mut crate::control::ReplyRetentionPermit,
) -> Result<(control_auth::ConfinedImage, Option<String>), ImageTargetError> {
    if want_bytes {
        return Ok((control_auth::ConfinedImage::in_memory(), None));
    }

    let target = if requested.is_empty() {
        let stem = if clean {
            "aterm-clean"
        } else {
            "aterm-control"
        };
        control_auth::confine_automatic_image_path_with_admission(sock_dir, stem, budgets, |path| {
            handoff.try_reconcile_for_resolved_path(path)
        })
        .map_err(|error| match error {
            control_auth::ArtifactConfinementError::AdmissionRefused => {
                ImageTargetError::AdmissionRefused
            }
            control_auth::ArtifactConfinementError::Invalid => {
                ImageTargetError::AutomaticUnavailable
            }
        })?
    } else {
        control_auth::confine_image_path_with_admission(sock_dir, requested, |path| {
            handoff.try_reconcile_for_resolved_path(path)
        })
        .map_err(|error| match error {
            control_auth::ArtifactConfinementError::AdmissionRefused => {
                ImageTargetError::AdmissionRefused
            }
            control_auth::ArtifactConfinementError::Invalid => ImageTargetError::InvalidRequested,
        })?
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = target.display_path().to_string_lossy().into_owned();
    Ok((target, Some(path)))
}

pub(crate) fn cmd_image(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    session: Option<u64>,
) -> crate::control::ControlReply {
    // `--bytes` / `bytes`: return the PNG OVER THE WIRE — the capture form for a
    // driver that cannot read the SERVER's filesystem, which the file path
    // names. LINES-framed as `OK 1\n<w> <h> <nbytes> <base64>` (see the return
    // site below + `framing_of`'s image-bytes flip), NOT a raw byte body. Strip the
    // flag from anywhere in the tail; the remainder parses as usual.
    let options = parse_image_options(rest);
    let want_bytes = options.want_bytes;
    let want_metadata = options.want_metadata;
    let rest = options.rest.as_str();
    // Optional leading `plain`/`clean` keyword → a CLEAN capture with all visual bling
    // (cursor trail/glow, sparkle words, the Scene) suppressed, so an AI reads the bare
    // terminal. The rest (if any) is the filename. `image plain [name]`.
    let (clean, rest) = {
        let t = rest.trim();
        match t.split_once(char::is_whitespace) {
            Some((kw, r)) if kw == "plain" || kw == "clean" => (true, r.trim()),
            None if t == "plain" || t == "clean" => (true, ""),
            _ => (false, t),
        }
    };
    let budgets = crate::control::ArtifactBudgets::process();
    let permit = if want_bytes {
        budgets.try_reserve_in_memory()
    } else {
        budgets.try_reserve_for_path(sock_dir)
    };
    let Some(mut handoff) = permit else {
        return format!("ERR image: {}\n", crate::control::ARTIFACT_HANDOFF_BUSY).into();
    };
    let (target, path) =
        match prepare_image_target(want_bytes, clean, rest, sock_dir, budgets, &mut handoff) {
            Ok(prepared) => prepared,
            Err(ImageTargetError::AdmissionRefused) => {
                return format!("ERR image: {}\n", crate::control::ARTIFACT_HANDOFF_BUSY).into();
            }
            Err(ImageTargetError::AutomaticUnavailable) => {
                return "ERR image: could not create the automatic capture path\n".into();
            }
            Err(ImageTargetError::InvalidRequested) => {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("image write '{rest}'"),
                    aterm_containment::mode_or_containment(),
                    "path escapes images/ subdir or names a nested target",
                );
                return "ERR path: give a bare filename (no '/'), or omit it to auto-name one\n"
                    .into();
            }
        };
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    // A capture is a main-thread photograph: refused while the main thread is
    // stalled or a dialog stands, as every main-thread verb is,
    // and counted as a hop until the main thread takes it.
    let posted = post_main(proxy, |reply| {
        Wake::Image(Box::new(ImageReq {
            handoff,
            target,
            clean,
            session,
            want_bytes,
            want_metadata,
            frame_metadata: std::sync::Arc::clone(&frame_metadata),
            cancel,
            reply,
        }))
    });
    let (rx, _hop) = match posted {
        Ok(posted) => posted,
        Err(refusal) => return format!("ERR {refusal}\n").into(),
    };
    // Generous ceiling: the render + encode is tens of ms; the deadline only fires
    // when the event loop or worker is wedged, where blocking the control thread for
    // the client's whole 900 s exchange window would wedge the verb too.
    match recv_capture_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs(120),
        "image",
    ) {
        // (0,0): a window was found but had nothing to capture this frame, and NO
        // file was written. Report it as an error instead of an `OK 0 0 <path>`
        // pointing at nothing. No window at all is the render's own `Err`.
        Ok(retained) if matches!(retained.value, (0, 0, _)) => {
            "ERR image: nothing to capture; retry\n".into()
        }
        // `--bytes`: Lines-framed `OK 1\n<w> <h> <nbytes> <base64-png>` — the control
        // dispatch replies TEXT (written as UTF-8), so the binary PNG is base64'd
        // (the same wire-safe form `image read` uses for inline images). A REMOTE
        // driver base64-decodes to get the exact pixels, with no server-local file
        // path. Dims are on the line so the driver need not parse the PNG header.
        Ok(retained) => {
            let parts = retained.into_parts();
            let retention = parts.1;
            let (w, h, png) = parts.0;
            let body = match png {
                Some(png) => {
                    match image_bytes_reply(w, h, &png, want_metadata, frame_metadata.get()) {
                        Ok(body) => body,
                        Err(error) => return error.into(),
                    }
                }
                None => {
                    let Some(path) = path.as_deref() else {
                        return "ERR image: byte capture returned no PNG\n".into();
                    };
                    image_file_reply(w, h, path, want_metadata, frame_metadata.get())
                }
            };
            crate::control::ControlReply::with_handoff(body, retention)
        }
        // The encode/write failed AFTER a successful render: no file on disk.
        Err(e) => format!("ERR {e}\n").into(),
    }
}

fn image_metadata_fields(metadata: Option<&crate::control::ImageFrameMetadata>) -> String {
    metadata.map_or_else(
        || {
            "image-meta-version=3 frame-kind=unknown frame-phase=unknown identity=unavailable"
                .to_string()
        },
        crate::control::ImageFrameMetadata::wire_fields,
    )
}

// Width, height, byte-count fields, separators, and newline occupy at most 44
// bytes on supported targets. Reserve 64 so a padded base64 row at this raw PNG
// ceiling is always below the shared 8 MiB protocol-line limit.
const IMAGE_BYTE_ROW_OVERHEAD: usize = 64;
const MAX_IMAGE_BYTE_REPLY_PNG_BYTES: usize =
    ((aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES - IMAGE_BYTE_ROW_OVERHEAD) / 4) * 3;

fn image_byte_reply_png_fits(png_len: usize) -> bool {
    png_len <= MAX_IMAGE_BYTE_REPLY_PNG_BYTES
}

fn image_bytes_reply(
    width: u32,
    height: u32,
    png: &[u8],
    want_metadata: bool,
    metadata: Option<&crate::control::ImageFrameMetadata>,
) -> Result<String, String> {
    let too_large = || "ERR image: PNG too large to return as bytes\n".to_string();
    if !image_byte_reply_png_fits(png.len()) {
        return Err(too_large());
    }
    let encoded_len = aterm_codec::base64::encoded_len(png.len()).ok_or_else(too_large)?;
    if want_metadata {
        let metadata = image_metadata_fields(metadata);
        if metadata.len().saturating_add("image-meta \n".len())
            > aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES
        {
            return Err("ERR image: metadata too large to return as bytes\n".to_string());
        }
        let mut body = String::with_capacity(
            encoded_len
                .saturating_add(IMAGE_BYTE_ROW_OVERHEAD)
                .saturating_add(metadata.len())
                .saturating_add("image-meta \n".len()),
        );
        write!(
            &mut body,
            "OK 2\nimage-meta {metadata}\n{width} {height} {} ",
            png.len(),
        )
        .expect("writing to a String cannot fail");
        aterm_codec::base64::encode_into(&mut body, png).map_err(|_| too_large())?;
        body.push('\n');
        Ok(body)
    } else {
        let mut body = String::with_capacity(encoded_len.saturating_add(IMAGE_BYTE_ROW_OVERHEAD));
        write!(&mut body, "OK 1\n{width} {height} {} ", png.len())
            .expect("writing to a String cannot fail");
        aterm_codec::base64::encode_into(&mut body, png).map_err(|_| too_large())?;
        body.push('\n');
        Ok(body)
    }
}

fn image_file_reply(
    width: u32,
    height: u32,
    path: &str,
    want_metadata: bool,
    metadata: Option<&crate::control::ImageFrameMetadata>,
) -> String {
    if want_metadata {
        format!(
            "OK {width} {height} image-meta {} path={path:?}\n",
            image_metadata_fields(metadata),
        )
    } else {
        format!("OK {width} {height} {path}\n")
    }
}

/// The refusal of `window <word>` when `<word>` is neither a target nor a file name:
/// `None` for the empty operand and for anything path-shaped — an extension
/// (`shot.png`, `front.png`) or a separator, which the path confinement then judges —
/// so both keep their meaning. The refusal names the targets and the `.png`
/// spelling that writes a file of that name.
fn unknown_window_target(first: &str) -> Option<String> {
    if first.is_empty() || first.contains(['.', '/', '\\']) {
        return None;
    }
    Some(format!(
        "ERR unknown window target '{first}' (targets: {}); a file name has an extension: \
         `window {first}.png`\n",
        aterm_types::control_verbs::WINDOW_TARGETS
    ))
}

/// The name a `window <target>` capture is written under when no path is
/// given: `aterm-<keyword>`, and `aterm-window` for the front window.
fn default_stem(aux: crate::app_introspect::AuxTarget) -> String {
    match aux {
        crate::app_introspect::AuxTarget::Front => "aterm-window".to_string(),
        target => format!("aterm-{}", target.keyword()),
    }
}

/// `window [<target>] [path]` -> capture a full-window artifact to a PNG,
/// replying `OK <w> <h> <path>` (the SAME wire shape as `image`). For the front
/// terminal window this stitches platform chrome around the exact submitted
/// client destination. It does not claim compositor visibility or scanout, and
/// refuses capture when translucent/material pixels cannot be represented
/// honestly. `<target>` is an optional leading keyword selecting WHICH window:
///   * (omitted) / `front` — the frontmost TERMINAL window: native macOS chrome
///     (titlebar, traffic lights, unified toolbar, full-width tab strip) AND the
///     terminal content (`image` rasterizes only the content framebuffer, no OS
///     chrome).
///   * `prefs` / `settings` — the Settings surface.
///   * `about` / `update` / `menu` / `tab-menu` / `overview` — a
///     Settings route or an overlay of the front window.
///
/// Every target but `front` is photographed IN the front window, so one the front
/// window is not showing is REFUSED (`App::window_target_refusal`, asked on the main
/// thread by `capture_aux_window`) rather than answered with a plain front frame under
/// the target's name.
///
/// A first token that is not a known keyword is treated as the path (`window [path]`)
/// when it looks like one — it has an extension (`shot.png`); a bare word is refused
/// instead ([`unknown_window_target`]). A literal filename `prefs`/`front` must
/// therefore be given a target first (e.g. `window front prefs`).
///
/// PATH CONFINEMENT (mirrors [`cmd_image`]): the `path` is validated by
/// `confine_image_path` to a single filename inside the socket dir's `images/` subdir,
/// so the socket can never overwrite an arbitrary file. Omitted names are unique
/// inside this process instance's bounded automatic-output namespace.
///
/// MAIN-THREAD HOP (mirrors [`cmd_chrome`]): reaching a window's `NSWindow` + reading its
/// window number + calling `CGWindowListCreateImage` may ONLY happen on the main thread,
/// but this runs on a background control thread. So we post [`Wake::CaptureWindow`]
/// (front) or [`Wake::CaptureAuxWindow`] (Settings routes) with the confined target + a
/// one-shot result channel and BLOCK. The main thread captures, the encode worker writes
/// and transfers a guarded `Ok((w, h))`, and the socket server retains it through the
/// client's explicit complete-response ACK; an `Err(msg)` is surfaced verbatim (window
/// not on screen / target not open / off macOS and Windows — never a missing Screen
/// Recording grant: aterm photographs its OWN window, which needs none;
/// `capture_window_pixels`).
pub(crate) fn cmd_window(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
) -> crate::control::ControlReply {
    use crate::app_introspect::AuxTarget;
    // Optional leading target keyword: `window [front|prefs] [path]`. A first token
    // that is not a known keyword is the PATH (default front), preserving `window [path]`.
    let mut it = rest.split_whitespace();
    let first = it.next().unwrap_or("");
    let (aux, path_arg) = match AuxTarget::parse(first) {
        Some(t) if !first.is_empty() => (t, it.next().unwrap_or("")),
        _ => {
            if let Some(refusal) = unknown_window_target(first) {
                return refusal.into();
            }
            (AuxTarget::Front, rest.trim())
        }
    };
    let budgets = crate::control::ArtifactBudgets::process();
    let Some(mut handoff) = budgets.try_reserve_for_path(sock_dir) else {
        return format!(
            "ERR window capture: {}\n",
            crate::control::ARTIFACT_HANDOFF_BUSY
        )
        .into();
    };
    let p = path_arg.trim();
    let confined = if p.is_empty() {
        match control_auth::confine_automatic_image_path_with_admission(
            sock_dir,
            &default_stem(aux),
            budgets,
            |path| handoff.try_reconcile_for_resolved_path(path),
        ) {
            Ok(target) => target,
            Err(control_auth::ArtifactConfinementError::AdmissionRefused) => {
                return format!(
                    "ERR window capture: {}\n",
                    crate::control::ARTIFACT_HANDOFF_BUSY
                )
                .into();
            }
            Err(control_auth::ArtifactConfinementError::Invalid) => {
                return "ERR window: could not create the automatic capture path\n".into();
            }
        }
    } else {
        match control_auth::confine_image_path_with_admission(sock_dir, p, |path| {
            handoff.try_reconcile_for_resolved_path(path)
        }) {
            Ok(target) => target,
            Err(control_auth::ArtifactConfinementError::AdmissionRefused) => {
                return format!(
                    "ERR window capture: {}\n",
                    crate::control::ARTIFACT_HANDOFF_BUSY
                )
                .into();
            }
            Err(control_auth::ArtifactConfinementError::Invalid) => {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("window write '{p}'"),
                    aterm_containment::mode_or_containment(),
                    "path escapes images/ subdir or names a nested target",
                );
                return "ERR path: give a bare filename (no '/'), or omit it to auto-name one\n"
                    .into();
            }
        }
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = confined.display_path().to_string_lossy().into_owned();
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    // Refused while the main thread is stalled or a dialog stands, as every
    // main-thread verb is. Front uses `CaptureWindow`; aux windows use
    // `CaptureAuxWindow` (resolved by their own window number on the main thread).
    let posted = post_main(proxy, |reply| match aux {
        AuxTarget::Front => Wake::CaptureWindow {
            path: confined,
            handoff,
            cancel,
            reply,
        },
        _ => Wake::CaptureAuxWindow {
            target: aux,
            path: confined,
            handoff,
            cancel,
            reply,
        },
    });
    let (rx, _hop) = match posted {
        Ok(posted) => posted,
        Err(refusal) => return format!("ERR {refusal}\n").into(),
    };
    // Same wedge guard as `cmd_image`: the photograph + encode is fast; only a
    // stuck main thread or dead worker reaches the deadline.
    match recv_capture_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs(120),
        "window",
    ) {
        Ok(retained) => {
            let ((w, h), retention) = retained.into_parts();
            crate::control::ControlReply::with_handoff(format!("OK {w} {h} {path}\n"), retention)
        }
        // The main thread's clear, actionable message (headless / no on-screen window
        // number / not on screen / geometry moved mid-capture / cancelled / off-platform)
        // is surfaced as a single `ERR`. Never a missing permission: aterm photographs
        // its OWN window, which needs no Screen Recording grant (`capture_window_pixels`).
        Err(msg) => format!("ERR {msg}\n").into(),
    }
}

/// The platforms a `window` photograph is taken on (`App::run_capture`, its one
/// caller), plus tests.
#[cfg(any(target_os = "macos", windows, test))]
impl crate::App {
    /// `window <target>`'s refusal when the front window is not showing `target`,
    /// or `None` when it is (always for `front`). Asked on the main thread in the
    /// same turn as the capture attempt (`App::run_capture`), so a photograph
    /// taken in that turn cannot outlive the answer. A photograph deferred for its
    /// GPU drawable carries its target and re-enters the same door in the later
    /// turn that replays it (`resume_deferred_gpu_captures`), so an overlay
    /// dismissed while it waited is refused.
    ///
    /// Every target lives in the front frame, so capturing a closed overlay would
    /// return a plain terminal frame under the overlay's name.
    ///
    /// "Showing" is the photograph's question, not `controls`': Settings counts
    /// only as the front window's active tab (`controls prefs` also reads one in a
    /// background tab or another window, which the capture cannot see), and
    /// `about` / `update` only on their own route. The action names the verb that
    /// opens the target.
    pub(crate) fn window_target_refusal(
        &self,
        target: crate::app_introspect::AuxTarget,
    ) -> Option<String> {
        use crate::app_introspect::AuxTarget;
        use crate::native_settings::SettingsRoute;
        let shown = match target {
            AuxTarget::Front => return None,
            AuxTarget::Prefs => self.native_settings_front_view_target().is_some(),
            AuxTarget::About => self
                .native_settings_route_view_target(SettingsRoute::About)
                .is_some(),
            AuxTarget::Update => self
                .native_settings_route_view_target(SettingsRoute::SoftwareUpdate)
                .is_some(),
            AuxTarget::Menu => self.front().is_some_and(|ws| ws.palette().is_some()),
            AuxTarget::TabMenu => self.front().is_some_and(|ws| ws.tab_menu.is_some()),
            AuxTarget::AutopilotMenu => self
                .autopilot_menu
                .as_ref()
                .is_some_and(|m| Some(m.wid) == self.frontmost_window),
            AuxTarget::Overview => self.front().is_some_and(|ws| ws.overview().is_some()),
        };
        if shown {
            return None;
        }
        let name = target.keyword();
        Some(format!(
            "{name} is not open in the front window; use `open {name}` first"
        ))
    }
}

/// `window <target>` refuses a target the front window is not showing and
/// photographs one it is. Driven through [`crate::App::window_target_refusal`],
/// the question the main thread asks, because the photograph itself needs a live
/// window a unit test cannot construct; the last test drives `capture_aux_window`
/// through the capture door to show the question is asked before admission.
#[cfg(test)]
mod window_target_refusal_tests {
    use crate::app_introspect::AuxTarget;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId};

    /// A bare word that is no target is refused, listing every target `window`
    /// takes and the spelling that writes a file of that name; the empty operand
    /// and anything path-shaped keep their meaning.
    #[test]
    fn a_bare_word_that_is_no_target_is_refused_not_written() {
        let refusal = super::unknown_window_target("bogus").expect("a bare word is refused");
        assert!(refusal.starts_with("ERR "), "{refusal}");
        // In the table's order, and exactly the parser's targets.
        let listed: Vec<&str> = refusal
            .split_once("(targets: ")
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(list, _)| list.split(" | ").collect())
            .expect("the refusal lists the targets");
        assert_eq!(
            listed,
            AuxTarget::ALL
                .iter()
                .map(|target| target.keyword())
                .collect::<Vec<_>>()
        );
        for operand in ["", "shot.png", "front.png", "sub/x", r"sub\x"] {
            assert_eq!(super::unknown_window_target(operand), None, "{operand:?}");
        }
    }

    /// `help open` names the targets `open` takes, from the one table the
    /// parser reads (`control_verbs::aux_targets!`): each is opened, and each
    /// target it does not take (`front`) is refused with a
    /// line naming that list; `close` and an alias resolve.
    #[test]
    fn help_open_names_every_target_open_takes() {
        use crate::app_introspect::AuxTarget;
        use aterm_types::control_verbs::{OPEN_TARGETS, spec};
        let help = spec("open").expect("the open verb").help_line();
        assert!(help.contains(OPEN_TARGETS), "{help}");
        assert!(OPEN_TARGETS.contains("autopilot-menu"), "{OPEN_TARGETS}");
        for &target in AuxTarget::ALL {
            let word = target.keyword();
            match super::open_target(word) {
                Ok(taken) => assert_eq!(taken, (target, false), "{word}"),
                Err(refusal) => {
                    assert!(!target.opens(), "{word} is opened but refused: {refusal}");
                    assert!(refusal.contains(OPEN_TARGETS), "{word}: {refusal}");
                }
            }
        }
        assert_eq!(
            super::open_target("autopilot-menu close"),
            Ok((AuxTarget::AutopilotMenu, true))
        );
        assert_eq!(
            super::open_target("settings").map(|(t, _)| t),
            Ok(AuxTarget::Prefs),
            "an alias resolves"
        );
    }

    /// Each overlay, once open on the front window, is photographed — and only
    /// that one: the refusal reads the surface, not a global "something is open".
    #[test]
    fn an_open_overlay_is_photographed_and_only_that_one() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let shown = |app: &App| {
            AuxTarget::ALL
                .iter()
                .copied()
                .filter(|&t| t != AuxTarget::Front && app.window_target_refusal(t).is_none())
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&app), vec![], "nothing is open yet");

        app.palette_enter();
        assert_eq!(shown(&app), vec![AuxTarget::Menu]);
        app.palette_exit();

        assert!(app.open_active_tab_context_menu(wid));
        assert_eq!(shown(&app), vec![AuxTarget::TabMenu]);
        assert!(app.close_tab_menu(wid));

        assert!(app.open_overview().is_ok());
        assert_eq!(shown(&app), vec![AuxTarget::Overview]);
        app.overview_exit(wid);

        assert_eq!(shown(&app), vec![], "every overlay closed again");
    }

    /// Settings is photographed only as the front window's ACTIVE tab, and
    /// `about` / `update` only on their own route. The background case is the
    /// one `controls prefs` answers differently — it reads a Settings tab the
    /// photograph cannot see — so the refusal must not borrow that answer.
    #[test]
    fn settings_counts_only_as_the_active_tab_on_its_own_route() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(SettingsRoute::Home));
        assert_eq!(app.window_target_refusal(AuxTarget::Prefs), None);
        assert!(
            app.window_target_refusal(AuxTarget::About).is_some(),
            "Settings on its home route is not the about page"
        );
        assert!(app.window_target_refusal(AuxTarget::Update).is_some());

        assert!(app.open_settings_tab(SettingsRoute::About));
        assert_eq!(app.window_target_refusal(AuxTarget::About), None);
        assert_eq!(
            app.window_target_refusal(AuxTarget::Prefs),
            None,
            "any route is the Settings surface"
        );

        app.switch_tab(0);
        assert!(
            app.read_aux_controls(AuxTarget::Prefs)[0].starts_with("state open=true"),
            "`controls prefs` still reads the background Settings tab"
        );
        assert!(
            app.window_target_refusal(AuxTarget::Prefs).is_some(),
            "but the front window shows the terminal"
        );
    }

    /// The photograph asks the question before it is taken: a target the front
    /// window does not show is answered with the refusal before the capture is
    /// admitted, so no body runs. The negative control
    /// is the same request once the target is shown: it passes the door and its
    /// body runs (and, headless, answers that there is no window to photograph).
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn aux_capture_refuses_before_admission() {
        let photograph = |app: &mut App, target| {
            let bodies = crate::app_introspect::capture_bodies();
            let (reply, rx) = std::sync::mpsc::channel();
            app.capture_aux_window(
                target,
                crate::control::ReplyRetentionPermit::from_own_budget(),
                crate::control_auth::ConfinedImage::in_memory(),
                crate::control::CaptureCancellation::new(),
                reply,
            );
            let answer = rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("the capture answers")
                .expect_err("a headless app photographs nothing");
            (answer, crate::app_introspect::capture_bodies() - bodies)
        };
        let mut app = App::headless_for_test();
        let (refused, admitted) = photograph(&mut app, AuxTarget::Prefs);
        assert_eq!(
            refused,
            "prefs is not open in the front window; use `open prefs` first"
        );
        assert_eq!(admitted, 0, "refused before admission");

        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (answer, admitted) = photograph(&mut app, AuxTarget::Prefs);
        assert_eq!(admitted, 1, "shown, the capture is admitted: {answer}");
        assert_eq!(answer, "no window to capture (headless)");
    }
}

/// Parsed `video` verb arguments, every numeric already CLAMPED to its lawful
/// range (secs 0.5..=60, fps 1..=120, budget 64..=4096 MiB) so the consumer
/// never re-validates. Split from [`cmd_video`] so the parse laws are pure
/// unit-testable (no event loop needed).
#[derive(Debug)]
struct VideoArgs {
    secs: f64,
    full_res: bool,
    keys: bool,
    pace: bool,
    /// `fps=<n>` capture cap; `None` captures every present.
    fps: Option<u32>,
    /// Frame-store RAM budget (from `budget=<MiB>`; default 512 MiB).
    budget_bytes: usize,
}

/// The longest take `video` records: a longer request is clamped to it. The
/// seamless update's park waits for a live take at most this long
/// (`app_update_handoff::VIDEO_PARK_WAIT_MAX`).
pub(crate) const VIDEO_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(60);

const VIDEO_USAGE: &str = "usage: video [<seconds>] [full|half] [keys] [pace] [fps=<n>] [budget=<MiB>] (seconds default 3) | video status|stop | video frames [count=N]";

/// Default / max frames returned by `video frames` (the top-delta key frames).
const VIDEO_FRAMES_DEFAULT: usize = 8;
const VIDEO_FRAMES_MAX: usize = 64;

/// A healthy per-instance namespace converges to eight completed recordings.
/// Bound the directory walk well above that so a planted or crash-filled
/// namespace cannot make one control request scan an unbounded number of names.
const VIDEO_RECORDING_SCAN_MAX: usize = 128;

/// A 60 s / 120 fps recording plus the bounded 1024-input ledger is comfortably
/// below this. Read `MAX + 1` so an oversized regular file fails closed without
/// allocating its attacker-controlled length.
const VIDEO_INDEX_MAX_BYTES: usize = 4 * 1024 * 1024;

fn valid_recording_name(name: &str) -> bool {
    let Some(stamp) = name.strip_prefix("rec-") else {
        return false;
    };
    !stamp.is_empty()
        && stamp
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn valid_frame_file_name(file: &str) -> bool {
    if !crate::pinned_dir::is_one_component(std::ffi::OsStr::new(file)) {
        return false;
    }
    let Some(number) = file
        .strip_prefix("frame_")
        .and_then(|tail| tail.strip_suffix(".png"))
    else {
        return false;
    };
    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Debug)]
struct VideoFrameCandidate {
    delta: u64,
    n: u64,
    seq: u64,
    t_us: u64,
    file: String,
}

#[derive(Clone, Copy, Debug)]
struct BorrowedVideoFrameCandidate<'a> {
    delta: u64,
    n: u64,
    seq: u64,
    t_us: u64,
    file: &'a str,
    ordinal: usize,
}

impl BorrowedVideoFrameCandidate<'_> {
    fn into_owned(self) -> VideoFrameCandidate {
        VideoFrameCandidate {
            delta: self.delta,
            n: self.n,
            seq: self.seq,
            t_us: self.t_us,
            file: self.file.to_string(),
        }
    }
}

fn video_frame_candidate(
    frame: &aterm_json::Value,
    ordinal: usize,
) -> Option<BorrowedVideoFrameCandidate<'_>> {
    let file = frame.get("file")?.as_str()?;
    if !valid_frame_file_name(file) {
        return None;
    }
    Some(BorrowedVideoFrameCandidate {
        delta: frame.get("delta")?.as_u64()?,
        n: frame.get("n")?.as_u64()?,
        seq: frame.get("seq")?.as_u64()?,
        t_us: frame.get("t_us")?.as_u64()?,
        file,
        ordinal,
    })
}

fn video_frame_rank(
    left: &BorrowedVideoFrameCandidate<'_>,
    right: &BorrowedVideoFrameCandidate<'_>,
) -> std::cmp::Ordering {
    right
        .delta
        .cmp(&left.delta)
        .then(left.n.cmp(&right.n))
        .then(left.ordinal.cmp(&right.ordinal))
}

/// Select the highest-ranked manifest entries while allocating only `limit`
/// borrowed records. The full borrowed sort is reserved for damaged recordings
/// whose highest-ranked files cannot be opened.
fn video_frame_candidates(
    index: &aterm_json::Value,
    limit: usize,
) -> Vec<BorrowedVideoFrameCandidate<'_>> {
    if limit == 0 {
        return Vec::new();
    }
    let Some(frames) = index.get("frames").and_then(aterm_json::Value::as_array) else {
        return Vec::new();
    };
    let mut candidates = Vec::with_capacity(limit.min(frames.len()));
    for (ordinal, frame) in frames.iter().enumerate() {
        let Some(candidate) = video_frame_candidate(frame, ordinal) else {
            continue;
        };
        let position = candidates.partition_point(|existing| {
            video_frame_rank(existing, &candidate) != std::cmp::Ordering::Greater
        });
        if position < limit {
            candidates.insert(position, candidate);
            if candidates.len() > limit {
                candidates.pop();
            }
        }
    }
    candidates
}

struct PinnedVideoFrame {
    candidate: VideoFrameCandidate,
    path: std::path::PathBuf,
    identity: crate::pinned_dir::PinnedFileIdentity,
}

fn pin_video_frame_candidate(
    recording: &crate::pinned_dir::PinnedDir,
    recording_path: &std::path::Path,
    candidate: BorrowedVideoFrameCandidate<'_>,
    after_pin: &mut impl FnMut(),
) -> Option<PinnedVideoFrame> {
    let identity = recording
        .private_file_identity_at_retained(std::ffi::OsStr::new(candidate.file))
        .ok()?;
    after_pin();
    let path = recording_path.join(candidate.file);
    Some(PinnedVideoFrame {
        candidate: candidate.into_owned(),
        path,
        identity,
    })
}

fn collect_video_frame_rows(
    recording: &crate::pinned_dir::PinnedDir,
    recording_path: &std::path::Path,
    index: &aterm_json::Value,
    count: usize,
    mut after_pin: impl FnMut(),
) -> Option<Vec<PinnedVideoFrame>> {
    recording.validate_path_identity().ok()?;
    let mut rows = Vec::with_capacity(count.min(64));
    let candidates = video_frame_candidates(index, count);
    let attempted = candidates.len();
    for candidate in candidates {
        if let Some(row) =
            pin_video_frame_candidate(recording, recording_path, candidate, &mut after_pin)
        {
            rows.push(row);
        }
    }

    if rows.len() < count && attempted == count {
        let frames = index
            .get("frames")
            .and_then(aterm_json::Value::as_array)
            .expect("bounded selection found candidates only in a frames array");
        let mut fallback = frames
            .iter()
            .enumerate()
            .filter_map(|(ordinal, frame)| video_frame_candidate(frame, ordinal))
            .collect::<Vec<_>>();
        fallback.sort_unstable_by(video_frame_rank);
        for candidate in fallback.into_iter().skip(attempted) {
            if let Some(row) =
                pin_video_frame_candidate(recording, recording_path, candidate, &mut after_pin)
            {
                rows.push(row);
                if rows.len() == count {
                    break;
                }
            }
        }
    }
    Some(rows)
}

fn format_video_frame_rows(rows: &[PinnedVideoFrame]) -> String {
    // Four maximum-width u64 fields plus labels/newline need at most 108 bytes.
    // Add the path's encoded length and a fixed-width header so the common case
    // performs one response allocation and writes directly into it.
    let capacity = rows.iter().fold(24usize, |capacity, row| {
        capacity
            .saturating_add(108)
            .saturating_add(row.path.as_os_str().len())
    });
    let mut out = String::with_capacity(capacity);
    writeln!(&mut out, "OK {}", rows.len()).expect("writing to a String cannot fail");
    for row in rows {
        let VideoFrameCandidate {
            delta,
            n,
            seq,
            t_us,
            ..
        } = &row.candidate;
        // Absolute path so a local AI can open the PNG directly (same server-local
        // convention as `image <path>`).
        //
        // The identity checkpoint below makes this path name the validated file
        // at the response commit point. As with every filesystem-path API, a
        // same-user process can mutate it immediately after that checkpoint,
        // including before the caller receives the bytes.
        writeln!(
            &mut out,
            "frame n={n} delta={delta} t_us={t_us} seq={seq} {}",
            row.path.display()
        )
        .expect("writing to a String cannot fail");
    }
    out
}

#[cfg(any(unix, windows))]
mod confined_video_reader {
    use super::{
        PinnedVideoFrame, VIDEO_INDEX_MAX_BYTES, VIDEO_RECORDING_SCAN_MAX,
        collect_video_frame_rows, control_auth, format_video_frame_rows, valid_recording_name,
    };
    use crate::pinned_dir::{PinnedDir, PinnedFile};

    const INDEX_NAME: &str = "index.json";

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum ReadStage {
        SocketPathResolved,
        NamespacePinned,
        RecordingPinned,
        IndexPinned,
        FramePinned,
        SelectionValidated,
    }

    #[cfg(not(test))]
    #[derive(Clone, Copy)]
    enum ReadStage {
        SocketPathResolved,
        NamespacePinned,
        RecordingPinned,
        IndexPinned,
        FramePinned,
        SelectionValidated,
    }

    fn open_namespace(
        sock_dir: &std::path::Path,
        instance: &str,
        admit: impl FnOnce(&std::path::Path) -> bool,
        hook: &mut impl FnMut(ReadStage),
    ) -> Result<Option<PinnedDir>, &'static str> {
        if control_auth::video_instance_root_for(sock_dir, instance).is_none() {
            return Err("recording namespace is not confined");
        }
        let socket = PinnedDir::open_resolved_with_admission(sock_dir, |canonical_socket| {
            admit(
                &canonical_socket
                    .join(control_auth::VIDEO_DIR)
                    .join(instance),
            )
        })
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                crate::control::ARTIFACT_HANDOFF_BUSY
            } else {
                "recording namespace is not confined"
            }
        })?;
        hook(ReadStage::SocketPathResolved);
        socket
            .validate_path_identity()
            .map_err(|_| "recording namespace is not confined")?;
        // Until this launch's first `video` there is no `video/<instance>`, and on a fresh
        // machine no `video/`. Absence counts only while the parent still validates: a
        // swapped chain can surface as NotFound too, and stays a refusal.
        let video = match socket.child(std::ffi::OsStr::new(control_auth::VIDEO_DIR)) {
            Ok(video) => video,
            Err(error) => return absent_or_unconfined(&error, &socket),
        };
        match video.child(std::ffi::OsStr::new(instance)) {
            Ok(namespace) => Ok(Some(namespace)),
            Err(error) => absent_or_unconfined(&error, &video),
        }
    }

    /// A child of `parent` that could not be opened: `None` (nothing recorded yet) when it
    /// does not exist and `parent` is still the directory its path names, else the refusal.
    fn absent_or_unconfined(
        error: &std::io::Error,
        parent: &PinnedDir,
    ) -> Result<Option<PinnedDir>, &'static str> {
        if error.kind() == std::io::ErrorKind::NotFound && parent.validate_path_identity().is_ok() {
            Ok(None)
        } else {
            Err("recording namespace is not confined")
        }
    }

    fn recording_names(instance: &PinnedDir) -> Result<Vec<String>, &'static str> {
        let names = instance
            .names_up_to(VIDEO_RECORDING_SCAN_MAX.saturating_add(1))
            .map_err(|_| "could not read recording namespace")?;
        if names.len() > VIDEO_RECORDING_SCAN_MAX {
            return Err("recording namespace has too many entries");
        }
        let mut recordings = names
            .into_iter()
            .filter_map(|name| name.into_string().ok())
            .filter(|name| valid_recording_name(name))
            .collect::<Vec<_>>();
        recordings.sort();
        Ok(recordings)
    }

    fn read_index(
        recording: &PinnedDir,
        hook: &mut impl FnMut(ReadStage),
    ) -> Option<(PinnedFile, aterm_json::Value)> {
        let (bytes, index_file) = recording
            .read_private_at_retained(std::ffi::OsStr::new(INDEX_NAME), VIDEO_INDEX_MAX_BYTES)
            .ok()?;
        hook(ReadStage::IndexPinned);
        let value = aterm_json::from_slice::<aterm_json::Value>(&bytes).ok()?;
        value
            .get("frames")
            .is_some_and(aterm_json::Value::is_array)
            .then_some((index_file, value))
    }

    fn selection_still_matches(
        recording: &PinnedDir,
        published_marker: &PinnedFile,
        index_file: &PinnedFile,
        rows: &[PinnedVideoFrame],
    ) -> bool {
        recording.validate_path_identity().is_ok()
            && published_marker
                .validate_entry_identity_at_retained()
                .is_ok()
            && index_file.validate_entry_identity_at_retained().is_ok()
            && rows.iter().all(|row| {
                row.identity
                    .validate_at_retained(recording, std::ffi::OsStr::new(&row.candidate.file))
                    .is_ok()
            })
            && recording.validate_path_identity().is_ok()
    }

    struct VideoFramesRetention {
        recording: PinnedDir,
        published_marker: PinnedFile,
        index_file: PinnedFile,
        rows: Vec<PinnedVideoFrame>,
        _lease: control_auth::ArtifactPathLease,
    }

    impl crate::control::WireRetention for VideoFramesRetention {
        fn prepare_write(&mut self) -> Result<(), String> {
            selection_still_matches(
                &self.recording,
                &self.published_marker,
                &self.index_file,
                &self.rows,
            )
            .then_some(())
            .ok_or_else(|| "video frame identity changed before wire reply".to_string())
        }
    }

    fn read_with_permit_and_hook(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        budgets: &crate::control::ArtifactBudgets,
        mut permit: crate::control::ReplyRetentionPermit,
        mut hook: impl FnMut(ReadStage),
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        let Some(instance_dir) = open_namespace(
            sock_dir,
            instance,
            |path| permit.try_reconcile_for_resolved_path(path),
            &mut hook,
        )?
        else {
            return Ok(None);
        };
        hook(ReadStage::NamespacePinned);
        let names = recording_names(&instance_dir)?;

        // A still-encoding or torn newest recording is skipped. Walk newest to
        // oldest until both the publication marker and bounded completion index
        // are pinned through the one retained namespace chain.
        let mut selected = None;
        for recording_name in names.into_iter().rev() {
            let Ok(recording) = instance_dir.child(std::ffi::OsStr::new(&recording_name)) else {
                continue;
            };
            hook(ReadStage::RecordingPinned);
            let Ok(published_marker) = recording.pin_private_file_at_retained(
                std::ffi::OsStr::new(control_auth::VIDEO_PUBLISHED_FILE),
            ) else {
                continue;
            };
            let Some((index_file, index)) = read_index(&recording, &mut hook) else {
                continue;
            };
            selected = Some((
                std::ffi::OsString::from(recording_name),
                recording,
                published_marker,
                index_file,
                index,
            ));
            break;
        }
        let Some((recording_name, recording, published_marker, index_file, index)) = selected
        else {
            return Ok(None);
        };

        let Some(lease) = control_auth::retain_video_artifact_path(
            instance_dir,
            recording_name,
            &recording,
            budgets,
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                crate::control::ARTIFACT_HANDOFF_BUSY
            } else {
                "recording namespace changed while acquiring retention"
            }
        })?
        else {
            return Err("recording retention sweep is in progress");
        };
        let Some(rows) =
            collect_video_frame_rows(&recording, recording.path(), &index, count, || {
                hook(ReadStage::FramePinned)
            })
        else {
            return Err("recording namespace changed during read");
        };
        if !selection_still_matches(&recording, &published_marker, &index_file, &rows) {
            return Err("recording namespace changed during read");
        }
        hook(ReadStage::SelectionValidated);

        lease
            .arm_video_retention_sweep(&recording)
            .map_err(|_| "recording namespace changed while arming retention")?;
        let guard = VideoFramesRetention {
            recording,
            published_marker,
            index_file,
            rows,
            _lease: lease,
        };
        let output = format_video_frame_rows(&guard.rows);
        let retention = permit.retain(guard);
        Ok(Some(crate::control::Retained::guarded(output, retention)))
    }

    fn read_with_hook(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        budgets: &crate::control::ArtifactBudgets,
        hook: impl FnMut(ReadStage),
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        let permit = budgets
            .try_reserve_for_path(sock_dir)
            .ok_or(crate::control::ARTIFACT_HANDOFF_BUSY)?;
        read_with_permit_and_hook(sock_dir, instance, count, budgets, permit, hook)
    }

    pub(super) fn read(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        budgets: &crate::control::ArtifactBudgets,
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        read_with_hook(sock_dir, instance, count, budgets, |_| {})
    }

    /// A read with `hook` at each stage, on a budget of its own.
    #[cfg(all(test, unix))]
    pub(super) fn read_for_test(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        hook: impl FnMut(ReadStage),
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        read_with_hook(
            sock_dir,
            instance,
            count,
            &crate::control::ArtifactBudgets::shipping(),
            hook,
        )
    }
}

#[cfg(not(any(unix, windows)))]
mod confined_video_reader {
    pub(super) fn read(
        _sock_dir: &std::path::Path,
        _instance: &str,
        _count: usize,
        _budgets: &crate::control::ArtifactBudgets,
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        Err("safe recording reads are unsupported on this platform")
    }
}

/// `video frames [count=N]` — read the newest finished recording's `index.json`
/// and emit its N highest-`delta` frames as `OK <n>` + n `frame …` rows. The
/// `delta` (the 8×8 tiles that differ from the previous captured frame) is
/// already in the index, so this is a cheap read that hands an AI just the
/// eventful frames (the visual key moments) instead of the whole PNG sequence.
fn video_frames<'a>(
    sock_dir: &std::path::Path,
    args: impl Iterator<Item = &'a str>,
) -> crate::control::ControlReply {
    video_frames_for_instance(
        sock_dir,
        control_auth::process_instance_id(),
        args,
        crate::control::ArtifactBudgets::process(),
    )
}

fn video_frames_for_instance<'a>(
    sock_dir: &std::path::Path,
    instance: &str,
    args: impl Iterator<Item = &'a str>,
    budgets: &crate::control::ArtifactBudgets,
) -> crate::control::ControlReply {
    let mut count = VIDEO_FRAMES_DEFAULT;
    for t in args {
        if let Some(v) = t.strip_prefix("count=") {
            match v.parse::<usize>() {
                Ok(n) if n >= 1 => count = n.min(VIDEO_FRAMES_MAX),
                _ => {
                    return format!("ERR video frames: bad count '{t}' (1..={VIDEO_FRAMES_MAX})\n")
                        .into();
                }
            }
        } else {
            return format!(
                "ERR video frames: unknown arg '{t}' (usage: video frames [count=N])\n"
            )
            .into();
        }
    }
    match confined_video_reader::read(sock_dir, instance, count, budgets) {
        Ok(Some(output)) => {
            let (body, retention) = output.into_parts();
            crate::control::ControlReply::with_handoff(body, retention)
        }
        Ok(None) => {
            "ERR video frames: no finished recording found (run `video <seconds>` first)\n".into()
        }
        Err(error) => format!("ERR video frames: {error}\n").into(),
    }
}

/// Parse the `video` verb's argument tail. Unknown tokens and malformed
/// `fps=`/`budget=` values are REJECTED with the usage line (never silently
/// defaulted); out-of-range numbers are clamped, not rejected.
fn parse_video_args(rest: &str) -> Result<VideoArgs, String> {
    let mut args = VideoArgs {
        secs: 3.0,
        full_res: false,
        keys: false,
        pace: false,
        fps: None,
        budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
    };
    for tok in rest.split_whitespace() {
        match tok {
            "full" => args.full_res = true,
            "half" => args.full_res = false,
            "keys" => args.keys = true,
            "pace" => args.pace = true,
            t => {
                if let Some(v) = t.strip_prefix("fps=") {
                    match v.parse::<u32>() {
                        Ok(n) => args.fps = Some(n.clamp(1, 120)),
                        Err(_) => {
                            return Err(format!("ERR video: bad fps '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else if let Some(v) = t.strip_prefix("budget=") {
                    match v.parse::<u64>() {
                        Ok(mib) => args.budget_bytes = (mib.clamp(64, 4096) as usize) << 20,
                        Err(_) => {
                            return Err(format!("ERR video: bad budget '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else {
                    match t.parse::<f64>() {
                        Ok(s) if s.is_finite() => args.secs = s,
                        Ok(_) => {
                            return Err(format!(
                                "ERR video: non-finite duration '{t}' ({VIDEO_USAGE})\n"
                            ));
                        }
                        Err(_) => {
                            return Err(format!("ERR video: unknown arg '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                }
            }
        }
    }
    args.secs = args.secs.clamp(0.5, VIDEO_MAX_DURATION.as_secs_f64());
    Ok(args)
}

/// Cancels the request on every control-handler exit except a completed reply.
/// This covers timeout, disconnect/unwind, and event-loop send failure without
/// relying on each return site to remember the cross-thread signal.
struct CancelVideoRequestOnDrop(Option<crate::VideoCancellation>);

impl CancelVideoRequestOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }

    fn cancel_won(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(crate::VideoCancellation::cancel)
    }
}

impl Drop for CancelVideoRequestOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.cancel();
        }
    }
}

fn recv_video_reply(
    rx: &std::sync::mpsc::Receiver<crate::control::Retained<String>>,
    cancel_on_drop: &mut CancelVideoRequestOnDrop,
    timeout: std::time::Duration,
) -> Result<crate::control::Retained<String>, String> {
    match rx.recv_timeout(timeout) {
        Ok(reply) => {
            cancel_on_drop.disarm();
            Ok(reply)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancel_on_drop.cancel_won() => {
            cancel_on_drop.disarm();
            Err("timed out; recording publication cancelled".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Publication authorization is irrevocable. A second deadline would
            // recreate a false timeout ERR after the worker already won the CAS.
            match rx.recv() {
                Ok(reply) => {
                    cancel_on_drop.disarm();
                    Ok(reply)
                }
                Err(_) => {
                    cancel_on_drop.disarm();
                    Err("recording worker disconnected after commit authorization".to_string())
                }
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("recording worker disconnected".to_string())
        }
    }
}

/// `video <seconds> [full] [keys] [pace] [fps=<n>] [budget=<MiB>]` — record the
/// front window's exact swapchain bytes handed to the WSI present path,
/// including the swapchain-only glow/chrome layers every single-frame tool
/// misses. Compositor visibility and scanout are not observable here. The verb
/// dumps a PNG sequence + index.json and blocks until the durable `.published`
/// visibility marker is on disk (a multi-second recording can take a while, as
/// its help documents).
/// `keys` (the same-clock keystroke log) is OWNER-only: recording someone's
/// keystrokes is not a screen-read.
pub(crate) fn cmd_video(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    owner: bool,
) -> crate::control::ControlReply {
    // Observability + cancel for the blocking one-shot: `video status` reads the
    // in-flight recording (or reports none), `video stop` finalizes it now — the
    // dump still answers the ORIGINAL client. Both are cheap main-thread reads.
    // `stop` TRUNCATES a recording someone else started, so it is OWNER-only —
    // mirroring the `keys` gate below; a scoped edge must not be able to cut short
    // (or, via the mode readout, probe) an owner's in-flight capture. `status` is
    // a benign read and stays any-scope.
    match rest.trim() {
        "status" => {
            return match call_main(proxy, |tx| Wake::VideoStatus { reply: tx }) {
                Ok(line) => line,
                Err(e) => format!("ERR video: {e}\n"),
            }
            .into();
        }
        "stop" => {
            if !owner {
                return "ERR video: stop is owner-only\n".into();
            }
            return match call_main(proxy, |tx| Wake::VideoStop { reply: tx }) {
                Ok(line) => line,
                Err(e) => format!("ERR video: {e}\n"),
            }
            .into();
        }
        _ => {}
    }
    // `video frames [count=N]`: NO capture — read the newest finished recording's
    // index.json and return its N highest-`delta` frames (the visually eventful key
    // moments). A pure main-thread filesystem read, so it never touches the event
    // loop. Detect the sub-verb by first token (not exact match) so `count=` rides.
    {
        let mut toks = rest.split_whitespace();
        if toks.next() == Some("frames") {
            return video_frames(sock_dir, toks);
        }
    }
    let args = match parse_video_args(rest) {
        Ok(a) => a,
        Err(e) => return e.into(),
    };
    if args.keys && !owner {
        return "ERR video: keys is owner-only\n".into();
    }
    let budgets = crate::control::ArtifactBudgets::process();
    let Some(mut handoff) = budgets.try_reserve_for_path(sock_dir) else {
        return format!("ERR video: {}\n", crate::control::ARTIFACT_HANDOFF_BUSY).into();
    };
    let dir = match control_auth::confine_video_dir_with_admission(sock_dir, budgets, |path| {
        handoff.try_reconcile_for_resolved_path(path)
    }) {
        Ok(dir) => dir,
        Err(control_auth::ArtifactConfinementError::AdmissionRefused) => {
            return format!("ERR video: {}\n", crate::control::ARTIFACT_HANDOFF_BUSY).into();
        }
        Err(control_auth::ArtifactConfinementError::Invalid) => {
            return "ERR video: could not create the recording dir\n".into();
        }
    };
    let cancel = crate::VideoCancellation::new();
    let mut cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel.clone()));
    // The main thread keeps `wire` and a seamless update's Commit waits on it:
    // this request's reply, whatever it turns out to be, is written before
    // that `_exit`.
    let (wire, written) = crate::control::ReplyWire::new();
    // Refused while the main thread is stalled or a dialog stands, as every
    // main-thread verb is. A refused or unsent request drops the
    // unforgeable directory guard, which removes the unpublished directory.
    let posted = post_main(proxy, |reply| Wake::Video {
        dur_ms: (args.secs * 1000.0) as u64,
        full_res: args.full_res,
        keys: args.keys,
        pace: args.pace,
        fps: args.fps,
        budget_bytes: args.budget_bytes,
        handoff,
        dir,
        cancel,
        reply,
        wire,
    });
    let (rx, _hop) = match posted {
        Ok(posted) => posted,
        Err(refusal) => return format!("ERR video: {refusal}\n").into(),
    };
    // The reply arrives only after index.json is on disk (recording duration +
    // the PNG encode burst) — wait generously (scaled with the recording so a
    // long take cannot falsely time out mid-encode), but never forever.
    match recv_video_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs_f64(args.secs + 120.0),
    ) {
        Ok(reply) => {
            let (body, retention) = reply.into_parts();
            crate::control::ControlReply::with_handoff(body, retention)
        }
        Err(error) => crate::control::ControlReply::from(format!("ERR video: {error}\n")),
    }
    .releasing_on_write(written)
}

/// `controls <target>` dumps a compatibility GUI target's semantic controls as text, the
/// analogue of `chrome`, so a driver can read native tabs and transient surfaces without
/// a screenshot.
///
/// Unlike the pixel `window` capture, this works HEADLESS: the main thread compiles the
/// native Settings model or calls a transient surface's concrete serializer
/// (`App::read_aux_controls`), never walking AppKit views, so no window need be on
/// screen. NEITHER verb needs a Screen Recording grant — `window` photographs aterm's OWN
/// window, which needs none (`cmd_window`, `capture_window_pixels`).
/// A closed Settings tab truthfully reports zero visible controls. Framed `OK <n>\n` +
/// `<n>` rows, the SAME multi-line shape as `chrome`/`text`.
pub(crate) fn cmd_controls(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    use crate::app_introspect::AuxTarget;
    let trimmed = rest.trim();
    // Native Settings aliases AND the front window's transient overlay have a controls
    // surface.
    // `front` (and a bare/empty arg, which `parse` maps to Front) reports the front
    // window's open overlay slot (open/closed + kind + fp + scroll extent) — headless-safe
    // — and, off macOS, a second line: whether a close/quit confirm is pending, who
    // asked, and how the wire answers it (`confirm open=true … by=ctl …
    // answer="confirm yes" cancel="confirm no"`, or `answer="-"` for a person's
    // question; see `close_confirm`; `confirm open=false` otherwise).
    // The targets are the one table's (`control_verbs::aux_targets!`), which
    // the entry and this refusal quote (`WINDOW_TARGETS`).
    let Some(target) = AuxTarget::parse(trimmed) else {
        return format!(
            "ERR unknown target (use: {})\n",
            aterm_types::control_verbs::WINDOW_TARGETS
        );
    };
    let lines = match call_main(proxy, |tx| Wake::ReadAuxControls { target, reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    // Same `OK <n>\n` + n-rows framing `chrome`/`text` use, so the aterm-ctl client
    // prints the rows verbatim (it lists `controls` among the multi-line verbs).
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Read the versioned semantic surface of native tab applications. Parsing is
/// completed on the control thread, but stable-view resolution and compilation
/// happen atomically on the main thread against the canonical live state.
pub(crate) fn cmd_inspect(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let request = match crate::app_control::parse_inspect(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    let lines = match call_main(proxy, |reply| Wake::InspectApp { request, reply }) {
        Ok(Ok(lines)) => lines,
        Ok(Err(error)) => return format!("ERR {error}\n"),
        Err(error) => return format!("ERR {error}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Dispatch one exact semantic action to a stable native view. The main-thread
/// handler revalidates view identity, UI key, action, enabled state, and value
/// type in one event-loop turn before entering the reducer/effect host.
pub(crate) fn cmd_act(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let request = match crate::app_control::parse_act(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    match call_main(proxy, |reply| Wake::ActApp { request, reply }) {
        Ok(Ok(message)) => format!("OK {message}\n"),
        Ok(Err(error)) => format!("ERR {error}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `open <target> [close]`'s target: the aux surface and whether to close it,
/// or the refusal line. `open <target> close` is the symmetric half: native
/// Settings targets close through the tab lifecycle and the transient palette
/// uses its overlay exit. A target is one the table marks as opened
/// ([`AuxTarget::opens`](crate::app_introspect::AuxTarget::opens)), whose
/// list the help entry and both refusals quote (`control_verbs::OPEN_TARGETS`).
/// `front` is always open (and a bare/empty arg maps to it), so it is refused
/// with the verb's contract.
fn open_target(trimmed: &str) -> Result<(crate::app_introspect::AuxTarget, bool), String> {
    use crate::app_introspect::AuxTarget;
    use aterm_types::control_verbs::OPEN_TARGETS;
    let (target_tok, close) = match trimmed.split_once(char::is_whitespace) {
        Some((t, r)) if r.trim() == "close" => (t, true),
        Some(_) => {
            return Err(format!(
                "ERR usage: open <{OPEN_TARGETS}> [close] | open app <…>\n"
            ));
        }
        None => (trimmed, false),
    };
    AuxTarget::parse(target_tok)
        .filter(|t| t.opens())
        .map(|t| (t, close))
        .ok_or_else(|| format!("ERR unknown target (use: {OPEN_TARGETS})\n"))
}

/// `open <target> [close]` -> open or close a compatibility GUI target. `prefs`,
/// `about`, and `update` resolve to routes in the native Settings tab; `menu` is a
/// transient palette.
/// The versioned, explicit forms are `open app settings [/route]` and
/// `inspect app/v1 ...`. Reuses the SAME open paths as human menu items.
///
/// MAIN-THREAD HOP: touching `App` state may only
/// happen on the main thread, but this runs on a background control thread — so we post
/// [`Wake::OpenAuxWindow`] + a one-shot reply and BLOCK; the main thread opens the
/// surface and replies `Ok(())` (now open) or `Err(msg)`. Single-line
/// `OK opened <target>` / `ERR`.
pub(crate) fn cmd_open(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let trimmed = rest.trim();
    if trimmed.split_whitespace().next() == Some("app") {
        let request = match crate::app_control::parse_open(trimmed) {
            Ok(request) => request,
            Err(error) => return format!("ERR {error}\n"),
        };
        return match call_main(proxy, |reply| Wake::OpenApp { request, reply }) {
            Ok(Ok(message)) => format!("OK {message}\n"),
            Ok(Err(error)) => format!("ERR {error}\n"),
            Err(error) => format!("ERR {error}\n"),
        };
    }
    let (target, close) = match open_target(trimmed) {
        Ok(taken) => taken,
        Err(refusal) => return refusal,
    };
    match call_main(proxy, |tx| Wake::OpenAuxWindow {
        target,
        close,
        reply: tx,
    }) {
        Ok(Ok(())) => format!(
            "OK {} {}\n",
            if close { "closed" } else { "opened" },
            target.keyword()
        ),
        Ok(Err(msg)) => format!("ERR {msg}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `settings [open|close|toggle]` -> control the process-singleton native Settings tab
/// (`open settings` opens the same app; this verb adds close/toggle and replies with
/// the resulting state). Bare arg / `toggle` toggles; `open`/`close` force the state.
/// `inspect app/v1 tabs` discovers its stable view and `image` captures it headlessly.
/// Posts to the main
/// thread (the sole `App` mutator) and BLOCKS on a one-shot reply carrying the resulting
/// open state. Single-line `OK settings open` / `OK settings closed` / `ERR …`.
fn normalize_settings_section_name(input: &str) -> String {
    input
        .trim()
        .trim_start_matches('/')
        .to_ascii_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the native label/path a person sees in Settings, plus compatibility aliases.
fn parse_settings_section_route(input: &str) -> Option<crate::native_settings::SettingsRoute> {
    use crate::native_settings::SettingsRoute;

    let normalized = normalize_settings_section_name(input);
    if let Some(route) = SettingsRoute::ALL.into_iter().find(|route| {
        normalize_settings_section_name(route.label()) == normalized
            || normalize_settings_section_name(route.path()) == normalized
    }) {
        return Some(route);
    }
    // Settings ▸ Harness answers to each agent's names that has a harness
    // table, its word and its product's (`codex`, `Claude Code`): the page
    // holds its switch.
    if aterm_phase::Program::ALL.into_iter().any(|p| {
        p.harness_table()
            && (normalized == p.name()
                || normalized == normalize_settings_section_name(p.product()))
    }) {
        return Some(SettingsRoute::Harness);
    }
    match normalized.as_str() {
        "prefs" | "preferences" | "settings" | "top" | "general" => Some(SettingsRoute::Home),
        "modified settings" => Some(SettingsRoute::Modified),
        "manual config" | "config" => Some(SettingsRoute::Manual),
        "text" | "typography" | "text and fonts" => Some(SettingsRoute::TextFonts),
        "cursor" | "cursor and motion" => Some(SettingsRoute::CursorMotion),
        // The cursor styles' page. "kitty" alone routes here and NOT to the
        // Kitty Log (a read-only book with no route): a bare "kitty" from
        // `settings section …` means the thing walking your cursor.
        "kitty" | "cat" | "cursor cat" | "cursor pet" | "kitty pet" | "cursor kitty"
        | "cursor style" | "styles" | "trail" | "trails" | "cursor trail" => {
            Some(SettingsRoute::CursorStyles)
        }
        "sounds" | "audio" | "music" | "music effects" | "volume" => Some(SettingsRoute::Sound),
        "windows" | "window and tabs" | "window & tabs" => Some(SettingsRoute::WindowTabs),
        "input" | "keyboard" | "keyboard and input" => Some(SettingsRoute::KeyboardInput),
        "update" | "software updates" => Some(SettingsRoute::SoftwareUpdate),
        // The owner's word is "log"; the log's page is Settings ▸ Messages.
        "log" | "logs" | "history" | "history log" | "activity log" | "message log"
        | "messages log" => Some(SettingsRoute::Messages),
        // ...and to the harness's own name, the control's and the host's:
        // what a person reaches for to turn the harness down (the Autopilot
        // design, §5).
        "coding agent harness" | "claude code harness" | "autopilot" | "supervisor" => {
            Some(SettingsRoute::Harness)
        }
        // Its AI CLIs sub-page answers to the AI words (an agent's own
        // names are the Harness page's, which holds its switch).
        "ai cli" | "ai reviews" | "ai logins" => Some(SettingsRoute::Ai),
        "tabs" | "tab control" | "control panel" | "fleet" | "monitor" | "fleet monitor"
        | "system monitor" => Some(SettingsRoute::Tabs),
        // Settings ▸ Themes answers to its own names, never to "theme" (the
        // theme setting lives on Appearance).
        "showcase" | "theme showcase" | "themes gallery" | "gallery" => Some(SettingsRoute::Themes),
        _ => None,
    }
}

fn settings_section_usage() -> String {
    crate::native_settings::SettingsRoute::ALL
        .into_iter()
        .map(|route| route.label().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" | ")
}

pub(crate) fn cmd_settings_overlay(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `settings set <key> <value…>` / `settings unset <key>`: enqueue ONE field in
    // the same serialized/versioned config lane as native Settings (works with the
    // Settings tab closed and headless). Keys come from the shared Settings/Manual
    // schema; `controls prefs` intentionally reports only controls visible on the
    // current native route. A value may contain spaces. The main thread posts the job
    // and stays free; this control thread blocks on the carried reply until durable
    // completion/reconcile.
    let t = rest.trim();
    let (word, tail) = match t.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim_start()),
        None => (t, ""),
    };
    if word == "set" || word == "unset" {
        let (key, value) = if word == "set" {
            match tail.split_once(char::is_whitespace) {
                Some((k, v)) if !v.trim().is_empty() => (k, Some(v.trim().to_string())),
                _ => {
                    return "ERR usage: settings set <key> <value…> | settings unset <key>\n"
                        .to_string();
                }
            }
        } else {
            if tail.is_empty() || tail.split_whitespace().count() != 1 {
                return "ERR usage: settings unset <key>\n".to_string();
            }
            (tail, None)
        };
        let key = key.to_string();
        return match call_main(proxy, |tx| Wake::SetSettingsField {
            key,
            value,
            reply: tx,
        }) {
            Ok(completion) => settings_field_wire_reply(completion),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    // `settings section <name>`: land the OPEN surface on the native page whose label or
    // stable path the caller supplied. Compatibility aliases remain accepted, but a
    // deleted legacy page can never creep back into the advertised choices.
    if word == "section" {
        let Some(route) = parse_settings_section_route(tail) else {
            return format!("ERR unknown section (use: {})\n", settings_section_usage());
        };
        return match call_main(proxy, |tx| Wake::SettingsShowSection { route, reply: tx }) {
            Ok(Ok(())) => format!(
                "OK settings section {}\n",
                route.path().trim_start_matches('/')
            ),
            Ok(Err(e)) => format!("ERR {e}\n"),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    let open = match rest.trim() {
        "" | "toggle" => None,
        "open" | "on" | "show" => Some(true),
        "close" | "off" | "hide" => Some(false),
        other => {
            return format!(
                "ERR unsupported {other:?} (use: open|on|show | close|off|hide | toggle | \
                 section <name> | set <key> <value…> | unset <key>)\n"
            );
        }
    };
    match call_main(proxy, |tx| Wake::SettingsOverlay { open, reply: tx }) {
        Ok(Some(true)) => "OK settings open\n".to_string(),
        Ok(Some(false)) => "OK settings closed\n".to_string(),
        Ok(None) => "ERR no front window\n".to_string(),
        Err(e) => format!("ERR {e}\n"),
    }
}

fn settings_field_wire_reply(completion: Result<String, String>) -> String {
    match completion {
        Ok(status) => format!("OK {status}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// What `invoke <name>` answers before any main-thread hop, or `None` to go
/// on: a RETIRED name is `ERR retired: <what to use instead>`
/// (`menu::retired_invoke_name`), and THE PERSON'S OWN ACTS (Take Over / Hand Back, and their aliases
/// `HoldSession` / `LiftHold`) are `ERR denied` to every scope, the Owner
/// token included — every in-session client holds Owner, so the held agent
/// would hand its own tab back (`InvokeAuthority::PersonOnly`).
pub(crate) fn invoke_refusal(name: &str) -> Option<String> {
    // A RETIRED name answers with what to use instead, before any hop.
    if let Some(instead) = crate::menu::retired_invoke_name(name) {
        return Some(format!("ERR retired: {instead}\n"));
    }
    let action = crate::menu::MenuAction::from_invoke_name(name)?;
    (action.invoke_authority() == crate::menu::InvokeAuthority::PersonOnly
        && !person_invoke_act(name))
    .then(|| {
        let at = match action {
            crate::menu::MenuAction::ToggleStatsLens => "View \u{25b8} Stats Lens, \u{2318}I",
            _ => "Agents \u{25b8} Take Over This Tab / Hand Back This Tab",
        };
        format!("ERR denied: {name} is the person's own act at the window ({at})\n")
    })
}

/// Whether `invoke <name>` is the PERSON'S OWN ACT pressed by a development
/// build's stand-in for the person at the window: with
/// `ATERM_DEBUG_PERSON_INVOKE=1` in the instance's environment at launch
/// (`aterm_types::dev_seam!`, read once), `invoke TakeOver|HandBack` presses
/// them on the front tab — under any hold, as the person's own gesture is —
/// so a headless test stands where the person does (the step-in's live test:
/// Take Over refusing another Mac's writes). A shipped build never reads it,
/// and an in-session client cannot set it on the instance it runs under.
pub(crate) fn person_invoke_act(name: &str) -> bool {
    person_seam_armed()
        && crate::menu::MenuAction::from_invoke_name(name.trim()).is_some_and(|action| {
            action.invoke_authority() == crate::menu::InvokeAuthority::PersonOnly
        })
}

/// Whether this instance was launched with a development build's stand-in
/// for the person at the window: `ATERM_DEBUG_PERSON_INVOKE=1` in its
/// environment at launch (`aterm_types::dev_seam!`, read once). It arms
/// `invoke TakeOver|HandBack` ([`person_invoke_act`]) and the `person` verb
/// ([`cmd_person`]); a shipped build never reads it.
fn person_seam_armed() -> bool {
    static SEAM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEAM.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEBUG_PERSON_INVOKE").is_some_and(|value| value == "1")
    })
}

/// Whether this build carries the `person` verb's stand-in at all
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2): a debug or test
/// build only. A release build answers [`PERSON_NEEDS_DEBUG`].
const PERSON_BUILD: bool = cfg!(any(test, debug_assertions));

/// What a build without the stand-in answers `person` with.
pub(crate) const PERSON_NEEDS_DEBUG: &str = "person needs a debug build";

/// What an unarmed instance answers `person` with: the verb is the person's
/// own, stood in for only where the instance was launched to be.
pub(crate) const PERSON_DENIED: &str = "denied: person is a development build's stand-in for \
     the person at the window (ATERM_DEBUG_PERSON_INVOKE=1 at launch)";

/// The usage line every malformed `person` form answers with.
const PERSON_USAGE: &str = "usage: person look <window>|off | person menubar press <at>";

/// One `person` request (§12.5.2), parsed on the control thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersonAsked {
    /// `person look <window>|off`: someone can look at that headless window
    /// now (its number as `ls` prints `window=`), or at none.
    Look(Option<u64>),
    /// `person menubar press <at>`: the menu-bar item's row at that path,
    /// clicked ([`crate::status_item::MenuAt`]).
    Press(crate::status_item::MenuAt),
}

/// `person`'s words read, where this build carries the stand-in (`built`)
/// and this instance was launched with it (`armed`), in that order: a
/// release build's refusal, then the person's own (`ERR denied`), then the
/// grammar. `armed` is asked only where `built` holds, so a release build
/// (with `dev-seams` or not) never reads the stand-in's arming. Pure but for
/// what `armed` reads.
pub(crate) fn parse_person(
    rest: &str,
    built: bool,
    armed: impl FnOnce() -> bool,
) -> Result<PersonAsked, String> {
    if !built {
        return Err(PERSON_NEEDS_DEBUG.to_string());
    }
    if !armed() {
        return Err(PERSON_DENIED.to_string());
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words.as_slice() {
        ["look", "off"] => Ok(PersonAsked::Look(None)),
        ["look", window] => window
            .parse::<u64>()
            .ok()
            .filter(|_| window.bytes().all(|b| b.is_ascii_digit()))
            .map(|w| PersonAsked::Look(Some(w)))
            .ok_or_else(|| format!("{PERSON_USAGE} (`{window}` is no window number)")),
        ["menubar", "press", at] => crate::status_item::MenuAt::parse(at)
            .map(PersonAsked::Press)
            .ok_or_else(|| format!("{PERSON_USAGE} (`{at}` is no row path: `chrome` prints them)")),
        _ => Err(PERSON_USAGE.to_string()),
    }
}

/// `person look <window>|off` | `person menubar press <at>`: A DEVELOPMENT
/// BUILD'S STAND-IN FOR THE PERSON AT THE WINDOW
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2), armed like
/// `invoke TakeOver|HandBack`'s ([`person_invoke_act`]), so a headless lab
/// can stand where the person does: a look at a headless window, which the
/// one looking rule reads (`App::person_look`), and a click on a menu-bar
/// row by its path (`App::person_menubar_press`). A main-thread hop like
/// `invoke`; answers `OK <words>` or `ERR <why>`.
pub(crate) fn cmd_person(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let asked = match parse_person(rest, PERSON_BUILD, person_seam_armed) {
        Ok(asked) => asked,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match call_main(proxy, |reply| Wake::PersonStandIn { asked, reply }) {
        Ok(Ok(words)) => format!("OK {words}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `invoke <action>` -> fire a menu action BY NAME — the `action=` tokens `controls
/// menu` prints (the `MenuAction` Debug names, e.g. `NewTab`) — through the SAME
/// single dispatch sink the native menu bar and the ⌘K palette use. The live
/// palette row's `enabled` (the `validateMenuItem:` conditions) gates it: a
/// disabled action is a named ERR, never a silent no-op. Main-thread hop like
/// `open`/`controls` (one-shot reply channel); works headless. `by` is the
/// caller ([`super::control_session::caller_actor`]), the exit ledger's `by=`
/// for a session the action retires ([`crate::App::invoke_attributed`]);
/// `typist` is who an action that types (`Paste`) is credited to where its
/// text lands (S9, `App::invoke_typist`).
pub(crate) fn cmd_invoke(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    by: crate::session_store::ExitActor,
    typist: Option<std::sync::Arc<crate::typed_by::Caller>>,
) -> String {
    let name = rest.trim();
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return "ERR usage: invoke <action>   (list actions with `controls menu`)\n".to_string();
    }
    if let Some(refusal) = invoke_refusal(name) {
        return refusal;
    }
    let name = name.to_string();
    match call_main(proxy, |tx| Wake::InvokeMenuAction {
        name,
        by,
        typist,
        reply: tx,
    }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `confirm yes|no` -> answer the close/quit question a control client raised in
/// the window (`close_confirm`, Windows): `OK answered=<yes|no> kind=<quit|close-window>`,
/// or `ERR` naming what stands in the way. The grammar is checked here, before the
/// main-thread hop, so a guess never reaches the question. Main-thread hop like
/// `invoke`; the event loop is free while a question stands, which is the point.
pub(crate) fn cmd_confirm(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let proceed = match crate::close_confirm::parse_wire_answer(rest) {
        Ok(proceed) => proceed,
        Err(usage) => return format!("ERR {usage}\n"),
    };
    match call_main(proxy, |tx| Wake::AnswerCloseConfirm { proceed, reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `rain [status|on|off|toggle]` -> the per-session matrix-rain override on the
/// focused window's front session (the same per-session state View ▸ Matrix
/// Rain and the `toggle_matrix_rain` keybinding flip). `status` is the
/// observability face scripts/tests read:
/// `OK config_enabled=<bool> session_override=<none|on|off> effective=<bool>
/// engine=<none|live> active=<bool> scope=<window|focused-pane>
/// focused=<bool> animating=<bool>` — the engine tail reports the actual
/// render state: `scope` says whether emission covers the whole window
/// (single-pane / zoomed) or the focused pane of a split, `active` is the
/// wake-arming `is_active()`, and `focused`/`animating` surface the motion
/// facts (an unfocused or Reduced window emits nothing regardless of the
/// enable bits).
/// Main-thread hop like `open`/`controls` (one-shot reply channel); works
/// headless. Runtime-only — nothing durable is written (the Settings switch /
/// `settings set` own the `[matrix_rain]` config bit).
pub(crate) fn cmd_rain(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let op = match rest.trim() {
        "" | "status" => crate::RainCtlOp::Status,
        "on" => crate::RainCtlOp::On,
        "off" => crate::RainCtlOp::Off,
        "toggle" => crate::RainCtlOp::Toggle,
        _ => return "ERR usage: rain [status|on|off|toggle]\n".to_string(),
    };
    match call_main(proxy, |tx| Wake::RainControl { op, reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// The usage line every malformed `theme` form answers with.
pub(crate) const THEME_USAGE: &str = "usage: theme [status] | theme list | theme preview <name> \
                                      [ttl=<secs>] | theme revert | theme commit | theme set \
                                      <value> | theme palette [session] | theme session [status] \
                                      | theme session set <name> | theme session clear";

/// One parsed `theme` request: a main-thread op, or `set`, which goes through
/// the `settings set theme` lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeForm {
    Op(crate::control_theme::ThemeCtlOp),
    Set(String),
}

/// Parse the `theme` verb's tail. Pure, so the grammar is provable without an
/// event loop; `Err` carries the whole usage line. A preview name may hold
/// spaces (`theme preview GitHub Light`) or be quoted; `ttl=` may sit anywhere
/// after `preview`, at most once.
pub(crate) fn parse_theme_form(rest: &str) -> Result<ThemeForm, String> {
    use crate::control_theme::{THEME_PREVIEW_DEFAULT_TTL, THEME_PREVIEW_MAX_TTL, ThemeCtlOp};
    let t = rest.trim();
    let (word, tail) = match t.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (t, ""),
    };
    let bare = |op: ThemeCtlOp| {
        if tail.is_empty() {
            Ok(ThemeForm::Op(op))
        } else {
            Err(format!("{THEME_USAGE} (got {t:?})"))
        }
    };
    match word {
        "" | "status" => bare(ThemeCtlOp::Status),
        "list" => bare(ThemeCtlOp::List),
        "revert" => bare(ThemeCtlOp::Revert),
        "commit" => bare(ThemeCtlOp::Commit),
        "set" if !tail.is_empty() => Ok(ThemeForm::Set(tail.to_string())),
        "palette" => match tail {
            "" => Ok(ThemeForm::Op(ThemeCtlOp::Palette { session: false })),
            "session" => Ok(ThemeForm::Op(ThemeCtlOp::Palette { session: true })),
            _ => Err(format!("{THEME_USAGE} (got {t:?})")),
        },
        "session" => {
            let (sub, name) = match tail.split_once(char::is_whitespace) {
                Some((w, r)) => (w, r.trim()),
                None => (tail, ""),
            };
            match (sub, name.is_empty()) {
                ("" | "status", true) => Ok(ThemeForm::Op(ThemeCtlOp::SessionStatus)),
                ("clear", true) => Ok(ThemeForm::Op(ThemeCtlOp::SessionClear)),
                ("set", false) => {
                    // Tokens rejoined as a preview's are: a quoted name keeps its
                    // spaces, and an unquoted one may hold them.
                    let selector = split_quoted_tokens(name)
                        .ok_or_else(|| format!("{THEME_USAGE} (unterminated quote)"))?
                        .join(" ");
                    if selector.is_empty() {
                        return Err(format!("{THEME_USAGE} (session set names no theme)"));
                    }
                    Ok(ThemeForm::Op(ThemeCtlOp::SessionSet { selector }))
                }
                ("set", true) => Err(format!("{THEME_USAGE} (session set names no theme)")),
                _ => Err(format!("{THEME_USAGE} (got {t:?})")),
            }
        }
        "preview" => {
            let tokens = split_quoted_tokens(tail)
                .ok_or_else(|| format!("{THEME_USAGE} (unterminated quote)"))?;
            let mut ttl = None;
            let mut name = Vec::new();
            for token in tokens {
                match token.strip_prefix("ttl=") {
                    Some(secs) => {
                        let parsed = secs
                            .parse::<u64>()
                            .ok()
                            .filter(|s| (1..=THEME_PREVIEW_MAX_TTL.as_secs()).contains(s))
                            .ok_or_else(|| {
                                format!(
                                    "{THEME_USAGE} (ttl= wants 1..={} seconds, got {secs:?})",
                                    THEME_PREVIEW_MAX_TTL.as_secs()
                                )
                            })?;
                        if ttl.replace(parsed).is_some() {
                            return Err(format!("{THEME_USAGE} (ttl= given twice)"));
                        }
                    }
                    None => name.push(token),
                }
            }
            let name = name.join(" ");
            if name.is_empty() {
                return Err(format!("{THEME_USAGE} (preview names no theme)"));
            }
            Ok(ThemeForm::Op(ThemeCtlOp::Preview {
                id: name,
                ttl: ttl.map_or(THEME_PREVIEW_DEFAULT_TTL, std::time::Duration::from_secs),
            }))
        }
        _ => Err(format!("{THEME_USAGE} (got {t:?})")),
    }
}

/// `theme …` -> the live theme ([`crate::App::theme_control`]): the bare form
/// and `status` answer one status line, `list` answers `OK <n>` + rows
/// (`framing_of` flips it to Lines), `preview`/`revert` drive the transient
/// lane and answer the status line after, `commit` writes the preview's name
/// and `set` writes `<value>`, both through the `settings set theme` lane, so
/// their reply waits for the durable write and names any miss. Main-thread hop
/// like `rain`; works headless.
///
/// `session` is the request's target on the session path (its own session or
/// `@<sid>`'s), which `theme session …` and `theme palette session` act on;
/// `None` on the app path, where they take the front terminal.
pub(crate) fn cmd_theme(proxy: &EventLoopProxy<Wake>, rest: &str, session: Option<u64>) -> String {
    let op = match parse_theme_form(rest) {
        Ok(ThemeForm::Op(op)) => op,
        Ok(ThemeForm::Set(value)) => {
            return match call_main(proxy, |tx| Wake::SetSettingsField {
                key: crate::prefs::EDIT_THEME.to_string(),
                value: Some(value),
                reply: tx,
            }) {
                Ok(completion) => settings_field_wire_reply(completion),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Err(usage) => return format!("ERR {usage}\n"),
    };
    let list = matches!(
        op,
        crate::control_theme::ThemeCtlOp::List | crate::control_theme::ThemeCtlOp::Palette { .. }
    );
    match call_main(proxy, |tx| Wake::ThemeControl {
        op,
        session,
        reply: tx,
    }) {
        Ok(Ok(body)) if list => format!("OK {}\n{body}\n", body.lines().count()),
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Parse the `fx` verb's tail into its op: `""`/`status` reads; `celebrate`
/// takes `sig=<char>` (default `C`), `bars=<1..4>` (default 2) and
/// `on=green|enter|done` (default `green`) in any order, each at most once. A
/// pure parse so the vocabulary is provable without an event loop; `Err`
/// carries the whole usage line, so the refusal teaches the grammar.
pub(crate) fn parse_fx_form(rest: &str) -> Result<crate::FxCtlOp, String> {
    use aterm_effects::kitty_sing::{CELEBRATE_MAX_BARS, CelebrateOn, song_signature};
    const USAGE: &str =
        "usage: fx [status|celebrate [sig=<char>] [bars=<1..4>] [on=green|enter|done]]";
    let mut words = rest.split_whitespace();
    match words.next() {
        None | Some("status") => {
            return if words.next().is_none() {
                Ok(crate::FxCtlOp::Status)
            } else {
                Err(format!("{USAGE} (got {:?})", rest.trim()))
            };
        }
        Some("celebrate") => {}
        Some(_) => return Err(format!("{USAGE} (got {:?})", rest.trim())),
    }
    let (mut sig, mut bars, mut on) = (None, None, None);
    for w in words {
        let Some((k, v)) = w.split_once('=') else {
            return Err(format!("{USAGE} (got {w:?})"));
        };
        let dup = match k {
            "sig" => {
                let mut chars = v.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => sig.replace(song_signature(c)).is_some(),
                    _ => {
                        return Err(format!(
                            "{USAGE} (sig= wants exactly one character, got {v:?})"
                        ));
                    }
                }
            }
            "bars" => match v.parse::<u8>() {
                Ok(b) if (1..=CELEBRATE_MAX_BARS).contains(&b) => bars.replace(b).is_some(),
                _ => {
                    return Err(format!(
                        "{USAGE} (bars= wants 1..={CELEBRATE_MAX_BARS}, got {v:?})"
                    ));
                }
            },
            "on" => match v {
                "green" => on.replace(CelebrateOn::Green).is_some(),
                "enter" => on.replace(CelebrateOn::Enter).is_some(),
                "done" => on.replace(CelebrateOn::Done).is_some(),
                _ => return Err(format!("{USAGE} (on= wants green|enter|done, got {v:?})")),
            },
            _ => return Err(format!("{USAGE} (got {w:?})")),
        };
        if dup {
            return Err(format!("{USAGE} ({k}= given twice)"));
        }
    }
    Ok(crate::FxCtlOp::Celebrate {
        sig: sig.unwrap_or_else(|| song_signature('C')),
        bars: bars.unwrap_or(2),
        on: on.unwrap_or(CelebrateOn::Green),
    })
}

/// `fx [status|celebrate …]` -> one status line for the focused window's
/// one-shot celebration ([`crate::App::fx_control`]): `status` reads,
/// `celebrate` ARMS. Main-thread hop like `rain`, since the detector is
/// per-window App state. Every form answers the same post-state line, so a
/// driver reads what it armed without a second round trip.
pub(crate) fn cmd_fx(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let op = match parse_fx_form(rest) {
        Ok(op) => op,
        Err(usage) => return format!("ERR {usage}\n"),
    };
    match call_main(proxy, |tx| Wake::FxControl { op, reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `streak [status]` -> one status line for the FRONT window's PRISM WAKE
/// output streak ([`crate::App::streak_status`]): every gate that decides
/// whether program output earns a comet, plus the live engine facts.
///
/// Read-only, and for the same reason `tone` is: the streak's knobs are
/// durable config (`settings set output_streak.…`), so there is no `on`/`off`
/// form here — this verb only ever OBSERVES. It exists because the effect is
/// deliberately subtle, which makes "off", "demoted", "suppressed" and
/// "resting" look identical on glass. Main-thread hop like `rain`/`tone`.
pub(crate) fn cmd_streak(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match rest.trim() {
        "" | "status" => {}
        _ => return "ERR usage: streak [status]\n".to_string(),
    }
    match call_main(proxy, |tx| Wake::StreakStatus { reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `tone [status]` -> one status line for the FRONT window's tone-of-typing
/// state ([`crate::App::tone_status`]). Read-only: there is no `on`/`off`
/// form because the knob is durable config (`settings set tone_melody …`) —
/// this verb only ever observes. Main-thread hop like `rain`, since the
/// tracker is per-window App state.
pub(crate) fn cmd_tone(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match rest.trim() {
        "" | "status" => {}
        _ => return "ERR usage: tone [status]\n".to_string(),
    }
    match call_main(proxy, |tx| Wake::ToneStatus { reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Which reading of the cursor-trail engine `trail …` was asked for.
///
/// Split out of [`cmd_trail`] as a pure parse so the vocabulary is provable
/// without an event loop: `status` must not be swallowed as a malformed count,
/// and a count must not be swallowed as an unknown keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailForm {
    /// `trail [<n>]` — the per-verdict admission ring.
    Admissions(Option<usize>),
    /// `trail status` — the standing engine-state line.
    Status,
}

/// Parse the `trail` verb's tail. `Err` carries the whole usage line, so the
/// caller never rebuilds it (and the two forms cannot document themselves
/// differently).
pub(crate) fn parse_trail_form(rest: &str) -> Result<TrailForm, String> {
    match rest.trim() {
        "" => Ok(TrailForm::Admissions(None)),
        "status" => Ok(TrailForm::Status),
        n => n
            .parse::<usize>()
            .map(|n| TrailForm::Admissions(Some(n)))
            .map_err(|_| "usage: trail [status|<n>]".to_string()),
    }
}

/// `trail [status|<n>]` -> the focused window's cursor-trail diagnostics.
///
/// * `trail [<n>]` — the last `n` spawn-seam verdicts (default: the whole
///   diagnostic ring, cap 32), one
///   `admission seq= phase= reason= age_ms= origin= target= alt= licence=`
///   row each, newest last ([`crate::App::trail_admissions`]). `phase` is
///   `licensed` or `declined`; `reason` on a decline is `no-fresh-hint` (no
///   key hint was fresh, so the move was program output nobody's fingers
///   asked for), `no-credits` (a multi-cell coalesce outran the press credit
///   budget), `off-shape` (licensed and classified, but the style's shape
///   gates laid nothing) or `program-row`; `licence=` names the
///   class a licensed row was admitted under — `key` (a press hint),
///   `inflight` (no hint was fresh and the unpaid presses still waiting on
///   the row licensed the batch — a stalled prompt catching up),
///   `insert` (a delivered insert, Rainbow Kitty only: a file drop, ⌘V, and
///   — into the tab on screen — the `paste` verb, `paste-bin`, `turn`'s
///   paste phase, an unguarded `key tab` or `key ctrl+v`; its bytes provably
///   on the wire, laid as one sweep) or `rewrite` (the program pulled the
///   caret back inside that insert's span and the ribbon retracted to it);
///   `none` on a decline. `send`, `feed`, a guarded `key if=` and any input
///   routed to a background session stamp nothing and stay dark by
///   contract.
/// * `trail status` — one `trail style= … ribbon_active= …` line of standing
///   engine state ([`crate::App::trail_status`]): the resolved style, every
///   gate from the `cursor_trail` knob to the glass, the cumulative
///   `licensed=`/`declined=`/`last_decline_reason=` scoreboard, the light
///   alive right now, who owns the block cursor and what colour their body is
///   built from (`block_fill=`/`block_fill_base=`/`block_fill_base_from=`),
///   delivered inserts (`inserts_delivered=`/`inserts_lit=`/
///   `inserts_retracted=`/`last_insert_cells=`, which advance under Rainbow
///   Kitty only), and presses in flight (`inflight_licensed=`/
///   `inflight_forgotten=`/`credits=`/`swallowed_no_echo=`/`park_returns=`/
///   `park_flushed=`).
///
/// A cursor-body effect replaces the caret's colour outright
/// (`RenderInput::cursor_fill_override` is applied instead of the frame cursor
/// colour), independent of `glow_active`/`pet_active`/`cat_active`.
/// `block_fill=none` means the terminal's own cursor colour is on the glass;
/// anything else names the owner, and `block_fill_base_from=style_identity`
/// marks owners (fire, water) whose colour is theirs by design.
///
/// Read-only, main-thread hop like `tone` (the state lives in per-window App
/// state); works headless.
pub(crate) fn cmd_trail(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let count = match parse_trail_form(rest) {
        Ok(TrailForm::Admissions(count)) => count,
        Ok(TrailForm::Status) => {
            // `trail` is declared `Lines` in the verb catalog, so the status
            // form answers `OK 1` + its single row rather than a bare `OK <line>`.
            return match call_main(proxy, |tx| Wake::TrailStatus { reply: tx }) {
                Ok(Ok(msg)) => format!("OK 1\n{msg}\n"),
                Ok(Err(e)) => format!("ERR {e}\n"),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Err(usage) => return format!("ERR {usage}\n"),
    };
    let lines = match call_main(proxy, |tx| Wake::TrailAdmissions { count, reply: tx }) {
        Ok(Ok(lines)) => lines,
        Ok(Err(e)) => return format!("ERR {e}\n"),
        Err(e) => return format!("ERR {e}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// `kitty` -> the cursor cat's wearable collection; `kitty wear <key>` -> put
/// one on.
///
/// The ledger elects by greatest pin stamp, so a wear is monotone — it needs no
/// unpin, and it survives a merge from any other replica of the collection.
/// Main-thread hop like `trail`/`tone`: the collection lives in `App`.
pub(crate) fn cmd_kitty(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let rest = rest.trim();
    if let Some(key) = rest.strip_prefix("wear") {
        let key = key.trim();
        if key.is_empty() {
            return "ERR usage: kitty wear <key> (`kitty` alone lists them)\n".to_string();
        }
        return match call_main(proxy, |tx| Wake::KittyWear {
            key: key.to_string(),
            reply: tx,
        }) {
            // ONE FRAMING PER VERB: `kitty` is `Lines` in the catalog, so the
            // wear form answers `OK 1` + its row, never a bare `OK <line>` a
            // client would read as a count.
            Ok(Ok(row)) => format!("OK 1\n{row}\n"),
            Ok(Err(e)) => format!("ERR {e}\n"),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    if !rest.is_empty() {
        return "ERR usage: kitty [wear <key>]\n".to_string();
    }
    let rows = match call_main(proxy, |tx| Wake::KittyCollection { reply: tx }) {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => return format!("ERR {e}\n"),
        Err(e) => return format!("ERR {e}\n"),
    };
    let mut out = format!("OK {}\n", rows.len());
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// `status` -> the target session's SUBJECT + classified STATUS record, one
/// versioned key=value line (RFC: Tab Subject & Status §8).
///
/// Read-only, with no sub-form: everything that could change a status is either
/// the user's own metadata (`meta set`) or durable config (`settings set
/// tab_status …`), so this verb only ever observes. A main-thread hop like
/// `tone`/`panes` because the classifier lives in `App` state, which the control
/// thread cannot reach; that also makes it work headless, which is what lets an
/// end-to-end test assert on a classification at all.
pub(crate) fn cmd_session_status(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    if !rest.trim().is_empty() {
        return "ERR usage: status\n".to_string();
    }
    match call_main(proxy, |tx| Wake::ReadSessionStatus { session, reply: tx }) {
        Ok(Ok(record)) => format!("OK {record}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Split `rest` into `key=value` tokens, honouring a double-quoted value.
///
/// The control protocol is newline-delimited and otherwise whitespace-split; a
/// value may be wrapped in `"…"` for paths containing spaces (e.g.
/// `cwd="C:\Program Files\Git"`), and only the wrapping quotes are consumed.
/// There is deliberately no escape vocabulary (`\"`, doubling, percent-encoding):
/// the front door refuses to forward a path containing `"`
/// (`aterm_cli::WindowRequest::control_request`).
///
/// A `"` opens a quote only at the start of a token or immediately after the `=`
/// of a `key=` prefix; anywhere else (such as `/tmp/a"b`) it is an ordinary
/// character, so unquoted tokens split identically to `split_whitespace`.
///
/// Returns `None` for an unterminated quote — a malformed request, answered with
/// the verb's usage line rather than a half-path.
///
/// `pub(crate)` because the socket-policy fence in `control.rs` must classify
/// with the same tokenizer that later parses the command.
pub(crate) fn split_quoted_tokens(rest: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_quotes = false;
    for ch in rest.chars() {
        if ch == '"' && in_quotes {
            in_quotes = false;
        } else if ch == '"' && (current.is_empty() || current.ends_with('=')) {
            in_quotes = true;
            started = true;
        } else if ch.is_whitespace() && !in_quotes {
            if started {
                out.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(ch);
            started = true;
        }
    }
    if in_quotes {
        return None;
    }
    if started {
        out.push(current);
    }
    Some(out)
}

/// The `spawn` verb's usage line — one string for every malformed form, naming
/// the whole grammar (plain + aimed + connected + worker) so a caller who got
/// one token wrong sees the complete contract.
pub(crate) const SPAWN_USAGE: &str = "ERR usage: spawn [window=<id>] [raise=<t|f>] [cwd=<path>] [split=<v|h>] [theme=<name>] [connected=controlled|controller place=window|tab of=<sid> | agent=<claude|gemini|agy|muse> [model=<m>] [autopilot=<land|cruise>] [from=<sid>] len=<n>]\n";

/// `agent=<word>` (a WORKER, `task_launch`): which agent the newborn's shell
/// starts at its first prompt, its task the frame the request carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentSpec {
    /// `agent=<claude|gemini|agy|muse>`: `agent` a harness with a worker row
    /// (`aterm_agent::harness::worker::WORKERS`), with `model=` its model
    /// flag's model when given, `autopilot=` the newborn's own Autopilot word
    /// (`land` or `cruise`; `None`: land,
    /// [`crate::task_launch::WORKER_AUTOPILOT`]), and `from=` the session that
    /// starts it (the client's own): the task's `from=`, and the agent of the
    /// same kind whose program, homes and login the worker's line carries.
    /// Without it the task is `from=ctl`.
    Worker {
        agent: aterm_phase::Program,
        model: Option<String>,
        autopilot: Option<aterm_agent::supervise::Autopilot>,
        from: Option<aterm_session::SessionId>,
    },
    /// `agent=codex`: an agent aterm reads ([`aterm_phase::Program`]) that
    /// has no worker row, reserved: answered `ERR agent <word>: not yet`
    /// (its screen harness was retired, 2026-10-16). Never a shell, and never
    /// another agent under its name.
    Reserved(aterm_phase::Program),
}

/// The agent `agent=<word>` names, by [`aterm_phase::Program::name`]: an
/// agent with a worker row (`aterm_agent::harness::worker::worker_cli`)
/// starts a worker, every other agent's is reserved, and a word no agent
/// goes by (the generic reader's included) is no agent.
fn agent_spec(word: &str) -> Option<AgentSpec> {
    use aterm_phase::Program;
    let program = Program::ALL.into_iter().find(|p| p.name() == word)?;
    match program {
        Program::Generic => None,
        _ if aterm_agent::harness::worker::worker_cli(program).is_some() => {
            Some(AgentSpec::Worker {
                agent: program,
                model: None,
                autopilot: None,
                from: None,
            })
        }
        Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Gemini => {
            Some(AgentSpec::Reserved(program))
        }
    }
}

/// Whether `model=<m>` names a model a worker's line may carry:
/// `[A-Za-z0-9._-]{1,64}`, a word no shell reads anything into, opening
/// with no `-` (Gemini CLI's yargs and Muse Code's clap read `--model -x`
/// as a flag and exit).
fn valid_model(model: &str) -> bool {
    (1..=64).contains(&model.len())
        && !model.starts_with('-')
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The `tab` verb's usage line, shared by the front-window form
/// ([`super::control_input::cmd_tab`]) and the aimed `@<sid> tab` form
/// ([`cmd_tab_aimed`]) — one grammar, one sentence.
const TAB_USAGE: &str = "ERR usage: tab <new|N|next|prev|close [N]|move <from> <to>>\n";

/// One parsed `spawn` request: the plain tab/split spawn (optionally aimed at a
/// window), or the connected form with every argument present and validated.
#[derive(Debug, PartialEq, Eq)]
enum SpawnForm {
    Plain {
        /// `window=<id>`: the window (one of the ids `inspect app/v1 tabs`
        /// prints) the newborn lands in. `None` = the `@<sid>` selector's host
        /// window when one was given, else the front window
        /// ([`SpawnAim::from_request`]).
        window: Option<u64>,
        /// `raise=<t|f>` (`true|false` too, as `split=` takes `vertical`): an
        /// EXPLICIT raise verdict. `None` defers to [`raise_after_spawn`]'s
        /// default — raise only when nothing was aimed.
        raise: Option<bool>,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
        /// `theme=<name>`: the newborn's own theme ([`crate::session_theme`]).
        theme: Option<String>,
        /// `agent=<claude|gemini|agy|muse> [model=<m>] len=<n>` (a WORKER);
        /// `None` = a shell.
        agent: Option<AgentSpec>,
    },
    Connected {
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: aterm_session::SessionId,
        cwd: Option<String>,
        /// `theme=<name>`: the newborn's own theme.
        theme: Option<String>,
    },
}

/// Pure parse of the `spawn` argument line (unit-tested off the event loop),
/// over [`split_quoted_tokens`] so a quoted `cwd=` keeps its spaces in both
/// forms. `of=` is mandatory when `connected=` is present — the App dispatch
/// discards selectors, and an authority-minting argument gets no guessed
/// default; `place=` is equally explicit. A `place=`/`of=` without `connected=`
/// is a usage error, as is any unknown token or value.
///
/// `split=`, `window=`, and `raise=` are mutually exclusive with `connected=`:
/// the connected form places by `place=`/`of=`, so an aim, raise verdict, or
/// split beside it is refused rather than one of the two silently winning.
///
/// `window=<id>` is the id `inspect app/v1 tabs` prints (a plain integer);
/// `raise=` accepts `t`/`true` or `f`/`false` — anything else is the usage line,
/// so a misspelt verdict can never be read as "use the default".
///
/// `agent=<claude|gemini|agy|muse>` makes the plain form a worker's
/// (`task_launch`; `agent=codex` is reserved):
/// `model=`, `autopilot=<land|cruise>` (the newborn's own word; never `off`,
/// which would start a worker nothing sees through), `from=` and the frame's
/// `len=` (read by the serve loop, `control::spawn_frame_len`) come only with
/// it, and it never joins the connected form.
fn parse_spawn_args(rest: &str) -> Result<SpawnForm, ()> {
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
    let (mut cwd, mut split, mut kind, mut place, mut origin) = (None, None, None, None, None);
    let (mut window, mut raise, mut theme) = (None, None, None);
    let (mut agent, mut model, mut from, mut framed) = (None, None, None, false);
    let mut autopilot = None;
    let Some(tokens) = split_quoted_tokens(rest) else {
        return Err(());
    };
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("cwd=") {
            cwd = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("theme=") {
            // A name or a `dark:…,light:…` split, quoted when it holds spaces;
            // validated on the main thread against the catalog. Empty, or given
            // twice, is the usage line.
            if v.is_empty() || theme.replace(v.to_string()).is_some() {
                return Err(());
            }
        } else if let Some(v) = tok.strip_prefix("window=") {
            window = Some(v.parse::<u64>().map_err(|_| ())?);
        } else if let Some(v) = tok.strip_prefix("raise=") {
            // Accept both `<t|f>` from the summary and the long spelling.
            raise = Some(match v {
                "t" | "true" => true,
                "f" | "false" => false,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("split=") {
            split = Some(match v {
                "v" | "vertical" => crate::pane::SplitDir::Vertical,
                "h" | "horizontal" => crate::pane::SplitDir::Horizontal,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("connected=") {
            kind = Some(match v {
                "controlled" => ConnectedSpawnKind::Controlled,
                "controller" => ConnectedSpawnKind::Controller,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("place=") {
            place = Some(match v {
                "window" => ConnectedSpawnPlace::Window,
                "tab" => ConnectedSpawnPlace::Tab,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("of=") {
            if v.is_empty() {
                return Err(());
            }
            origin = Some(aterm_session::SessionId::new(v));
        } else if let Some(v) = tok.strip_prefix("agent=") {
            agent = Some(agent_spec(v).ok_or(())?);
        } else if let Some(v) = tok.strip_prefix("model=") {
            if !valid_model(v) {
                return Err(());
            }
            model = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("autopilot=") {
            // The worker's own word: Land (the default) or Cruise. Off, a
            // word given twice or any other word is the usage line.
            let word = match v {
                "land" => aterm_agent::supervise::Autopilot::Land,
                "cruise" => aterm_agent::supervise::Autopilot::Cruise,
                _ => return Err(()),
            };
            if autopilot.replace(word).is_some() {
                return Err(());
            }
        } else if let Some(v) = tok.strip_prefix("from=") {
            // A session id, as a task record's `from=` reads one.
            if v == crate::task_record::FROM_CTL || !crate::task_record::valid_from(v) {
                return Err(());
            }
            from = Some(aterm_session::SessionId::new(v));
        } else if tok.starts_with("len=") {
            // The frame's length, which the serve loop read and consumed.
            framed = true;
        } else {
            return Err(());
        }
    }
    // `model=`, `autopilot=`, `from=` and a frame are a worker's alone.
    let agent = match agent {
        Some(AgentSpec::Worker { agent, .. }) => Some(AgentSpec::Worker {
            agent,
            model,
            autopilot,
            from,
        }),
        Some(AgentSpec::Reserved(program)) => Some(AgentSpec::Reserved(program)),
        None if model.is_some() || autopilot.is_some() || from.is_some() || framed => {
            return Err(());
        }
        None => None,
    };
    match (kind, place, origin) {
        (None, None, None) => Ok(SpawnForm::Plain {
            window,
            raise,
            cwd,
            split,
            theme,
            agent,
        }),
        (Some(kind), Some(place), Some(origin))
            if split.is_none() && window.is_none() && raise.is_none() && agent.is_none() =>
        {
            Ok(SpawnForm::Connected {
                kind,
                place,
                origin,
                cwd,
                theme,
            })
        }
        // A partial connected form (connected= without of=/place=, or a stray
        // place=/of=), or a connected form carrying split=/window=/raise= or
        // agent=, never guesses.
        _ => Err(()),
    }
}

/// `cwd=<path>` naming a folder no shell can start in is refused by name:
/// `ERR no such folder <path>` when nothing is there, `ERR no access to folder
/// <path>` when this user may not enter it. The caller named the folder and can
/// act on the refusal, where a shell started quietly in the home folder — what
/// a restored pane or a New Tab gets ([`crate::spawn_folder`]) — would mislead
/// it. `None` with no `cwd=`, a folder a shell can start in, or one the check
/// could not judge in time. Off unix nothing is refused here
/// ([`crate::spawn_folder::folder_fault`]).
fn spawn_cwd_refusal(form: &SpawnForm) -> Option<String> {
    use crate::spawn_folder::Fault;
    let (SpawnForm::Plain { cwd, .. } | SpawnForm::Connected { cwd, .. }) = form;
    let cwd = cwd.as_deref()?;
    let shown = if cwd.is_empty() { "\"\"" } else { cwd };
    match crate::spawn_folder::folder_fault(cwd)? {
        Fault::Missing => Some(format!("ERR no such folder {shown}\n")),
        Fault::Shut => Some(format!("ERR no access to folder {shown}\n")),
        // Not this check's to say: a folder that did not answer goes on to the
        // spawn, which starts the shell in the home folder when it is on a
        // volume that is not local (`folder_fault` says neither).
        Fault::Unresponsive | Fault::HomeUnresponsive => None,
    }
}

/// Where an aimed `spawn` lands: the front window (`aterm new-tab`), an explicit
/// `window=<id>`, or the window hosting the `@<sid>` selector's session (the
/// routing rule `@<sid> image` uses). Carried by [`Wake::SpawnSession`] and
/// resolved to a window on the main thread ([`crate::App::aimed_window`]) — the
/// control thread knows neither the window table nor whether the instance is
/// headless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnAim {
    Front,
    Window(u64),
    Session(u64),
}

impl SpawnAim {
    /// `window=` wins over `@<sid>` (the explicit spelling beats the implied
    /// one); `@<sid>` alone aims at its host; neither means the front window.
    const fn from_request(window: Option<u64>, session: Option<u64>) -> Self {
        match (window, session) {
            (Some(id), _) => Self::Window(id),
            (None, Some(local)) => Self::Session(local),
            (None, None) => Self::Front,
        }
    }

    /// True when the caller named a window, in either spelling — the input to
    /// [`raise_after_spawn`].
    pub(crate) const fn is_aimed(self) -> bool {
        !matches!(self, Self::Front)
    }
}

/// The raise policy, pure. An explicit `raise=` always wins; otherwise a spawn
/// that named no window raises (`aterm new-tab` attach contract) and one that
/// did name a window does not, so an agent aiming at a background window does
/// not steal keyboard focus from the foreground window.
pub(crate) const fn raise_after_spawn(window_named: bool, raise: Option<bool>) -> bool {
    match raise {
        Some(explicit) => explicit,
        None => !window_named,
    }
}

/// A WORKER'S ORDER from its spawn — `agent=<word>`'s agent, `model=`,
/// `autopilot=` and `from=`, the line's `cwd=`, and the frame the serve loop
/// read after it: the task under the spawn's own Autopilot word, Land unless
/// it says cruise ([`crate::task_launch::TaskOrder::under`]). `Err` is the
/// reply. One door for [`cmd_spawn`] and a test, so the spawn's word cannot
/// be dropped with every test green.
fn worker_order(
    frame: Option<Vec<u8>>,
    agent: aterm_phase::Program,
    model: Option<String>,
    autopilot: Option<aterm_agent::supervise::Autopilot>,
    from: Option<aterm_session::SessionId>,
    cwd: Option<&str>,
) -> Result<crate::task_launch::TaskOrder, String> {
    crate::task_launch::TaskOrder::of_frame(frame, agent, model, cwd, from)
        .map(|order| order.under(autopilot))
}

/// `spawn [window=<id>] [raise=<t|f>] [cwd=<path>] [split=<v|h>]` ->
/// mint ONE new session and reply `OK <sid>` — birth as a socket primitive.
/// The sid is live in the registry before the reply is sent, so `@<sid> …`
/// works immediately: an orchestrator stands up a fleet with a loop of spawn
/// calls and drives each newborn with `turn`/`send`/`subscribe`, no process
/// management.
///
/// Aiming: the newborn lands in the frontmost window unless the
/// caller names one: `window=<id>` (an id from `inspect app/v1 tabs`) or the
/// `@<sid>` selector, which means "the window hosting `<sid>`" — `session` is
/// that selector's resolved local id, `None` for a flagless request, as `image`
/// receives it; only an Owner reaches here with a selector — the aimed forms
/// keep the App authority verdict every other App-target selector gets
/// (`control::app_route`). `window=` wins over `@<sid>`. `split=` on an aimed
/// spawn divides that window's focused pane. The main thread answers
/// `ERR no such window <id>` for an id no window carries; a headless instance
/// owns logical window 0 (the one `ls`/`dims` name), so `window=0` and `@<sid>`
/// resolve there just as they do to a real window and only `window=<other>` is
/// `ERR no such window`. `raise=` is decided here, purely
/// ([`raise_after_spawn`]): explicit wins, else only an un-aimed spawn raises.
///
/// `cwd=<path>` sets the newborn's working directory (default: inherit the
/// focused pane's cwd, like Cmd-T); a path containing spaces is quoted
/// (`cwd="C:\Program Files\Git"`, see [`split_quoted_tokens`]). On macOS
/// and Linux a path no shell can start in is refused, `ERR no such folder
/// <path>` or `ERR no access to folder <path>` ([`spawn_cwd_refusal`]); an
/// inherited folder a shell cannot start in starts the newborn in the home
/// folder, and a Messages row says so — unless a program other than that
/// pane's shell (ssh, a container's shell) holds the pane: that folder may be
/// another machine's, so it is not taken, and the newborn starts where it
/// would with none ([`crate::spawn_folder::inherited`]).
///
/// `split=v` / `split=h` divides the focused pane instead of opening a tab —
/// side-by-side and stacked respectively, the same two directions Cmd-D and
/// Cmd-Shift-D take (the wire half of `aterm split-pane [-d <dir>]`).
///
/// The newborn runs the default shell; give it a command with
/// `@<sid> turn '<cmd>'`. Main-thread hop like `open`/`controls`.
///
/// `spawn connected=controlled|controller place=window|tab of=<sid> [cwd=…]`
/// additionally mints the `both` session connection binding the newborn to
/// `of=` — Owner-only (the `argument_escalation` fence runs before this
/// dispatch). The handler resolves `of=` on the main thread and refuses
/// `place=window` under headless with `ERR headless`; `place=tab` works
/// headless. cwd default for the connected form is the origin session's cwd.
///
/// `spawn … agent=<claude|gemini|agy|muse> [model=<m>]
/// [autopilot=<land|cruise>] [from=<sid>] len=<n>` starts a WORKER
/// (`task_launch`; Owner-only, the `argument_escalation` fence) running that
/// agent: the task is the frame the serve loop read after the line
/// ([`crate::task_launch::take_frame`]), `autopilot=` the newborn's own
/// Autopilot word (land unless it says cruise), and `from=` — the client's
/// own session, which the `@<sid>` aim need not be — is the task's `from=`.
/// `agent=codex` is `ERR agent codex: not yet`.
pub(crate) fn cmd_spawn(proxy: &EventLoopProxy<Wake>, rest: &str, session: Option<u64>) -> String {
    let Ok(form) = parse_spawn_args(rest) else {
        return SPAWN_USAGE.to_string();
    };
    if let Some(refusal) = spawn_cwd_refusal(&form) {
        return refusal;
    }
    let sent = match form {
        SpawnForm::Plain {
            window,
            raise,
            cwd,
            split,
            theme,
            agent,
        } => {
            let task = match agent {
                None => None,
                Some(AgentSpec::Reserved(program)) => {
                    return format!("ERR agent {}: not yet\n", program.name());
                }
                Some(AgentSpec::Worker {
                    agent,
                    model,
                    autopilot,
                    from,
                }) => match worker_order(
                    crate::task_launch::take_frame(),
                    agent,
                    model,
                    autopilot,
                    from,
                    cwd.as_deref(),
                ) {
                    Ok(order) => Some(order),
                    Err(refusal) => return refusal,
                },
            };
            let aim = SpawnAim::from_request(window, session);
            let raise = raise_after_spawn(aim.is_aimed(), raise);
            call_main(proxy, |tx| Wake::SpawnSession {
                aim,
                cwd,
                split,
                raise,
                theme,
                task,
                reply: tx,
            })
        }
        SpawnForm::Connected {
            kind,
            place,
            origin,
            cwd,
            theme,
        } => call_main(proxy, |tx| Wake::SpawnConnectedSession {
            kind,
            place,
            origin,
            cwd,
            theme,
            reply: tx,
        }),
    };
    match sent {
        Ok(Ok(sid)) => format!("OK {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `@<sid> tab new | <N> | next | prev | close [N] | move <from> <to>` -> drive
/// the tabs of the window HOSTING the resolved session (design S3) and reply
/// `OK <active_index> <tab_count>` — the aimed twin of
/// [`super::control_input::cmd_tab`], which drives the FRONT window. Same
/// grammar, same reply; only the aim can additionally fail — a session no
/// window hosts cannot be driven through one, and an unknown `@<sid>` is
/// `ERR no such session` — so the main-thread hop ([`Wake::TabCmdAimed`])
/// replies a `Result` where its sibling replies a pair. A headless instance
/// drives its one logical window like a real one (no `ERR headless`). `by` is
/// the caller ([`super::control_session::caller_actor`]), the exit ledger's
/// `by=` for a session `close` retires.
pub(crate) fn cmd_tab_aimed(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    session: u64,
    by: crate::session_store::ExitActor,
) -> String {
    let Some(action) = super::control_input::parse_tab(rest) else {
        return TAB_USAGE.to_string();
    };
    match call_main(proxy, |reply| Wake::TabCmdAimed {
        session,
        action,
        by,
        reply,
    }) {
        Ok(Ok((active, count))) => format!("OK {active} {count}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(error) => format!("ERR tab command failed: {error}\n"),
    }
}

/// Why this instance starts no worker, whatever its task
/// ([`crate::App::worker_shell`]): the spawn's refusal before anything is
/// created, and why the menu bar's worker launch (track B's Summarize),
/// which starts one, is grey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerBlock {
    /// No window harness runs here: an instance with no control socket
    /// (`--no-control-sock`, or a socket that could not be bound) starts no
    /// host (`HarnessPosture::Unhosted`), so nothing would carry the worker.
    /// The wire's `spawn` never meets it — it came over the socket — but the
    /// menu bar's worker launch (track B's Summarize) does.
    Unhosted,
    /// The window harness would not carry the worker's agent's session, so
    /// nothing would finish the task: the switch that stops it
    /// (`harness_host::worker_unsupervised` — `[harness] enabled = false`, a
    /// headless instance's `headless = false`, `[harness.<agent>] enabled =
    /// false`, or `[harness] autopilot = "off"`).
    Unsupervised(&'static str),
    /// Shell integration is off (`--no-shell-integration`, or an instance
    /// started to run a command): no prompt is marked to type the worker's
    /// line at.
    NoIntegration,
    /// The shell is not one a worker's line is written for: its program's
    /// name (`task_launch::worker_dialect`).
    Shell(String),
    /// The shell is a fish its `--version` said is older than 3.4, whose
    /// parser refuses the line's task operand (`task_launch::fish_known_old`).
    OldFish,
}

impl WorkerBlock {
    /// The spawn's reply: `ERR <this>`, `agent` the worker's.
    pub(crate) fn refusal(self, agent: aterm_phase::Program) -> String {
        let word = agent.name();
        match self {
            Self::Unhosted => {
                "unsupervised: no window harness runs here (no control socket)".to_string()
            }
            Self::Unsupervised(switch) => format!("unsupervised: {switch}"),
            Self::NoIntegration => format!(
                "spawn agent={word}: shell integration is off, so no prompt is marked to type at"
            ),
            Self::Shell(name) => {
                format!("spawn agent={word}: the shell {name} is not zsh, bash or fish")
            }
            Self::OldFish => format!(
                "spawn agent={word}: the shell is a fish older than 3.4, which cannot take the \
                 task's operand"
            ),
        }
    }
}

/// What a worker's start needs once its checks passed
/// ([`crate::App::worker_precheck`]).
pub(crate) struct WorkerStart {
    order: crate::task_launch::TaskOrder,
    /// The task's `from=`: the starter's global sid, or `ctl`.
    from: String,
    /// The starter's local id (`from=`'s session), whose agent of the
    /// worker's kind the worker's line follows; `None` for `from=ctl`, or a
    /// starter another instance hosts.
    caller: Option<u64>,
    /// The worker's shell and the dialect its line is written in.
    shell: std::path::PathBuf,
    dialect: aterm_agent::harness::upgrade::Dialect,
}

impl WorkerStart {
    /// The seed of the worker's task file: its task, and the fact line naming
    /// its `from=` when the order asks for one
    /// ([`crate::task_launch::TaskOrder::fact_line`]).
    fn seed(&self) -> crate::task_launch::TaskSeed {
        let from = self.order.fact_line.then_some(self.from.as_str());
        let seed = crate::task_launch::TaskSeed::new(crate::task_launch::task_text(
            &self.order.task,
            from,
            self.order.autopilot,
            self.order.agent,
        ));
        let trust = aterm_agent::harness::worker::worker_cli(self.order.agent)
            .and_then(|cli| cli.trust_file_env)
            .and(self.order.trusted_cwd.as_deref());
        match trust {
            Some(folder) => seed.with_trust_file(folder),
            None => seed,
        }
    }
}

/// A spawn's outcome on the main thread ([`crate::App::spawn_session_aimed`]):
/// the newborn's sid, and for a WORKER what is left of its start once the
/// reply is taken ([`crate::App::answer_spawn`]).
pub(crate) struct Spawned {
    sid: String,
    worker: Option<PendingWorker>,
}

/// A worker spawned and marked `working`, its launcher not yet started
/// ([`crate::App::ready_worker`]).
pub(crate) struct PendingWorker {
    launch: crate::task_launch::Launch,
    /// The seed that wrote its task file, which an undo removes.
    seed: crate::task_launch::TaskSeed,
}

/// What a spawn's booking asks beyond its aim and a worker's task
/// ([`crate::App::book_spawn`]).
pub(crate) struct SpawnBooking {
    /// `spawn cwd=`: the folder the newborn starts in; `None` inherits the
    /// focused pane's.
    pub(crate) cwd: Option<String>,
    /// `spawn split=<v|h>`: split the window's focused pane instead of
    /// opening a tab.
    pub(crate) split: Option<crate::pane::SplitDir>,
    /// Bring the hosting window to the front once the newborn lands
    /// ([`raise_after_spawn`]); a TAB that does not raise lands behind the
    /// window's active one.
    pub(crate) raise: bool,
    /// What the landing checks again (`tab_spawn::SpawnAdmission`): the menu
    /// bar's worker launch's reservation, else nothing.
    pub(crate) admission: crate::tab_spawn::SpawnAdmission,
}

/// A spawn checked before anything is booked ([`crate::App::resolve_spawn`]):
/// the window that hosts it, the theme it asked for, and a worker's start.
pub(crate) struct SpawnPlan {
    owner: crate::WindowId,
    theme: Option<crate::session_theme::SessionTheme>,
    worker: Option<WorkerStart>,
}

impl SpawnPlan {
    /// What the spawner is asked for beyond the booking's folder, place and
    /// admission ([`crate::App::place_spawn`]): the theme and a worker's task file.
    fn ask(
        theme: Option<crate::session_theme::SessionTheme>,
        task: Option<crate::task_launch::TaskSeed>,
    ) -> crate::tab_spawn::SpawnAsk {
        crate::tab_spawn::SpawnAsk {
            theme,
            task,
            ..crate::tab_spawn::SpawnAsk::default()
        }
    }
}

/// What takes a booked spawn's outcome on the main thread
/// ([`crate::App::book_spawn`]): the newborn, registered and, for a worker,
/// readied — or why there is none.
pub(crate) type SpawnTaken = Box<dyn FnOnce(&mut crate::App, Result<Spawned, String>)>;

/// Where a wire `tab` verb's `(active_index, tab_count)` or refusal goes.
pub(crate) type TabCmdReply = std::sync::mpsc::Sender<Result<(usize, usize), String>>;

/// The main-thread half of the aimed `spawn`/`tab` verbs. It sits beside the
/// verbs' control-thread half rather than in `app_tabs.rs` because it is verb
/// policy — which window a wire request means, and whether a spawn may take the
/// foreground — not tab mechanics; the mechanics it calls
/// (`open_tab_off_loop`, `split_focused_pane_in_window`, `apply_tab_cmd_in`)
/// stay window-parameterised beside every other caller of theirs.
impl crate::App {
    /// Resolve a [`SpawnAim`] to the window it names. `Front` is `Ok(None)` —
    /// "whatever is frontmost when the spawn runs"; a named window resolves
    /// through [`Self::named_window`] / [`Self::hosting_window`], both of which
    /// refuse before anything is created. A headless instance owns one logical
    /// window (id 0, the one `ls`/`windows`/`dims` report) — so `window=0` and
    /// `@<sid>` resolve to it there as they do to a real window, and only
    /// `window=<other>` is `ERR no such window`; raising the resolved window is
    /// a no-op headless (it has no OS surface — [`Self::spawn_session_aimed`]).
    pub(crate) fn aimed_window(&self, aim: SpawnAim) -> Result<Option<crate::WindowId>, String> {
        match aim {
            SpawnAim::Front => Ok(None),
            SpawnAim::Window(id) => self.named_window(id).map(Some),
            SpawnAim::Session(local) => self.hosting_window(local).map(Some),
        }
    }

    /// `window=<id>`: one of the ids `inspect app/v1 tabs` prints (`dims`'
    /// `window=` is the same number, and a headless instance's is `0`). An id
    /// not in the table is refused by name — `ERR no such window <id>` — never
    /// rounded to the front window.
    fn named_window(&self, id: u64) -> Result<crate::WindowId, String> {
        let wid = crate::WindowId(id);
        if self.windows.contains_key(&wid) {
            Ok(wid)
        } else {
            Err(format!("no such window {id}"))
        }
    }

    /// `@<sid>`: the window whose pane trees contain the session (any tab, any
    /// pane — [`Self::window_of_session`], the roster's front-else-lowest rule).
    /// A registered session no window hosts cannot be aimed through; say so
    /// rather than fall back to the front window the caller pointedly did not
    /// name.
    fn hosting_window(&self, local: u64) -> Result<crate::WindowId, String> {
        self.window_of_session(local)
            .ok_or_else(|| "no window hosts the target session".to_string())
    }

    /// `spawn` on the main thread: resolve the aim, then answer `reply`.
    ///
    /// A TAB ([`Self::open_off_loop`]) and a SPLIT
    /// ([`Self::split_focused_pane_in_window`]) are both spawned off the event
    /// loop ([`Self::place_spawn`]): `reply` waits with the session in flight
    /// and is answered when it lands, with the sid once it is registered, so
    /// the reply still comes after the sid is addressable. A split the focused
    /// pane cannot take is refused at once, and again at landing if the window
    /// changed meanwhile. Either way the raise verdict the control thread
    /// already decided ([`raise_after_spawn`]) applies only to a SUCCESSFUL
    /// spawn (a refused request must not steal the foreground), and on the
    /// window the session actually landed in, so an explicit `raise=true` on
    /// an aimed spawn raises THAT window, not the front one. The apprt decides
    /// what raising means (Windows: SW_RESTORE + SetForegroundWindow); the
    /// default is nothing, so no other platform's focus behaviour moves here.
    ///
    /// A spawn that does NOT raise keeps its window's active tab: the newborn
    /// tab lands behind the tab active then (`tab_spawn::SpawnAsk::background`,
    /// the `tab_strip` model's NewTab then SelectTab), so the person's view
    /// stays put. A split focuses its new pane as it always did. `theme=` is
    /// checked before anything is asked of the spawner ([`Self::spawn_theme`]):
    /// a name that does not draw spawns nothing.
    ///
    /// A WORKER (`task`, `spawn agent=claude`): refused BEFORE anything is
    /// created when the window harness would not carry it (`ERR unsupervised:
    /// <switch>`: nothing would finish the task) or the shell is no shell its
    /// line is written for ([`Self::worker_precheck`]); then spawned with its
    /// task file, and its task record marked `working` once its newborn is
    /// registered and before the reply ([`Self::ready_worker`]).
    /// A worker that spawned and then could not start is closed again, its
    /// task file removed, and the reply is the reason
    /// ([`Self::unspawn_worker`]): an `ERR` leaves nothing behind. Its
    /// launcher starts only once the reply is taken ([`Self::answer_spawn`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "the spawn verb's arguments as the control thread parsed them — aim, cwd, \
                  split, raise, theme and a worker's task — and `reply` its answer; \
                  a struct would only rename the Wake's fields"
    )]
    pub(crate) fn spawn_session_aimed(
        &mut self,
        aim: SpawnAim,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
        raise: bool,
        theme: Option<String>,
        task: Option<crate::task_launch::TaskOrder>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) {
        let theme = match self.spawn_theme(theme) {
            Ok(theme) => theme,
            Err(why) => {
                let _ = reply.send(Err(why));
                return;
            }
        };
        let booking = SpawnBooking {
            cwd,
            split,
            raise,
            admission: crate::tab_spawn::SpawnAdmission::Plain,
        };
        match self.resolve_spawn(aim, task) {
            // A plain spawn: `OK <sid>` once it lands, its window raised first
            // when `raise` says so (`TabSpawnThen::Reply`); its tab stays
            // whether the reply is taken or not, as it always did.
            Ok(SpawnPlan {
                owner,
                worker: None,
                ..
            }) => {
                let then = crate::tab_spawn::TabSpawnThen::Reply { reply, raise };
                let ask = SpawnPlan::ask(theme, None);
                self.place_spawn(owner, ask, booking, then);
            }
            Ok(mut plan) => {
                plan.theme = theme;
                self.book_spawn(
                    plan,
                    booking,
                    Box::new(move |app, spawned| app.answer_spawn(spawned, &reply)),
                );
            }
            Err(why) => {
                let _ = reply.send(Err(why));
            }
        }
    }

    /// A spawn's `theme=`, checked as `theme session set` checks a selector:
    /// `ERR theme "<name>": <why>` for one with grammar issues or a side that
    /// would draw Default for want of a theme.
    pub(crate) fn spawn_theme(
        &self,
        theme: Option<String>,
    ) -> Result<Option<crate::session_theme::SessionTheme>, String> {
        let Some(selector) = theme else {
            return Ok(None);
        };
        if let Some(why) = self.theme_preview_unavailable(&selector) {
            return Err(format!("theme {selector:?}: {why}"));
        }
        Ok(Some(crate::session_theme::SessionTheme {
            selector,
            origin: crate::session_theme::SessionThemeOrigin::Spawn,
        }))
    }

    /// The aimed `spawn`'s checks, before anything is booked: the window
    /// ([`Self::aimed_window`], else the front one), a worker's checks
    /// ([`Self::worker_precheck`]).
    pub(crate) fn resolve_spawn(
        &self,
        aim: SpawnAim,
        task: Option<crate::task_launch::TaskOrder>,
    ) -> Result<SpawnPlan, String> {
        let host = self.aimed_window(aim)?;
        let worker = task.map(|order| self.worker_precheck(order)).transpose()?;
        let owner = host
            .or(self.frontmost_window)
            .ok_or_else(|| "no window to host the session".to_string())?;
        Ok(SpawnPlan {
            owner,
            theme: None,
            worker,
        })
    }

    /// Book `plan`'s spawn off the event loop ([`Self::place_spawn`]), a
    /// WORKER's with its task file. `taken` hears the newborn once it is
    /// registered — a worker readied first, its window raised when
    /// `booking.raise` says so ([`Self::landed_spawn`]) — or why it never
    /// landed, a worker's task file then removed.
    pub(crate) fn book_spawn(&mut self, plan: SpawnPlan, booking: SpawnBooking, taken: SpawnTaken) {
        let SpawnPlan {
            owner,
            theme,
            worker,
        } = plan;
        let seed = worker.as_ref().map(WorkerStart::seed);
        let raise = booking.raise;
        let ask = SpawnPlan::ask(theme, seed.clone());
        let then = crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| {
            let spawned = match landed {
                Ok(sid) => {
                    let worker = worker.zip(seed);
                    app.landed_spawn(sid, owner, raise, worker)
                }
                Err(why) => {
                    // A shell that did not start may leave its task file.
                    if let Some(seed) = &seed {
                        seed.discard();
                    }
                    Err(why)
                }
            };
            taken(app, spawned);
        }));
        self.place_spawn(owner, ask, booking, then);
    }

    /// Book a spawn landing in `owner` (`tab_spawn`), asked for as `ask` says
    /// (its theme, a worker's task file): a tab, or a split of
    /// the focused pane (refused at once when it cannot take one, and again at
    /// landing, verbatim: `ERR Split refused: this pane is 19x11 cells; ...`).
    /// A tab that does not raise lands behind the window's active one. `then`
    /// is its waiter.
    fn place_spawn(
        &mut self,
        owner: crate::WindowId,
        ask: crate::tab_spawn::SpawnAsk,
        booking: SpawnBooking,
        then: crate::tab_spawn::TabSpawnThen,
    ) {
        let SpawnBooking {
            cwd,
            split,
            raise,
            admission,
        } = booking;
        let ask = crate::tab_spawn::SpawnAsk {
            admission,
            cwd,
            background: !raise && split.is_none(),
            ..ask
        };
        match split {
            None => self.open_off_loop(owner, crate::tab_spawn::SpawnPlace::Tab, ask, then),
            // A refusal is answered through `then`.
            Some(dir) => {
                let _ = self.split_focused_pane_in_window(owner, dir, ask, then);
            }
        }
    }

    /// A booked spawn's newborn `sid`, registered in `owner`: a worker made
    /// one ([`Self::ready_worker`]) — or closed again, the reason the outcome
    /// — then `owner` raised when `raise` says so. A spawn that failed raises
    /// nothing.
    fn landed_spawn(
        &mut self,
        sid: String,
        owner: crate::WindowId,
        raise: bool,
        worker: Option<(WorkerStart, crate::task_launch::TaskSeed)>,
    ) -> Result<Spawned, String> {
        let worker = match worker {
            Some((worker, seed)) => Some(self.ready_worker(&sid, worker, seed)?),
            None => None,
        };
        if raise && let Some(window) = self.windows.get(&owner).and_then(|ws| ws.os_window.clone())
        {
            self.apprt.window_bring_to_front(&window);
        }
        Ok(Spawned { sid, worker })
    }

    /// Answer a spawn on `reply`, and only then start its worker's launcher.
    /// A worker whose `OK <sid>` nobody took — the control lane stopped
    /// waiting (`call_main`'s deadline) and dropped its end — is closed again
    /// and its task file removed before anything is typed
    /// ([`Self::unspawn_worker`]): no one holds its sid, so it would do its
    /// task unowned, and the client's retry would run the task twice. A plain
    /// spawn's tab stays whether its reply was taken or not, as it always did.
    pub(crate) fn answer_spawn(
        &mut self,
        spawned: Result<Spawned, String>,
        reply: &std::sync::mpsc::Sender<Result<String, String>>,
    ) {
        let Spawned { sid, worker } = match spawned {
            Ok(spawned) => spawned,
            Err(why) => {
                let _ = reply.send(Err(why));
                return;
            }
        };
        let taken = reply.send(Ok(sid.clone())).is_ok();
        match worker {
            Some(worker) if taken => {
                aterm_log::info!(
                    "worker {sid}: its spawn's reply was taken; its launcher starts (session {})",
                    worker.launch.local
                );
                crate::task_launch::launch_worker(worker.launch);
            }
            Some(worker) => {
                aterm_log::warn!(
                    "worker {sid}: its spawn's reply was not taken; it is closed again"
                );
                self.unspawn_worker(&sid, Some(&worker.seed));
            }
            None => {}
        }
    }

    /// A spawn the main thread made for itself (Summarize in a New Tab,
    /// `App::summarize_in_new_tab`): no client waits on a reply, so its
    /// worker's launcher starts at once. The newborn's sid is returned.
    pub(crate) fn start_spawned(&mut self, spawned: Spawned) -> String {
        if let Some(worker) = spawned.worker {
            aterm_log::info!(
                "worker {}: its launcher starts (session {}, no reply owed)",
                spawned.sid,
                worker.launch.local
            );
            crate::task_launch::launch_worker(worker.launch);
        }
        spawned.sid
    }

    /// What a worker running `agent` needs of this instance, whatever its
    /// task (Summarize in a New Tab gates on it, for Claude Code): a window
    /// harness runs here (a host: this instance has a control socket) and
    /// would carry the worker's `agent` session
    /// (`harness_host::worker_unsupervised`, else the switch that stops it —
    /// the harness off, a headless instance's `headless = false`, the agent's
    /// own table off, or an Autopilot default of `off`), the shell marks its
    /// prompts (integration on), and
    /// the shell is one a worker's line is written for
    /// (`task_launch::worker_dialect`), a fish not known to be older than 3.4
    /// (`task_launch::fish_known_old`, whose first look at a fish asks its
    /// version off this thread) — that shell and its dialect.
    pub(crate) fn worker_shell(
        &self,
        agent: aterm_phase::Program,
    ) -> Result<(std::path::PathBuf, aterm_agent::harness::upgrade::Dialect), WorkerBlock> {
        if self.harness.is_none() {
            return Err(WorkerBlock::Unhosted);
        }
        if let Some(switch) = crate::harness_host::worker_unsupervised(
            &self.harness_policy.get().policy,
            self.headless,
            agent,
        ) {
            return Err(WorkerBlock::Unsupervised(switch));
        }
        if !self.session_factory.integrate {
            return Err(WorkerBlock::NoIntegration);
        }
        let shell =
            crate::spawn::interactive_shell_program(self.session_factory.shell_override.as_deref());
        let dialect = crate::task_launch::worker_dialect(&shell).map_err(WorkerBlock::Shell)?;
        if dialect == aterm_agent::harness::upgrade::Dialect::Fish
            && crate::task_launch::fish_known_old(&shell)
        {
            return Err(WorkerBlock::OldFish);
        }
        Ok((shell, dialect))
    }

    /// A worker's checks before anything is created ([`Self::worker_shell`]),
    /// and what its start needs ([`WorkerStart`]). The task's `from=` is the
    /// spawn's `from=` — the client's own session, which another instance may
    /// host (then no agent of its rides the line) — or `ctl`.
    fn worker_precheck(&self, order: crate::task_launch::TaskOrder) -> Result<WorkerStart, String> {
        let (shell, dialect) = self
            .worker_shell(order.agent)
            .map_err(|block| block.refusal(order.agent))?;
        let (from, caller) = match &order.caller {
            None => (crate::task_record::FROM_CTL.to_string(), None),
            Some(sid) => {
                let g = self.store.read().unwrap_or_else(|p| p.into_inner());
                match g.by_sid(sid) {
                    Some(h) => (h.sid.as_str().to_string(), Some(h.local_id)),
                    None => (sid.as_str().to_string(), None),
                }
            }
        };
        Ok(WorkerStart {
            order,
            from,
            caller,
            shell,
            dialect,
        })
    }

    /// The newborn `sid` made a worker ([`Self::start_worker`]), its
    /// launcher left for [`Self::answer_spawn`] to start — or, when it cannot
    /// be one, closed again and its task file removed
    /// ([`Self::unspawn_worker`]), the reason the reply.
    fn ready_worker(
        &mut self,
        sid: &str,
        worker: WorkerStart,
        seed: crate::task_launch::TaskSeed,
    ) -> Result<PendingWorker, String> {
        match self.start_worker(sid, worker) {
            Ok(mut launch) => {
                launch.trust_file = seed.trust_file();
                Ok(PendingWorker { launch, seed })
            }
            Err(why) => {
                self.unspawn_worker(sid, Some(&seed));
                Err(why)
            }
        }
    }

    /// The newborn `sid` is a worker: its folder trusted for it alone (the
    /// window harness's folder-trust rule reads `meta`'s `trusted_cwd=`), its
    /// own Autopilot word written (`land` unless the spawn said `cruise`:
    /// FOREVER LAND, a worker's one wake at the end is its landing), and its
    /// task record `working` with its `from=` — on the main thread, so
    /// with no wait on a Commit's fence (`task_record::mark_session_task_within`)
    /// — before the reply; what its launcher needs is returned
    /// ([`crate::task_launch::launch_worker`]). The line carries the starter's
    /// agent homes and login from the starter when it is hosted here. The
    /// agent it starts is named on its task record, for the startup deadline's reader
    /// (`harness_host::startup_seen`) and the window harness (`meta`'s
    /// `worker=`), and a seamless update carries it with the record.
    fn start_worker(
        &self,
        sid: &str,
        worker: WorkerStart,
    ) -> Result<crate::task_launch::Launch, String> {
        let (newborn, caller) = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            let newborn = g
                .by_sid(&aterm_session::SessionId::new(sid))
                .map(|h| (h.local_id, h.term.clone(), h.ctx.clone()));
            let caller = worker
                .caller
                .and_then(|local| g.by_local(local).map(|h| h.ctx.clone()));
            (newborn, caller)
        };
        let Some((local, term, ctx)) = newborn else {
            return Err("the worker did not register".to_string());
        };
        ctx.meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .trusted_cwd
            .clone_from(&worker.order.trusted_cwd);
        // The newborn's word, beside its trusted folder and before its agent
        // runs: its loop's first read is the worker's own Autopilot. aterm's
        // write, not a person's.
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some(worker.order.autopilot.word().to_string()),
            None,
            aterm_slog::Actor::Aterm,
            None,
        );
        let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
        mark.from = Some(worker.from);
        crate::task_record::mark_session_task_within(&ctx, local, &mark, std::time::Duration::ZERO)
            .map_err(|_| "busy update-committing".to_string())?;
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_task_agent(worker.order.agent);
        let human = ctx.human_input.seq();
        Ok(crate::task_launch::Launch {
            local,
            term,
            ctx,
            store: self.store.clone(),
            proxy: self.proxy.clone(),
            human,
            shell: worker.shell,
            dialect: worker.dialect,
            agent: worker.order.agent,
            model: worker.order.model,
            trusted_cwd: worker.order.trusted_cwd,
            trust_file: None,
            caller,
        })
    }

    /// Undo a worker's spawn that could not start or whose reply nobody took
    /// ([`Self::ready_worker`], [`Self::answer_spawn`]): close the newborn
    /// `sid` with no confirm — nothing was typed into it, and its shell's rc
    /// files are all that can run there — and remove the task file its seed
    /// wrote.
    fn unspawn_worker(&mut self, sid: &str, seed: Option<&crate::task_launch::TaskSeed>) {
        let local = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|h| h.local_id);
        if let Some(local) = local {
            let _closing = crate::session_store::CloseAttribution::enter(
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Ctl,
            );
            let policy = std::mem::replace(
                &mut self.close_confirm,
                crate::app_window::CloseConfirm::Programmatic,
            );
            if let Err(why) = self.close_session_by_id(local) {
                aterm_log::warn!("worker {sid} did not start and could not be closed: {why}");
            }
            self.close_confirm = policy;
        }
        if let Some(seed) = seed {
            seed.discard();
        }
    }

    /// `@<sid> tab …` on the main thread: [`Self::tab_cmd_bracketed`] applied
    /// to the window hosting `session` instead of the front one, answered on
    /// `reply` ([`Self::tab_new_answered`] for `tab new`).
    pub(crate) fn tab_cmd_aimed(
        &mut self,
        el: &ActiveEventLoop,
        session: u64,
        action: crate::TabAction,
        by: crate::session_store::ExitActor,
        reply: TabCmdReply,
    ) {
        let wid = self.hosting_window(session);
        if matches!(action, crate::TabAction::New) {
            return self.tab_new_answered(wid, reply);
        }
        let state = wid.and_then(|wid| {
            self.tab_cmd_bracketed(el, by, |app| app.apply_tab_cmd_in(wid, action))
        });
        let _ = reply.send(state);
    }

    /// The flagless `tab …` on the main thread (the [`Wake::TabCmd`] arm):
    /// [`Self::tab_cmd_bracketed`] applied to the FRONT window, resolved by
    /// [`Self::apply_tab_cmd`] exactly as before so a window-less instance keeps
    /// its `no window is focused` refusal, answered on `reply`
    /// ([`Self::tab_new_answered`] for `tab new`).
    pub(crate) fn tab_cmd_front(
        &mut self,
        el: &ActiveEventLoop,
        action: crate::TabAction,
        by: crate::session_store::ExitActor,
        reply: TabCmdReply,
    ) {
        if matches!(action, crate::TabAction::New) {
            let front = self
                .frontmost_window
                .ok_or_else(|| crate::app_tabs::NO_FOCUSED_WINDOW.to_string());
            return self.tab_new_answered(front, reply);
        }
        let _ = reply.send(self.tab_cmd_bracketed(el, by, |app| app.apply_tab_cmd(action)));
    }

    /// `tab new` on `window`: its reply counts the new tab, so the tab's shell
    /// spawns off the event loop and `reply` is answered once the tab is in the
    /// window (`OK <active> <count>`), or with why it never landed. The wire
    /// bracket's move-license fence runs first, as for every `tab` action; its
    /// close policy and exit attribution have nothing to govern here.
    fn tab_new_answered(&mut self, window: Result<crate::WindowId, String>, reply: TabCmdReply) {
        match window {
            Ok(wid) => {
                self.clear_tab_surface_move_license(None);
                self.open_tab_off_loop(wid, None, crate::tab_spawn::TabSpawnThen::Counts(reply));
            }
            Err(why) => {
                let _ = reply.send(Err(why));
            }
        }
    }

    /// The wire `tab` bracket, shared by the front and aimed forms so the two
    /// spellings of `tab close` cannot carry different close policies.
    ///
    /// A control-socket `tab close` is a non-interactive instruction and must
    /// not pop the blocking native confirm dialog (which would wedge the UI
    /// thread inside `runModal` and the client's blocking reply behind it). It
    /// runs the `close` verb's
    /// [`CloseConfirm::WireRefuseBusy`](crate::app_window::CloseConfirm): no
    /// dialog ever, an idle close proceeds, a busy last-tab close is refused in
    /// the `close` verb's words (`ERR close refused (a running job armed the
    /// last-tab confirm)`) and a busy tab among several as `close` refuses its
    /// pane (`App::close_tab_via_verb`), carried to the reply by
    /// `pending_action_refusal`.
    /// A last-tab close that proceeds flags `pending_close`, escalated here so
    /// the window tears down (the close paths hold no `ActiveEventLoop`).
    ///
    /// The bracket also names who closed: [`Self::tab_cmd_applied`] opens the
    /// `ctl-close` scope with the caller `by`, so a `tab close` journals `by=ctl`
    /// (or the edge caller's sid) the way the `close` verb does.
    fn tab_cmd_bracketed(
        &mut self,
        el: &ActiveEventLoop,
        by: crate::session_store::ExitActor,
        apply: impl FnOnce(&mut Self) -> Result<(usize, usize), String>,
    ) -> Result<(usize, usize), String> {
        let state = self.tab_cmd_applied(by, apply);
        self.escalate_pending_close(el);
        self.close_confirm = crate::app_window::CloseConfirm::Interactive;
        state
    }

    /// The part of [`Self::tab_cmd_bracketed`] that needs no event loop: arm the
    /// wire close policy (left armed through the escalation — the bracket
    /// restores `Interactive` after it) and apply the action inside a
    /// `ctl-close` scope attributed to `by`. The scope ends with the action: a
    /// last-tab close has only FLAGGED its window by then, and the window
    /// stashes this attribution for the deferred teardown, so the escalation
    /// journals it without a scope of its own — and without lending it to
    /// another window's pending close.
    fn tab_cmd_applied(
        &mut self,
        by: crate::session_store::ExitActor,
        apply: impl FnOnce(&mut Self) -> Result<(usize, usize), String>,
    ) -> Result<(usize, usize), String> {
        self.clear_tab_surface_move_license(None);
        self.close_confirm = crate::app_window::CloseConfirm::WireRefuseBusy;
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            by,
        );
        // NOT `?`: the wire-policy bracket must be unwound even on a refusal,
        // or a declined close would leave `close_confirm` pinned to the wire
        // policy and the next human gesture would skip its dialog.
        apply(self)
    }

    /// `invoke <action>` on the main thread (the [`Wake::InvokeMenuAction`]
    /// arm): the action runs inside a `ctl-close` scope attributed to the
    /// caller `by`, so a session it retires journals `reason=ctl-close
    /// by=<caller>` the way the `close` and `tab close` verbs do. `invoke
    /// CloseTab` lands in `close_active_tab`, Cmd-W's own path, which claims
    /// `ui-close by=human` for the gestures that reach it directly; the scope
    /// is outermost-wins, so the wire caller's attribution holds, and a
    /// last-tab close stashes it on its window for the deferred teardown.
    pub(crate) fn invoke_attributed(
        &mut self,
        by: crate::session_store::ExitActor,
        invoke: impl FnOnce(&mut Self) -> Result<String, String>,
    ) -> Result<String, String> {
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            by,
        );
        invoke(self)
    }
}

/// `@<sid> close` -> retire the resolved session by id (the death half of `spawn`):
/// close the PANE showing it — a split collapses onto its siblings the way Cmd-W
/// collapses one, and the tab goes only with its last pane
/// (`App::close_session_by_id`). Reply `OK closed <sid>` on success, `ERR <why>`
/// if unknown, if a job running in that pane refused the close, or if another
/// view still holds the session. Main-thread hop like `spawn`.
///
/// `by` is the CALLER (`control_session::caller_actor`: the session an
/// edge-scoped connection's token was granted to, `Ctl` for an owner-token
/// connection, which names no session) — it rides the wake so the exit ledger
/// can say who
/// closed the session (`exits` → `reason=ctl-close by=<by>`), the one fact the
/// target's ctx cannot supply: the dispatch resolved `session`/`sid` from the
/// selector, and the caller is somebody else.
/// `close` takes no argument — validate the tail before anything is retired,
/// so a flag or tab index (`close --dry-run`, `close 2`) is refused rather than
/// retiring the session.
pub(crate) fn close_no_arg(rest: &str) -> Result<(), String> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err("ERR usage: close\n".to_string())
    }
}

pub(crate) fn cmd_close(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    sid: &str,
    by: crate::session_store::ExitActor,
) -> String {
    match call_main(proxy, |tx| Wake::CloseSession {
        session,
        by,
        reply: tx,
    }) {
        Ok(Ok(())) => format!("OK closed {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `chrome` -> dump the frontmost window's NATIVE macOS UI: its `NSToolbar` items
/// (each `id=<identifier> label="<label>"`, e.g. the "+" New Tab button) and the
/// app menu bar (`menu "File": New Window, New Tab, ...`). A read-only
/// introspection verb so an AI driving aterm can SEE and verify the native chrome
/// — which `image`/`text` CANNOT capture, as they render only the terminal content
/// view, never the OS toolbar/menu bar.
///
/// MAIN-THREAD HOP (mirrors [`cmd_image`]): AppKit objects (`NSToolbar`/`NSMenu`/
/// `NSWindow`) may ONLY be touched on the main thread, but this runs on a
/// background control thread. So we build a one-shot reply channel, post
/// [`Wake::ReadChrome`] to wake the event loop, and BLOCK on the reply; the main
/// thread reads the chrome (`App::read_native_chrome`) and sends back the text
/// lines. The lines are returned in the SAME multi-line shape as `text`:
/// `OK <n>\n` followed by `<n>` data rows.
///
/// Off macOS the main thread replies with one explanatory line (no native chrome),
/// so the wire shape (`OK 1` + one row) is identical on every platform.
pub(crate) fn cmd_chrome(proxy: &EventLoopProxy<Wake>) -> String {
    let lines = match call_main(proxy, |tx| Wake::ReadChrome { reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    // Same `OK <n>\n` + n-rows framing the `text` verb uses, so the aterm-ctl client
    // prints the rows verbatim (it lists `chrome` among the multi-line verbs).
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// `panes` -> the active-tab split-pane layout: a `layout tab=<i> panes=<n>
/// zoomed=<bool>` header + one
/// `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> focused=<bool>`
/// row per visible pane, in cell coords. `session` = a cross-session target:
/// the layout of the window whose active tab displays that session (the
/// `image` routing rule); `None` = the front window. A pure main-thread read
/// (`Wake::ReadPanes`, the `chrome` round-trip shape); works headless.
pub(crate) fn cmd_panes(proxy: &EventLoopProxy<Wake>, session: Option<u64>) -> String {
    let lines = match call_main(proxy, |tx| Wake::ReadPanes { session, reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod trail_parse_tests {
    use super::{TrailForm, parse_trail_form};

    /// The two forms of one verb must not swallow each other: `status` is a
    /// keyword, not a malformed count, and a count is a count, not an unknown
    /// keyword.
    #[test]
    fn the_status_keyword_and_a_ring_count_are_separate_forms() {
        assert_eq!(parse_trail_form("status"), Ok(TrailForm::Status));
        assert_eq!(parse_trail_form("  status  "), Ok(TrailForm::Status));
        assert_eq!(parse_trail_form(""), Ok(TrailForm::Admissions(None)));
        assert_eq!(parse_trail_form("   "), Ok(TrailForm::Admissions(None)));
        assert_eq!(parse_trail_form("8"), Ok(TrailForm::Admissions(Some(8))));
        assert_eq!(parse_trail_form("0"), Ok(TrailForm::Admissions(Some(0))));
        for bad in ["stats", "Status", "on", "-1", "3 4"] {
            assert!(parse_trail_form(bad).is_err(), "{bad:?} must be refused");
        }
    }
}

#[cfg(test)]
mod fx_parse_tests {
    use super::parse_fx_form;
    use crate::FxCtlOp;
    use aterm_effects::kitty_sing::{CelebrateOn, song_signature};

    /// The `fx` grammar: the bare and `status` forms read; `celebrate` takes
    /// its three keys in any order with the documented defaults (`C`, 2 bars,
    /// the green edge).
    #[test]
    fn fx_celebrate_parses_its_keys_in_any_order_with_defaults() {
        assert_eq!(parse_fx_form(""), Ok(FxCtlOp::Status));
        assert_eq!(parse_fx_form(" status "), Ok(FxCtlOp::Status));
        assert_eq!(
            parse_fx_form("celebrate"),
            Ok(FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 2,
                on: CelebrateOn::Green
            })
        );
        assert_eq!(
            parse_fx_form("celebrate bars=4 on=enter sig=g"),
            Ok(FxCtlOp::Celebrate {
                sig: song_signature('g'),
                bars: 4,
                on: CelebrateOn::Enter
            })
        );
        assert_eq!(
            parse_fx_form("celebrate sig=C bars=2"),
            Ok(FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 2,
                on: CelebrateOn::Green
            })
        );
        // The program's own turn end (OSC 7501 `done`).
        assert_eq!(
            parse_fx_form("celebrate on=done"),
            Ok(FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 2,
                on: CelebrateOn::Done
            })
        );
    }

    /// Refused: an unknown form, an unknown key, a bare word, a multi-character
    /// `sig`, bars outside 1..=4, a bad edge, a repeated key.
    #[test]
    fn fx_refuses_every_malformed_form() {
        for bad in [
            "fire",
            "status now",
            "celebrate loud",
            "celebrate key=C",
            "celebrate sig=CD",
            "celebrate sig=",
            "celebrate bars=0",
            "celebrate bars=5",
            "celebrate bars=two",
            "celebrate on=red",
            "celebrate on=DONE",
            "celebrate on=done on=green",
            "celebrate bars=2 bars=3",
        ] {
            assert!(parse_fx_form(bad).is_err(), "{bad:?} must be refused");
        }
        let usage = parse_fx_form("celebrate on=red").expect_err("refused");
        assert!(usage.contains("on=green|enter|done"), "{usage}");
    }
}

#[cfg(test)]
mod spawn_parse_tests {
    #[cfg(unix)]
    use super::spawn_cwd_refusal;
    use super::{AgentSpec, SpawnForm, parse_spawn_args, split_quoted_tokens};
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};

    /// `spawn cwd=<dir>` naming no folder is refused by name, in both forms,
    /// before anything is spawned (`ERR no such folder <dir>` or `ERR no access
    /// to folder <dir>`); an existing folder and no `cwd=` pass through.
    #[cfg(unix)]
    #[test]
    fn a_spawn_whose_cwd_names_no_folder_is_refused_by_name() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-spawn-cwd")
            .tempdir()
            .expect("scratch dir");
        let here = scratch.path().to_str().expect("utf-8 scratch path");
        let gone = format!("{here}/gone");
        let refused = |rest: &str| spawn_cwd_refusal(&parse_spawn_args(rest).expect("parses"));
        assert_eq!(
            refused(&format!("cwd={gone}")),
            Some(format!("ERR no such folder {gone}\n"))
        );
        assert_eq!(
            refused(&format!("connected=controlled place=tab of=s-a cwd={gone}")),
            Some(format!("ERR no such folder {gone}\n")),
            "the connected form too"
        );
        assert_eq!(
            refused(r#"cwd="""#),
            Some("ERR no such folder \"\"\n".to_string()),
            "an empty folder is named as empty, never read as no cwd="
        );
        assert_eq!(
            refused(&format!("cwd={here} split=v")),
            None,
            "a folder that is there"
        );
        assert_eq!(
            refused("split=h"),
            None,
            "no cwd=: the focused pane's folder"
        );
        // A folder this user may not enter (root enters any folder).
        use std::os::unix::fs::PermissionsExt as _;
        let shut = format!("{here}/shut");
        std::fs::create_dir(&shut).unwrap();
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o600)).unwrap();
        let verdict = refused(&format!("cwd={shut}"));
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
        // SAFETY: getuid has no failure mode and takes no arguments.
        if unsafe { libc::getuid() } != 0 {
            assert_eq!(verdict, Some(format!("ERR no access to folder {shut}\n")));
        }
        assert_eq!(
            refused(&format!("cwd={shut}")),
            None,
            "the same folder, opened"
        );
    }

    /// Both the `<t|f>` letters from the `spawn` summary and the long
    /// `true|false` form parse; malformed `raise=` or `window=` values are
    /// rejected as usage errors rather than falling back to defaults.
    #[test]
    fn the_summarys_raise_letters_parse_like_splits_do() {
        let raise_of = |rest: &str| match parse_spawn_args(rest) {
            Ok(SpawnForm::Plain { raise, .. }) => raise,
            other => panic!("{rest:?}: {other:?}"),
        };
        for (rest, want) in [
            ("raise=t", Some(true)),
            ("raise=true", Some(true)),
            ("raise=f", Some(false)),
            ("raise=false", Some(false)),
            ("split=v raise=t", Some(true)),
            ("cwd=/tmp raise=f", Some(false)),
            ("", None),
        ] {
            assert_eq!(raise_of(rest), want, "{rest:?}");
        }
        assert!(parse_spawn_args("split=v").is_ok());
        for bad in [
            "raise=",
            "raise=yes",
            "raise=T",
            "raise=True",
            "raise=1",
            "raise=tr",
            "raise=maybe",
            "window=",
            "window=abc",
            "window=-1",
            "window=0x1",
            "window=1 window=x",
            "window=1 raise=maybe",
        ] {
            assert!(parse_spawn_args(bad).is_err(), "{bad:?} is usage");
        }
    }

    /// Unquoted `spawn` requests tokenize identically to `split_whitespace`,
    /// including mid-value quotes on POSIX paths.
    #[test]
    fn unquoted_requests_tokenize_exactly_as_whitespace_splitting_did() {
        for rest in [
            "",
            "   ",
            "cwd=/tmp/x",
            "  cwd=/tmp/x  ",
            "cwd=/tmp/x split=v",
            // A `"` inside a value is treated as data when not at a token
            // boundary or immediately after `=`.
            r#"cwd=/tmp/a"b"#,
            r#"cwd=/tmp/a"b"c"#,
            r#"cwd=/tmp/say"hi split=h"#,
        ] {
            let expected: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            assert_eq!(split_quoted_tokens(rest), Some(expected), "{rest:?}");
        }
    }

    /// A WORKER's spawn (`agent=<word> [model=<m>] [autopilot=<land|cruise>]
    /// [from=<sid>] len=<n>`): `agent=` is a worker's word (claude, gemini,
    /// agy, muse), or another agent aterm reads (codex: reserved), `model=` is
    /// `[A-Za-z0-9._-]{1,64}` opening with no `-` (each CLI's parser reads
    /// `--model -x` as a flag; one with a `-` inside it is a model),
    /// `autopilot=` land or cruise once (never off),
    /// `from=` a sid, and they and the frame's `len=` come only with
    /// `agent=`, which never joins the connected form. Everything else is the
    /// usage line.
    #[test]
    fn a_worker_spawn_parses_agent_model_and_its_frame() {
        let agent_of = |rest: &str| match parse_spawn_args(rest) {
            Ok(SpawnForm::Plain { agent, .. }) => agent,
            other => panic!("{rest:?}: {other:?}"),
        };
        assert_eq!(agent_of(""), None);
        assert_eq!(
            agent_of("agent=claude len=12"),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Claude,
                model: None,
                autopilot: None,
                from: None
            })
        );
        assert_eq!(
            agent_of("cwd=/w raise=false agent=claude model=claude-opus-4.1 from=s-ab12 len=3"),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Claude,
                model: Some("claude-opus-4.1".to_string()),
                autopilot: None,
                from: Some(aterm_session::SessionId::new("s-ab12")),
            })
        );
        assert_eq!(
            agent_of("agent=gemini model=gemini-2.5-pro- from=s-ab12 len=3"),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Gemini,
                model: Some("gemini-2.5-pro-".to_string()),
                autopilot: None,
                from: Some(aterm_session::SessionId::new("s-ab12")),
            })
        );
        let longest = "M".repeat(64);
        assert_eq!(
            agent_of(&format!("model={longest} agent=claude len=1 split=v")),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Claude,
                model: Some(longest.clone()),
                autopilot: None,
                from: None,
            })
        );
        // The worker's own word: land or cruise, once.
        for (word, autopilot) in [
            ("land", aterm_agent::supervise::Autopilot::Land),
            ("cruise", aterm_agent::supervise::Autopilot::Cruise),
        ] {
            assert_eq!(
                agent_of(&format!("agent=claude autopilot={word} len=1")),
                Some(AgentSpec::Worker {
                    agent: aterm_phase::Program::Claude,
                    model: None,
                    autopilot: Some(autopilot),
                    from: None,
                })
            );
        }
        for program in [
            aterm_phase::Program::Agy,
            aterm_phase::Program::Muse,
            aterm_phase::Program::Gemini,
        ] {
            assert_eq!(
                agent_of(&format!(
                    "agent={} model=m-1 autopilot=cruise from=s-ab12 len=1",
                    program.name()
                )),
                Some(AgentSpec::Worker {
                    agent: program,
                    model: Some("m-1".to_string()),
                    autopilot: Some(aterm_agent::supervise::Autopilot::Cruise),
                    from: Some(aterm_session::SessionId::new("s-ab12")),
                }),
                "a worker of {program:?}"
            );
        }
        assert_eq!(
            agent_of("agent=codex len=1"),
            Some(AgentSpec::Reserved(aterm_phase::Program::Codex)),
            "CONTROL: Codex has no worker row"
        );
        let too_long = format!("agent=claude model={longest}x len=1");
        for bad in [
            "agent=",
            "agent=generic",
            "agent=Claude",
            "model=opus",
            "from=s-ab12",
            "len=4",
            "agent=claude model=",
            "agent=claude from=",
            "agent=claude from=ctl",
            "agent=claude from=s-xyz",
            "agent=claude model=a/b",
            "agent=claude model=a$b",
            "agent=claude model=a\u{e9}",
            "agent=claude autopilot=off len=1",
            "agent=claude autopilot= len=1",
            "agent=claude autopilot=Land len=1",
            "agent=claude autopilot=land autopilot=cruise len=1",
            "autopilot=land",
            too_long.as_str(),
            "agent=claude connected=controlled place=tab of=s-a",
            "agent=gemini connected=controlled place=tab of=s-a",
            "agent=Gemini len=1",
            "agent=muse model=a/b len=1",
            "agent=gemini model=--yolo len=1",
            "agent=muse model=-x len=1",
            "agent=claude model=- len=1",
            "connected=controlled place=tab of=s-a len=3",
        ] {
            assert_eq!(parse_spawn_args(bad), Err(()), "{bad}");
        }
        assert!(super::SPAWN_USAGE.contains(
            "agent=<claude|gemini|agy|muse> [model=<m>] [autopilot=<land|cruise>] [from=<sid>] \
             len=<n>"
        ));
    }

    #[test]
    fn plain_spawn_parses_with_and_without_cwd() {
        assert_eq!(
            parse_spawn_args(""),
            Ok(SpawnForm::Plain {
                window: None,
                raise: None,
                cwd: None,
                split: None,
                theme: None,
                agent: None,
            })
        );
        assert_eq!(
            parse_spawn_args("cwd=/tmp/x"),
            Ok(SpawnForm::Plain {
                window: None,
                raise: None,
                cwd: Some("/tmp/x".to_string()),
                split: None,
                theme: None,
                agent: None,
            })
        );
        assert_eq!(
            parse_spawn_args("cwd=/tmp/x split=v"),
            Ok(SpawnForm::Plain {
                window: None,
                raise: None,
                cwd: Some("/tmp/x".to_string()),
                split: Some(crate::pane::SplitDir::Vertical),
                theme: None,
                agent: None,
            })
        );
        // The quoted `cwd=` reaches the plain form through the same tokenizer.
        assert_eq!(
            parse_spawn_args(r#"cwd="C:\Program Files\Git""#),
            Ok(SpawnForm::Plain {
                window: None,
                raise: None,
                cwd: Some(r"C:\Program Files\Git".to_string()),
                split: None,
                theme: None,
                agent: None,
            })
        );
        assert_eq!(parse_spawn_args("bogus"), Err(()));
        assert_eq!(parse_spawn_args("split=diagonal"), Err(()));
        assert_eq!(parse_spawn_args("identity=worker"), Err(()));
        assert_eq!(parse_spawn_args("identity=-"), Err(()));
        // An unterminated quote is malformed for the verb, not "to end of line".
        assert_eq!(parse_spawn_args(r#"cwd="C:\Program Files"#), Err(()));
    }

    #[test]
    fn connected_spawn_requires_the_full_explicit_form() {
        // The complete form parses, cwd optional.
        let full = parse_spawn_args("connected=controlled place=tab of=s-abc cwd=/w");
        assert_eq!(
            full,
            Ok(SpawnForm::Connected {
                kind: ConnectedSpawnKind::Controlled,
                place: ConnectedSpawnPlace::Tab,
                origin: aterm_session::SessionId::new("s-abc"),
                cwd: Some("/w".to_string()),
                theme: None,
            })
        );
        assert!(matches!(
            parse_spawn_args("connected=controller place=window of=s-abc"),
            Ok(SpawnForm::Connected {
                kind: ConnectedSpawnKind::Controller,
                place: ConnectedSpawnPlace::Window,
                ..
            })
        ));
        // `of=` and `place=` are mandatory when `connected=` is present.
        assert_eq!(parse_spawn_args("connected=controlled place=tab"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled of=s-abc"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled"), Err(()));
        // A stray place=/of= without connected= never means a plain spawn.
        assert_eq!(parse_spawn_args("place=tab"), Err(()));
        assert_eq!(parse_spawn_args("of=s-abc"), Err(()));
        // Unknown values fail closed.
        assert_eq!(
            parse_spawn_args("connected=owner place=tab of=s-abc"),
            Err(())
        );
        assert_eq!(
            parse_spawn_args("connected=controlled place=pane of=s-abc"),
            Err(())
        );
        assert_eq!(
            parse_spawn_args("connected=controlled place=tab of="),
            Err(())
        );
        // `place=`/`of=` place a connected newborn: an aim, a raise verdict
        // or a split beside them is refused rather than one silently winning.
        for knob in [
            "window=1",
            "raise=false",
            "split=v",
            "identity=worker",
            "identity=-",
        ] {
            assert_eq!(
                parse_spawn_args(&format!("connected=controlled place=tab of=s-abc {knob}")),
                Err(()),
                "{knob}"
            );
        }
    }

    /// `window=` (the explicit spelling) beats `@<sid>` (the implied one); the
    /// selector alone aims at its host; neither is the front window.
    #[test]
    fn spawn_aim_prefers_the_explicit_window_over_the_selector() {
        use super::SpawnAim;
        assert_eq!(SpawnAim::from_request(None, None), SpawnAim::Front);
        assert_eq!(SpawnAim::from_request(None, Some(7)), SpawnAim::Session(7));
        assert_eq!(SpawnAim::from_request(Some(2), None), SpawnAim::Window(2));
        assert_eq!(
            SpawnAim::from_request(Some(2), Some(7)),
            SpawnAim::Window(2)
        );
        assert!(!SpawnAim::Front.is_aimed());
        assert!(SpawnAim::Window(0).is_aimed());
        assert!(SpawnAim::Session(0).is_aimed());
    }

    /// The raise policy, all four cells: an explicit verdict wins either way;
    /// with none, an un-aimed spawn raises (`aterm new-tab` attach contract)
    /// and an aimed one does not.
    #[test]
    fn raise_after_spawn_all_four_cells() {
        use super::raise_after_spawn;
        assert!(
            raise_after_spawn(false, None),
            "un-aimed, no verdict: raise (attach)"
        );
        assert!(
            !raise_after_spawn(true, None),
            "aimed, no verdict: do NOT raise"
        );
        assert!(
            raise_after_spawn(true, Some(true)),
            "aimed, raise=true: insist"
        );
        assert!(
            !raise_after_spawn(false, Some(false)),
            "un-aimed, raise=false: hold"
        );
    }
}

#[cfg(test)]
mod close_arg_tests {
    use super::close_no_arg;

    /// The bare verb (with or without whitespace) is the only accepted form,
    /// and every guess a driver might make — a flag, a tab index, a repeated
    /// selector — is a usage error rather than a retired session. The message
    /// is the exact wire line, so the catalog's `ERR usage: close` promise and
    /// the code cannot drift apart.
    #[test]
    fn close_takes_no_argument_and_a_guess_never_retires_a_session() {
        assert_eq!(close_no_arg(""), Ok(()));
        assert_eq!(close_no_arg("   "), Ok(()));
        for bad in [
            "--dry-run",
            "2",
            "now",
            "force",
            "@s-0123456789abcdef0123",
            "trim",
        ] {
            assert_eq!(
                close_no_arg(bad),
                Err("ERR usage: close\n".to_string()),
                "`close {bad}` must be refused, not honoured"
            );
        }
    }
}

/// The wire `tab close` guard: a busy last-tab `tab close` is refused in the
/// `close` verb's words, a busy tab among several is refused as `close` refuses
/// its pane, and an idle one proceeds. Driven through `tab_cmd_applied`, the
/// part of `tab_cmd_bracketed` that runs before the escalation, because the
/// bracket itself needs the `ActiveEventLoop` a unit test cannot construct.
/// Every guarded case is windowed (`headless = false`): the headless
/// short-circuit never confirms.
#[cfg(test)]
mod wire_tab_close_guard_tests {
    use crate::app_window::{CloseConfirm, WIRE_CLOSE_REFUSED, WIRE_TAB_CLOSE_REFUSED};
    use crate::session_store::{ExitActor, ExitReason, RosterChange};
    use crate::{App, TabAction, WindowId};

    use crate::feed_session_for_test as feed;

    /// The `Start-Sleep 60` shape: a command the shell reports running
    /// (133;C, no 133;D) with no child process — the stub session's PTY says
    /// nothing at all (`master`/`pid` = -1), so ONLY the block state can arm
    /// the guard here.
    fn start_in_shell_job(app: &App, session: u64) {
        feed(
            app,
            session,
            b"\x1b]133;A\x07PS> \x1b]133;B\x07Start-Sleep 60\n\x1b]133;C\x07",
        );
    }

    fn finish_job(app: &App, session: u64) {
        feed(app, session, b"\x1b]133;D;0\x07");
    }

    /// `tab close` from an owner-token client, as the bracket applies it.
    fn wire_tab_close(app: &mut App, which: Option<usize>) -> Result<(usize, usize), String> {
        wire_tab_close_by(app, which, ExitActor::Ctl)
    }

    /// `tab close` from the caller `by`: the bracket's policy and ledger scope
    /// around the action, then the policy restored as the bracket restores it
    /// after the escalation.
    fn wire_tab_close_by(
        app: &mut App,
        which: Option<usize>,
        by: ExitActor,
    ) -> Result<(usize, usize), String> {
        let state = app.tab_cmd_applied(by, |app| app.apply_tab_cmd(TabAction::Close(which)));
        app.close_confirm = CloseConfirm::Interactive;
        state
    }

    /// The reason and `by=` of the newest `Exited` journal row.
    fn last_exit(app: &App) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let row = g
            .roster_since(0)
            .rfind(|r| r.change == RosterChange::Exited)
            .expect("an exit row after the close");
        (row.reason, row.actor.as_wire().to_string())
    }

    /// `tab close` names its caller on the exit ledger, the way `close` does:
    /// `by=ctl` for an owner-token client, the edge caller's sid for an edge,
    /// for both a non-last tab retired inside the verb's turn and a last tab
    /// whose window teardown runs on a later turn.
    #[test]
    fn a_wire_tab_close_journals_its_caller() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));

        wire_tab_close(&mut app, Some(1)).expect("a non-last tab closes");
        assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

        // The last tab only flags its window; the attribution must survive the
        // scope to the deferred teardown (`escalate_pending_close` ->
        // `close_window_logical`, driven directly here).
        wire_tab_close_by(&mut app, None, ExitActor::Sid("s-edge-caller".to_string()))
            .expect("an idle last-tab close proceeds");
        assert!(app.windows[&wid].pending_close);
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        assert_eq!(
            last_exit(&app),
            (ExitReason::CtlClose, "s-edge-caller".to_string())
        );
    }

    /// `invoke CloseTab` names its caller as well: its arm lands in
    /// `close_active_tab`, Cmd-W's own path, inside [`App::invoke_attributed`].
    #[test]
    fn an_invoked_close_tab_journals_its_caller_not_a_human() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));

        // Negative control: the same close with no wire scope is the human's.
        assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
        assert_eq!(last_exit(&app), (ExitReason::UiClose, "human".to_string()));

        let invoked = app.invoke_attributed(ExitActor::Ctl, |app| {
            assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
            Ok("invoked CloseTab".to_string())
        });
        assert_eq!(invoked, Ok("invoked CloseTab".to_string()));
        assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

        // The last tab only flags its window; the stash carries the caller to
        // the teardown that runs after the scope has closed.
        let _ = app.invoke_attributed(ExitActor::Sid("s-edge-caller".to_string()), |app| {
            assert_eq!(app.close_active_tab(), Some(wid), "the last tab closes");
            Ok("invoked CloseTab".to_string())
        });
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        assert_eq!(
            last_exit(&app),
            (ExitReason::CtlClose, "s-edge-caller".to_string())
        );
    }

    /// `tab close` of a last tab running an in-shell job is refused in the
    /// `close` verb's words; the tab and session survive, and the close
    /// proceeds once the job finishes.
    #[test]
    fn a_busy_last_tab_tab_close_is_refused_and_proceeds_once_idle() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        start_in_shell_job(&app, 0);

        assert_eq!(
            wire_tab_close(&mut app, None),
            Err(WIRE_CLOSE_REFUSED.to_string()),
            "a running job refuses the last-tab close on the wire, in `close`'s words"
        );
        assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab is still there");
        assert!(
            !app.windows[&wid].pending_close,
            "nothing was flagged for teardown"
        );
        assert!(app.pool.get(0).is_some(), "the session was not retired");
        assert!(
            app.pending_action_refusal.is_none(),
            "the refusal was consumed by the reply, not left for the next verb"
        );

        finish_job(&app, 0);
        wire_tab_close(&mut app, None).expect("an idle last-tab close proceeds");
        assert!(
            app.windows[&wid].pending_close,
            "the idle close flagged the window for the deferred teardown"
        );
    }

    /// A busy tab among several is refused too, as `close` refuses its pane;
    /// nothing moves on the refusal, and the close proceeds once the job ends.
    #[test]
    fn a_busy_non_last_tab_close_is_refused_as_close_refuses_its_pane() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        assert_eq!(app.windows[&wid].tab_set.len(), 2);
        start_in_shell_job(&app, 0);

        assert_eq!(
            wire_tab_close(&mut app, Some(0)),
            Err(WIRE_TAB_CLOSE_REFUSED.to_string()),
            "a running job refuses the close of its tab, last or not"
        );
        assert_eq!(app.windows[&wid].tab_set.len(), 2, "both tabs stand");
        assert!(app.pool.get(0).is_some(), "the session was not retired");
        assert!(!app.windows[&wid].pending_close);
        assert!(
            app.pending_action_refusal.is_none(),
            "the refusal was consumed by the reply, not left for the next verb"
        );

        // The sibling verb refuses the same pane: the two spellings agree.
        app.close_confirm = CloseConfirm::WireRefuseBusy;
        let close_verb = app.close_session_by_id(0);
        app.close_confirm = CloseConfirm::Interactive;
        assert_eq!(
            close_verb,
            Err(crate::app_tabs::WIRE_PANE_CLOSE_REFUSED.to_string())
        );

        finish_job(&app, 0);
        let state = wire_tab_close(&mut app, Some(0)).expect("an idle non-last tab closes");
        assert_eq!(state.1, 1, "the tab is gone once its job ended");
        assert!(app.pool.get(0).is_none(), "and its session with it");
        assert!(!app.windows[&wid].pending_close, "the window stays open");
    }

    /// The guard reads the CLOSED tab's panes and nothing else: a job running
    /// in a sibling tab never refuses the close of an idle one, and a busy
    /// session another view still shows (Cmd-Shift-O) is not hung up by
    /// closing this tab, so it does not refuse it either.
    #[test]
    fn a_non_last_tab_close_reads_only_the_jobs_it_would_hang_up() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));
        start_in_shell_job(&app, 0);

        let state = wire_tab_close(&mut app, Some(2)).expect("an idle tab beside a busy one");
        assert_eq!(state.1, 2, "the idle tab closed");
        assert!(app.pool.get(0).is_some(), "the busy sibling's job runs on");

        start_in_shell_job(&app, 1);
        app.pool.attach(1); // another view now shows session 1
        let state = wire_tab_close(&mut app, Some(1)).expect("a co-viewed busy tab closes");
        assert_eq!(state.1, 1, "the co-viewed tab closed");
        assert!(
            app.pool.get(1).is_some(),
            "its session lives on in the other view"
        );
    }

    /// A `--headless` instance never confirms and never refuses, as `close`
    /// says: the busy non-last tab closes there.
    #[test]
    fn a_headless_busy_non_last_tab_close_proceeds() {
        let mut app = App::headless_for_test();
        assert!(app.headless);
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        start_in_shell_job(&app, 0);

        let state = wire_tab_close(&mut app, Some(0)).expect("headless never refuses");
        assert_eq!(state.1, 1);
    }
}

#[cfg(test)]
mod spawn_aim_app_tests {
    use super::SpawnAim;
    use crate::{App, TabAction, WindowId};

    /// A headless App with two logical windows — the smallest fixture on which
    /// "the aimed window is not the front one" is observable. Installing a
    /// second window makes IT the front one (`install_window_state` focuses
    /// what it installs), so window 1 is the front (one tab, session 1) and
    /// window 0 becomes the BACKGROUND window, given a second tab: it hosts
    /// sessions 0 and 2 with tab 1 (session 2) active.
    fn two_windows() -> App {
        let mut app = App::headless_for_test();
        let front = app.insert_logical_window(crate::stub_session(1), 24, 80);
        assert_eq!(front, WindowId(1));
        assert_eq!(app.frontmost_window, Some(front));
        let back = WindowId(0);
        app.push_stub_tab(back, crate::stub_session(2));
        assert_eq!(app.windows[&back].tab_set.len(), 2);
        assert_eq!(app.windows[&back].tab_set.active_index(), Some(1));
        assert_eq!(
            app.frontmost_window,
            Some(front),
            "a background tab is not a focus change"
        );
        app
    }

    /// `window=<id>` names the table's ids (the ones `inspect app/v1 tabs`
    /// prints), an unknown id is refused by name, and `@<sid>` resolves to the
    /// window whose pane trees hold the session — a background window included,
    /// in both windowed and `--headless` modes.
    #[test]
    fn a_named_or_hosting_window_resolves_and_an_unknown_one_is_named_back() {
        for headless in [true, false] {
            let mut app = two_windows();
            app.headless = headless;
            assert_eq!(app.frontmost_window, Some(WindowId(1)));
            assert_eq!(app.aimed_window(SpawnAim::Front), Ok(None));
            assert_eq!(app.aimed_window(SpawnAim::Window(0)), Ok(Some(WindowId(0))));
            assert_eq!(app.aimed_window(SpawnAim::Window(1)), Ok(Some(WindowId(1))));
            assert_eq!(
                app.aimed_window(SpawnAim::Window(7)),
                Err("no such window 7".to_string()),
                "headless={headless}"
            );
            assert_eq!(
                app.aimed_window(SpawnAim::Session(0)),
                Ok(Some(WindowId(0)))
            );
            assert_eq!(
                app.aimed_window(SpawnAim::Session(1)),
                Ok(Some(WindowId(1)))
            );
            assert_eq!(
                app.aimed_window(SpawnAim::Session(2)),
                Ok(Some(WindowId(0))),
                "headless={headless}"
            );
            assert_eq!(
                app.aimed_window(SpawnAim::Session(99)),
                Err("no window hosts the target session".to_string())
            );
        }
    }

    /// The aimed tab drive walks the BACKGROUND window's tabs and leaves the
    /// front window untouched — the human's keyboard focus and active tab do
    /// not move when an agent cycles its own window.
    #[test]
    fn aimed_tab_commands_drive_the_hosting_window_not_the_front_one() {
        let mut app = two_windows();
        app.headless = false;
        let (front, back) = (WindowId(1), WindowId(0));
        // next wraps 1 -> 0; prev wraps 0 -> 1; a bare index selects.
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Next), Ok((0, 2)));
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Prev), Ok((1, 2)));
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Select(0)), Ok((0, 2)));
        assert_eq!(app.windows[&back].tab_set.active_index(), Some(0));
        // The front window never moved: still one tab, still index 0, still front.
        assert_eq!(app.windows[&front].tab_set.len(), 1);
        assert_eq!(app.windows[&front].tab_set.active_index(), Some(0));
        assert_eq!(app.frontmost_window, Some(front));
        // The front-window form is the SAME entry on the front window.
        assert_eq!(app.apply_tab_cmd(TabAction::Next), Ok((0, 1)));
        // An unknown window reports the empty state instead of guessing.
        assert_eq!(
            app.apply_tab_cmd_in(WindowId(9), TabAction::Next),
            Ok((0, 0))
        );
        assert!(app.structural_invariants_ok());
    }

    /// `@<sid> tab close` closes in the HOSTING window: window 1 drops to one
    /// tab and window 0 keeps its only tab (the front form would have flagged
    /// the front window's last-tab close instead).
    #[test]
    fn aimed_tab_close_closes_in_the_hosting_window() {
        let mut app = two_windows();
        app.headless = false;
        let (front, back) = (WindowId(1), WindowId(0));
        assert_eq!(
            app.apply_tab_cmd_in(back, TabAction::Close(Some(1))),
            Ok((0, 1))
        );
        assert_eq!(app.windows[&back].tab_set.len(), 1);
        assert!(!app.windows[&back].pending_close);
        assert_eq!(app.windows[&front].tab_set.len(), 1);
        assert!(!app.windows[&front].pending_close);
        assert!(app.structural_invariants_ok());
    }
}

/// A worker's start on the main thread, after its spawn: the newborn made a
/// worker or closed again ([`crate::App::ready_worker`]), and its launcher
/// started only once its reply is taken ([`crate::App::answer_spawn`]). The
/// newborns are stub tabs (no PTY), so what a spawn would have created is
/// here without one.
#[cfg(test)]
mod worker_start_app_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use aterm_agent::harness::upgrade::Dialect;
    use aterm_session::SessionId;
    #[cfg(unix)]
    use aterm_session::sink::SinkWriter;

    #[cfg(unix)]
    use super::Spawned;
    use super::WorkerStart;
    use crate::task_launch::{TaskOrder, TaskSeed, task_text};
    use crate::task_record::TaskState;
    use crate::{App, WindowId};

    /// What a worker's start needs, its starter `ctl`.
    fn worker() -> WorkerStart {
        WorkerStart {
            order: TaskOrder {
                task: b"the task".to_vec(),
                agent: aterm_phase::Program::Claude,
                model: None,
                caller: None,
                trusted_cwd: None,
                fact_line: true,
                autopilot: crate::task_launch::WORKER_AUTOPILOT,
            },
            from: crate::task_record::FROM_CTL.to_string(),
            caller: None,
            shell: PathBuf::from("/bin/zsh"),
            dialect: Dialect::Zsh,
        }
    }

    /// The seed of `sid`'s task file, written where the state home says.
    fn seeded(sid: &str) -> (TaskSeed, PathBuf) {
        let seed = TaskSeed::new(task_text(
            b"the task",
            Some(crate::task_record::FROM_CTL),
            crate::task_launch::WORKER_AUTOPILOT,
            aterm_phase::Program::Claude,
        ));
        let path = seed.write_for(&SessionId::new(sid)).expect("written");
        (seed, path)
    }

    fn sid_of(app: &App, local: u64) -> String {
        app.store
            .read()
            .unwrap()
            .by_local(local)
            .map(|h| h.sid.as_str().to_string())
            .expect("registered")
    }

    fn ctx_of(app: &App, local: u64) -> Arc<crate::SessionCtx> {
        app.pool.get(local).expect("live session").ctx.clone()
    }

    /// A worker that cannot start leaves nothing behind: one that did not
    /// register is refused and its task file removed; one whose `working` mark
    /// a committing update's fence refuses is closed again too. With no fence,
    /// the worker starts as `working` with its tab and task file standing until
    /// its session closes.
    #[test]
    fn a_worker_that_cannot_start_is_closed_again_and_leaves_no_task_file() {
        let state = aterm_tempfile::Builder::new()
            .prefix("aterm-worker-start")
            .tempdir()
            .expect("scratch state home");
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.push_stub_tab(wid, crate::stub_session(1));
            app.push_stub_tab(wid, crate::stub_session(2));

            let ghost = "s-0123dead";
            let (seed, path) = seeded(ghost);
            assert_eq!(
                app.ready_worker(ghost, worker(), seed).err().as_deref(),
                Some("the worker did not register")
            );
            assert!(!path.exists(), "the task file of a worker that is not");

            let busy = sid_of(&app, 1);
            let fenced = ctx_of(&app, 1);
            let _ = fenced.fabric.fence_hold(false);
            let (seed, path) = seeded(&busy);
            assert_eq!(
                app.ready_worker(&busy, worker(), seed).err().as_deref(),
                Some("busy update-committing")
            );
            fenced.fabric.lift_hold_fence();
            assert!(app.pool.get(1).is_none(), "the newborn is closed again");
            assert!(!path.exists(), "and its task file removed");

            let ready = sid_of(&app, 2);
            let (seed, path) = seeded(&ready);
            let pending = app.ready_worker(&ready, worker(), seed).expect("a worker");
            assert!(app.pool.get(2).is_some(), "the worker stands");
            assert!(path.exists(), "with its task file");
            let record = ctx_of(&app, 2)
                .timeline
                .lock()
                .unwrap()
                .task()
                .cloned()
                .expect("a record");
            assert_eq!(
                (record.state, record.from.as_str()),
                (TaskState::Working, "ctl")
            );
            drop(pending);
            app.push_stub_tab(wid, crate::stub_session(3));
            app.close_confirm = crate::app_window::CloseConfirm::Programmatic;
            assert!(app.close_session_by_id(2).is_ok(), "it closes");
            assert!(!path.exists(), "a closed worker's task file goes with it");

            // A Gemini worker's sidecar exists before registration, never
            // first created by a launcher after the close below. Interpose
            // that close after its observed prompt and before final launch
            // admission, with no thread, timer or actual program.
            let sid = sid_of(&app, 3);
            let mut requested = worker();
            requested.order.agent = aterm_phase::Program::Gemini;
            requested.order.trusted_cwd = Some(state.path().to_string_lossy().into_owned());
            let seed = requested.seed();
            let task = seed.write_for(&SessionId::new(sid.as_str())).unwrap();
            let pending = app.ready_worker(&sid, requested, seed).unwrap();
            let trust = pending.launch.trust_file.clone().expect("owned sidecar");
            assert!(task.exists() && trust.exists(), "both precede the launcher");
            let prompt = {
                let mut terminal = crate::term_lock(&pending.launch.term);
                terminal.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
                crate::task_launch::prompt_fence(&terminal, pending.launch.human).unwrap()
            };
            assert!(app.close_session_by_id(3).is_ok());
            assert!(
                crate::task_launch::type_launch_line(
                    &app.store,
                    &pending.launch.ctx.self_id,
                    pending.launch.ctx.nonce,
                    prompt,
                    "worker",
                )
                .is_err(),
                "close wins final admission"
            );
            drop(pending);
            assert!(!task.exists() && !trust.exists(), "neither file reappears");
        });
    }

    /// `spawn agent=claude autopilot=cruise` parsed, ordered and started writes
    /// the newborn's `autopilot=cruise` and seeds its task file with the Cruise
    /// fact line; with no word it is a Land worker with the Land fact line.
    /// Every harness's worker is started so, its agent named on its task
    /// record for the startup deadline's reader (`TaskRecord::agent`).
    /// NEGATIVE CONTROL: a plain session names no worker agent.
    #[test]
    fn a_spawns_autopilot_word_is_its_workers_word_and_fact_line() {
        use aterm_agent::supervise::Autopilot;
        let state = aterm_tempfile::Builder::new()
            .prefix("aterm-worker-word")
            .tempdir()
            .expect("scratch state home");
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            for (local, args, word) in [
                (1, "agent=claude autopilot=cruise len=8", Autopilot::Cruise),
                (2, "agent=claude len=8", Autopilot::Land),
                (3, "agent=gemini autopilot=cruise len=8", Autopilot::Cruise),
                (4, "agent=agy len=8", Autopilot::Land),
                (5, "agent=muse len=8", Autopilot::Land),
            ] {
                app.push_stub_tab(wid, crate::stub_session(local));
                let Ok(super::SpawnForm::Plain {
                    agent:
                        Some(super::AgentSpec::Worker {
                            agent,
                            model,
                            autopilot,
                            from,
                        }),
                    cwd,
                    ..
                }) = super::parse_spawn_args(args)
                else {
                    panic!("{args}: a worker's spawn");
                };
                let order = super::worker_order(
                    Some(b"the task".to_vec()),
                    agent,
                    model,
                    autopilot,
                    from,
                    cwd.as_deref(),
                )
                .expect("an order");
                let start = WorkerStart { order, ..worker() };
                let sid = sid_of(&app, local);
                let seed = start.seed();
                let path = seed.write_for(&SessionId::new(&sid)).expect("written");
                assert_eq!(
                    std::fs::read(&path).expect("the task file"),
                    task_text(b"the task", Some(crate::task_record::FROM_CTL), word, agent),
                    "{args}: the fact line under {word:?}"
                );
                let pending = app.ready_worker(&sid, start, seed).expect("a worker");
                assert_eq!(pending.launch.agent, agent, "{args}: the line's agent");
                assert_eq!(
                    ctx_of(&app, local).meta.lock().unwrap().autopilot_word(),
                    Some(word),
                    "{args}: the newborn's own word"
                );
                let record_agent = |local| {
                    ctx_of(&app, local)
                        .timeline
                        .lock()
                        .unwrap()
                        .task()
                        .and_then(|t| t.agent)
                };
                assert_eq!(
                    record_agent(local),
                    Some(agent),
                    "{args}: the agent its startup is read by"
                );
            }
            app.push_stub_tab(wid, crate::stub_session(6));
            assert_eq!(
                ctx_of(&app, 6)
                    .timeline
                    .lock()
                    .unwrap()
                    .task()
                    .and_then(|t| t.agent),
                None,
                "CONTROL: a plain session"
            );
        });
    }

    /// A pipe a stub session's sink writes into, its read end non-blocking.
    #[cfg(unix)]
    fn pipe() -> [i32; 2] {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        pipe
    }

    /// What the pipe holds now.
    #[cfg(unix)]
    fn drained(fd: i32) -> Vec<u8> {
        let mut bytes = [0u8; 4096];
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        usize::try_from(read).map_or_else(|_| Vec::new(), |n| bytes[..n].to_vec())
    }

    /// What worker `local`'s launcher said it did with its line, waited for
    /// at most a minute (a hang detector: the launcher must say).
    #[cfg(unix)]
    fn launch_note(app: &App, local: u64) -> String {
        let ctx = ctx_of(app, local);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let said = ctx
                .timeline
                .lock()
                .unwrap()
                .worker_launch()
                .map(|(_, words)| words.clone());
            if let Some(words) = said {
                return words;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the launcher said nothing within a minute"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A worker's launcher starts only once its reply is taken: a worker whose
    /// `OK <sid>` nobody took (the lane's end dropped) is closed again, its
    /// task file removed, and nothing is typed into it. With its reply taken,
    /// its launcher runs to its prompt and on to the line's main-thread
    /// admission, keeping its tab and file.
    #[cfg(unix)]
    #[test]
    fn a_workers_launcher_starts_only_once_its_reply_is_taken() {
        let state = aterm_tempfile::Builder::new()
            .prefix("aterm-worker-reply")
            .tempdir()
            .expect("scratch state home");
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let (dropped, taken) = (pipe(), pipe());
            for (local, pipe) in [(1, dropped), (2, taken)] {
                app.push_stub_tab(
                    wid,
                    crate::stub_session_with_sink(local, Arc::new(SinkWriter::new(pipe[1]))),
                );
                // The shell's first prompt, its line editor reading.
                crate::feed_session_for_test(&app, local, b"\x1b]133;A\x07$ \x1b]133;B\x07");
                assert!(matches!(
                    app.pool
                        .get(local)
                        .unwrap()
                        .term
                        .lock()
                        .unwrap()
                        .all_blocks()
                        .last()
                        .map(|b| b.state),
                    Some(aterm_core::terminal::BlockState::EnteringCommand)
                ));
            }

            let sid = sid_of(&app, 1);
            let dropped_ctx = ctx_of(&app, 1);
            let (seed, path) = seeded(&sid);
            let pending = app.ready_worker(&sid, worker(), seed).expect("a worker");
            let (tx, rx) = std::sync::mpsc::channel();
            drop(rx);
            app.answer_spawn(
                Ok(Spawned {
                    sid,
                    worker: Some(pending),
                }),
                &tx,
            );
            assert!(
                app.pool.get(1).is_none(),
                "an unowned worker is closed again"
            );
            assert!(!path.exists(), "and its task file removed");
            assert_eq!(drained(dropped[0]), b"", "nothing was typed into it");
            assert!(
                dropped_ctx
                    .timeline
                    .lock()
                    .unwrap()
                    .worker_launch()
                    .is_none(),
                "and no launcher started for it"
            );

            let sid = sid_of(&app, 2);
            let (seed, path) = seeded(&sid);
            let pending = app.ready_worker(&sid, worker(), seed).expect("a worker");
            let (tx, rx) = std::sync::mpsc::channel();
            app.answer_spawn(
                Ok(Spawned {
                    sid: sid.clone(),
                    worker: Some(pending),
                }),
                &tx,
            );
            assert_eq!(rx.recv().expect("answered"), Ok(sid));
            // The launcher ran: it says what became of its line (here, that no
            // event loop admitted it).
            let said = launch_note(&app, 2);
            assert_eq!(drained(taken[0]), b"", "nothing typed: {said}");
            assert!(app.pool.get(2).is_some(), "the worker stands");
            assert!(path.exists(), "with its task file");
            drop(app);
            for fd in dropped.into_iter().chain(taken) {
                unsafe { libc::close(fd) };
            }
        });
    }
}

#[cfg(test)]
mod video_parse_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn settings_section_parser_accepts_every_visible_label_and_stable_path() {
        for route in crate::native_settings::SettingsRoute::ALL {
            assert_eq!(
                parse_settings_section_route(route.label()),
                Some(route),
                "visible label {:?}",
                route.label()
            );
            assert_eq!(
                parse_settings_section_route(route.path()),
                Some(route),
                "stable path {:?}",
                route.path()
            );
        }
    }

    #[test]
    fn settings_field_wire_reply_preserves_success_and_failure_framing() {
        assert_eq!(
            settings_field_wire_reply(Ok("saved: copy_on_select = true".to_string())),
            "OK saved: copy_on_select = true\n"
        );
        assert_eq!(
            settings_field_wire_reply(Err(
                "publication unverified for copy_on_select; reload aterm.toml".to_string()
            )),
            "ERR publication unverified for copy_on_select; reload aterm.toml\n",
            "a non-success completion must never acquire an OK prefix"
        );
    }

    fn native_image_metadata() -> crate::control::ImageFrameMetadata {
        crate::control::ImageFrameMetadata {
            frame_kind: "native",
            phase: "presented",
            window: 3,
            view: Some(42),
            generation: Some(7),
            config_revision: Some(11),
            update_revision: Some(13),
            document_seq: Some(17),
            presentation_revision: Some(19),
            paint_revision: Some(21),
            capture_serial: 23,
            width: 800,
            height: 600,
            pixel_fingerprint: 0x1111_2222,
            compiled_fingerprint: Some(0x1234_abcd),
            raster_fingerprint: 0x3333_4444,
            raster_model_fingerprint: 0x5555_6666,
            raster_geometry: 0x7777_8888,
            overlay_fingerprint: 0x9999_aaaa,
            theme_fingerprint: 0xbbbb_cccc,
            leaves: vec![crate::control::ImageLeafFrameMetadata {
                kind: "native",
                view: 42,
                session: None,
                focused: true,
                width: 800,
                height: 600,
                snapshot_seq: None,
                instance: Some(5),
                generation: Some(7),
                geometry: Some(9),
                config_revision: Some(11),
                update_revision: Some(13),
                document_seq: Some(17),
                presentation_revision: Some(19),
                paint_revision: Some(21),
                compiled_fingerprint: Some(0x1234_abcd),
                raster_fingerprint: Some(0x3333_4444),
            }],
        }
    }

    /// The `--bytes` reply is one allocation: its up-front reservation holds
    /// the header, the base64 row and the newline, so the base64 is appended in
    /// place, never built in a buffer of its own and copied.
    #[test]
    fn image_reply_base64_appends_without_a_second_buffer() {
        let metadata = image_metadata_fields(None);
        for len in 0..=257 {
            let png = (0..len)
                .map(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
                .collect::<Vec<_>>();
            let encoded = aterm_codec::base64::encode(&png).expect("bounded test input");
            for want_metadata in [false, true] {
                let reply = image_bytes_reply(7, 5, &png, want_metadata, None)
                    .expect("a short PNG fits a byte reply");
                let (head, reserved) = if want_metadata {
                    (
                        format!("OK 2\nimage-meta {metadata}\n"),
                        encoded.len()
                            + IMAGE_BYTE_ROW_OVERHEAD
                            + metadata.len()
                            + "image-meta \n".len(),
                    )
                } else {
                    (
                        "OK 1\n".to_string(),
                        encoded.len() + IMAGE_BYTE_ROW_OVERHEAD,
                    )
                };
                assert_eq!(
                    reply,
                    format!("{head}7 5 {len} {encoded}\n"),
                    "base64 parity at input length {len}",
                );
                assert_eq!(
                    reply.capacity(),
                    reserved,
                    "the reply must not reallocate at input length {len} (meta={want_metadata})",
                );
            }
        }
    }

    #[test]
    fn image_bytes_target_bypasses_filesystem_confinement() {
        let budgets = crate::control::ArtifactBudgets::shipping();
        let mut permit = budgets
            .try_reserve_in_memory()
            .expect("a fresh budget has room");
        let (_target, display_path) = prepare_image_target(
            true,
            false,
            "nested/paths-are-invalid-for-file-output.png",
            std::path::Path::new("missing-socket-directory"),
            &budgets,
            &mut permit,
        )
        .expect("inline output needs no filesystem target");
        assert_eq!(display_path, None);
    }

    #[test]
    fn image_meta_is_an_opt_in_flag_and_preserves_legacy_names() {
        assert_eq!(
            parse_image_options("plain --meta --bytes shot.png"),
            ImageOptions {
                want_bytes: true,
                want_metadata: true,
                rest: "plain shot.png".to_string(),
            }
        );
        assert_eq!(
            parse_image_options("meta"),
            ImageOptions {
                want_bytes: false,
                want_metadata: false,
                rest: "meta".to_string(),
            },
            "only --meta is reserved; an existing file literally named meta still works",
        );
    }

    #[test]
    fn image_meta_protocol_is_additive_and_keeps_legacy_replies_exact() {
        let metadata = native_image_metadata();
        assert_eq!(
            image_file_reply(800, 600, "/tmp/legacy.png", false, Some(&metadata)),
            "OK 800 600 /tmp/legacy.png\n",
        );
        let file = image_file_reply(800, 600, "/tmp/frame.png", true, Some(&metadata));
        assert!(file.starts_with("OK 800 600 image-meta image-meta-version=3 frame-kind=native "));
        for field in [
            "image-meta-version=3",
            "frame-phase=presented",
            "view=42",
            "generation=7",
            "config-revision=11",
            "update-revision=13",
            "document-seq=17",
            "presentation-revision=19",
            "capture-serial=23",
            "dimensions=800x600",
            "pixel-fingerprint=0000000011112222",
            "compiled-fingerprint=000000001234abcd",
            "leaf-count=1",
            "leaves=v42:native:s-:f1:",
            "path=\"/tmp/frame.png\"",
        ] {
            assert!(file.contains(field), "missing {field}: {file}");
        }

        let png = [0x89, b'P', b'N', b'G'];
        assert_eq!(
            image_bytes_reply(2, 3, &png, false, Some(&metadata)).unwrap(),
            "OK 1\n2 3 4 iVBORw==\n",
        );
        let bytes = image_bytes_reply(2, 3, &png, true, Some(&metadata)).unwrap();
        let lines = bytes.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "OK 2");
        assert!(lines[1].starts_with("image-meta image-meta-version=3 frame-kind=native "));
        assert!(lines[1].contains("capture-serial=23"));
        assert_eq!(lines[2], "2 3 4 iVBORw==");
        assert!(image_byte_reply_png_fits(MAX_IMAGE_BYTE_REPLY_PNG_BYTES));
        assert!(
            !image_byte_reply_png_fits(MAX_IMAGE_BYTE_REPLY_PNG_BYTES.saturating_add(1)),
            "the server never emits a row its shipping client must reject",
        );

        let unavailable = image_file_reply(4, 5, "/tmp/unknown.png", true, None);
        assert!(unavailable.contains(
            "image-meta-version=3 frame-kind=unknown frame-phase=unknown identity=unavailable"
        ));
    }

    /// Bare `video` keeps the historical defaults (3s, half-res, no keys/pace,
    /// no fps cap, DEFAULT_BUDGET); `budget=<MiB>` and `fps=<n>` ride beside
    /// the flags; out-of-range numbers CLAMP (secs 0.5..=60, fps 1..=120,
    /// budget 64..=4096 MiB) — never reject, never pass through raw.
    #[test]
    fn video_args_defaults_values_and_clamps() {
        let a = parse_video_args("").expect("defaults parse");
        assert_eq!(a.secs, 3.0);
        assert!(!a.full_res && !a.keys && !a.pace);
        assert_eq!(a.fps, None);
        assert_eq!(a.budget_bytes, aterm_gpu::video_tap::DEFAULT_BUDGET);
        let a = parse_video_args("5 full keys pace fps=10 budget=1024").expect("parses");
        assert_eq!(a.secs, 5.0);
        assert!(a.full_res && a.keys && a.pace);
        assert_eq!(a.fps, Some(10));
        assert_eq!(a.budget_bytes, 1024 << 20);
        assert_eq!(parse_video_args("0.1").unwrap().secs, 0.5);
        assert_eq!(parse_video_args("999").unwrap().secs, 60.0);
        assert_eq!(parse_video_args("fps=0").unwrap().fps, Some(1));
        assert_eq!(parse_video_args("fps=500").unwrap().fps, Some(120));
        assert_eq!(parse_video_args("budget=8").unwrap().budget_bytes, 64 << 20);
        assert_eq!(
            parse_video_args("budget=999999").unwrap().budget_bytes,
            4096 << 20
        );
    }

    /// Unknown tokens and malformed key=value forms are still REJECTED with
    /// the usage error — the new args must not turn typos into defaults.
    #[test]
    fn video_args_rejects_unknown_and_malformed() {
        for bad in [
            "bogus",
            "fps=abc",
            "fps=",
            "budget=abc",
            "budget=",
            "fps=-1",
        ] {
            let err = parse_video_args(bad).expect_err(bad);
            assert!(err.starts_with("ERR video:"), "{bad}: {err}");
        }
    }

    #[test]
    fn capture_timeout_reports_err_only_when_cancellation_wins() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::control::CaptureCancellation::new();
        let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
        let cancelled =
            recv_capture_reply::<(u32, u32)>(&rx, &mut cancel_on_drop, Duration::ZERO, "image")
                .expect_err("live request is cancelled at timeout");
        assert!(cancelled.contains("publication cancelled"));
        drop(tx);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::control::CaptureCancellation::new();
        assert!(cancel.authorize_commit(), "worker wins before timeout");
        let cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut cancel_on_drop = cancel_on_drop;
            done_tx
                .send(recv_capture_reply(
                    &rx,
                    &mut cancel_on_drop,
                    Duration::ZERO,
                    "image",
                ))
                .unwrap();
        });
        tx.send(Ok(crate::control::Retained::plain((7_u32, 9_u32))))
            .unwrap();
        let reply = done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("committed result completes the pending waiter")
            .expect("commit winner is awaited instead of receiving a false timeout ERR");
        assert_eq!(reply.value, (7, 9));
        waiter.join().unwrap();
    }

    #[test]
    fn video_timeout_awaits_commit_winner_instead_of_returning_err() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        let mut cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel.clone()));
        let cancelled = recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO)
            .expect_err("live recording is cancelled at timeout");
        assert!(cancelled.contains("publication cancelled"));
        drop(tx);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        assert!(cancel.authorize_commit(), "encoder wins before timeout");
        let cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut cancel_on_drop = cancel_on_drop;
            done_tx
                .send(recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO))
                .unwrap();
        });
        tx.send(crate::control::Retained::plain("OK video\n".to_string()))
            .unwrap();
        let reply = done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("committed video result completes the pending waiter")
            .expect("commit winner is awaited instead of receiving a false timeout ERR");
        assert_eq!(reply.value, "OK video\n");
        waiter.join().unwrap();
    }

    fn video_test_dir(tag: &str) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "aterm-vf-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn instance_root(sock_dir: &std::path::Path, instance: &str) -> std::path::PathBuf {
        sock_dir.join(super::control_auth::VIDEO_DIR).join(instance)
    }

    /// A minimal fake recording under
    /// `<sock_dir>/video/<instance>/<rec>/index.json` with the exact frame-object
    /// shape `app_introspect` writes, for the `video frames` read.
    fn write_fake_recording(
        sock_dir: &std::path::Path,
        instance: &str,
        rec: &str,
        frames: &[(u64, u64)],
    ) {
        let dir = instance_root(sock_dir, instance).join(rec);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (i, (delta, t_us)) in frames.iter().enumerate() {
            if i > 0 {
                lines.push_str(",\n");
            }
            let n = i + 1;
            std::fs::write(dir.join(format!("frame_{n:04}.png")), b"png").unwrap();
            lines.push_str(&format!(
                "    {{\"n\":{n},\"seq\":{n},\"t_us\":{t_us},\"fp\":0,\"delta\":{delta},\"file\":\"frame_{n:04}.png\"}}"
            ));
        }
        std::fs::write(
            dir.join("index.json"),
            format!("{{\"frames\":[\n{lines}\n]}}"),
        )
        .unwrap();
        std::fs::write(
            dir.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"aterm-video-published-v1\n",
        )
        .unwrap();
    }

    #[test]
    fn video_frame_row_formatter_preserves_wire_shape() {
        let temp = aterm_tempfile::tempdir().expect("temporary frame directory");
        let recording = crate::pinned_dir::PinnedDir::open_resolved(temp.path())
            .expect("pinned frame directory");
        let name = std::ffi::OsStr::new("frame_0001.png");
        let identity = recording
            .write_new_private(name, b"png")
            .expect("private frame")
            .into_identity()
            .expect("frame identity");
        let path = recording.path().join(name);
        let rows = [PinnedVideoFrame {
            candidate: VideoFrameCandidate {
                delta: u64::MAX,
                n: u64::MAX,
                seq: u64::MAX,
                t_us: u64::MAX,
                file: "frame_0001.png".to_string(),
            },
            path: path.clone(),
            identity,
        }];

        let output = format_video_frame_rows(&rows);
        assert_eq!(
            output,
            format!(
                "OK 1\nframe n={} delta={} t_us={} seq={} {}\n",
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                path.display()
            )
        );
    }

    /// `video frames` returns the highest-`delta` frames of the NEWEST finished
    /// recording, ordered most-changed first, framed `OK <n>` + n rows — and honors
    /// `count=`. A recording DIR with no index.json (still encoding) and every
    /// live sibling's instance namespace are skipped.
    #[test]
    fn video_frames_ranks_by_delta_picks_newest_and_isolates_instance() {
        const INSTANCE: &str = "p101-current";
        const SIBLING: &str = "p202-sibling";
        let tmp = video_test_dir("rank");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Older recording (should be ignored once a newer complete one exists).
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(5, 10), (1, 20)]);
        // A still-encoding dir: newest name, but NO index.json -> must be skipped.
        std::fs::create_dir_all(instance_root(&tmp, INSTANCE).join("rec-1000000002-000")).unwrap();
        // Newest COMPLETE recording: deltas 3, 40, 7, 2 across four frames.
        write_fake_recording(
            &tmp,
            INSTANCE,
            "rec-1000000001-000",
            &[(3, 0), (40, 16), (7, 33), (2, 50)],
        );
        // A live sibling can own a lexicographically newer recording, but this
        // instance's read must never discover or return it.
        write_fake_recording(&tmp, SIBLING, "rec-9999999999-000", &[(999, 99)]);

        // count=2 -> the two highest deltas (40 then 7), most-changed first.
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            "count=2".split_whitespace(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with("OK 2\n"), "framed header: {out:?}");
        let body: Vec<&str> = out.lines().skip(1).collect();
        assert!(
            body[0].contains("delta=40") && body[0].contains("n=2"),
            "top: {out:?}"
        );
        assert!(
            body[1].contains("delta=7") && body[1].contains("n=3"),
            "second: {out:?}"
        );
        // It read the NEWEST complete recording, not the older one (delta=5 max there).
        assert!(
            !out.contains("delta=5") && !out.contains("delta=999"),
            "ignored the older recording and live sibling: {out:?}"
        );
        assert!(!out.contains(SIBLING), "no sibling path disclosed: {out:?}");

        // No count -> default cap, all four frames, still delta-ordered.
        let all = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(all.starts_with("OK 4\n"), "all frames: {all:?}");

        // Bad count / unknown arg are rejected.
        assert!(
            video_frames_for_instance(
                &tmp,
                INSTANCE,
                "count=0".split_whitespace(),
                &crate::control::ArtifactBudgets::shipping()
            )
            .starts_with("ERR ")
        );
        assert!(
            video_frames_for_instance(
                &tmp,
                INSTANCE,
                "bogus".split_whitespace(),
                &crate::control::ArtifactBudgets::shipping()
            )
            .starts_with("ERR ")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The bounded healthy-path selection may discover unusable high ranks.
    /// Backfill must make one borrowed-metadata fallback pass and continue in
    /// rank order without following a symlink or returning fewer valid rows.
    #[cfg(unix)]
    #[test]
    fn video_frames_backfills_past_missing_and_symlinked_high_ranks() {
        use std::os::unix::fs::symlink;

        const INSTANCE: &str = "p707-backfill";
        const RECORDING: &str = "rec-1000000000-000";
        let tmp = video_test_dir("backfill");
        let _ = std::fs::remove_dir_all(&tmp);
        write_fake_recording(
            &tmp,
            INSTANCE,
            RECORDING,
            &[(100, 10), (90, 20), (80, 30), (70, 40)],
        );
        let recording = instance_root(&tmp, INSTANCE).join(RECORDING);
        let missing = recording.join("frame_0001.png");
        std::fs::remove_file(&missing).unwrap();
        let linked = recording.join("frame_0002.png");
        std::fs::remove_file(&linked).unwrap();
        symlink("frame_0003.png", &linked).unwrap();
        assert!(
            !missing.exists(),
            "the highest-ranked frame is genuinely absent"
        );
        assert!(
            std::fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the second rank is genuinely an untrusted symlink"
        );

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            "count=2".split_whitespace(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with("OK 2\n"), "backfill result: {out:?}");
        let body = out.lines().skip(1).collect::<Vec<_>>();
        assert_eq!(body.len(), 2, "backfill rows: {out:?}");
        assert!(
            body[0].contains("delta=80") && body[0].contains("n=3"),
            "first usable rank: {out:?}"
        );
        assert!(
            body[1].contains("delta=70") && body[1].contains("n=4"),
            "second usable rank: {out:?}"
        );
        assert!(!out.contains("delta=100") && !out.contains("delta=90"));

        drop(body);
        drop(out);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn video_frame_reply_handles_stay_bounded() {
        if !super::control_auth::enter_low_nofile_test_child(128) {
            return;
        }

        const INSTANCE: &str = "p606-bounded";
        let temp = aterm_tempfile::tempdir().expect("temporary video root");
        let frames = (0..64).map(|n| (n, n)).collect::<Vec<_>>();
        write_fake_recording(temp.path(), INSTANCE, "rec-1000000000-000", &frames);
        let socket_depth = std::fs::canonicalize(temp.path())
            .unwrap()
            .components()
            .count();
        let units_per_reply = socket_depth
            .saturating_add(2) // `video/<instance>` initial pinned-chain suffix.
            .saturating_add(crate::pinned_dir::PINNED_DIR_OPERATION_DESCRIPTOR_UNITS);
        let expected = super::super::ARTIFACT_HANDOFF_LIMIT
            .min(super::super::ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT / units_per_reply);
        assert!(expected > 0, "the fixture path must fit one admitted reply");

        let budgets = crate::control::ArtifactBudgets::shipping();
        let mut replies = Vec::new();
        while replies.len() < super::super::ARTIFACT_HANDOFF_LIMIT {
            let mut retained =
                match super::confined_video_reader::read(temp.path(), INSTANCE, 64, &budgets) {
                    Ok(Some(retained)) => retained,
                    Err(error) if error == super::super::ARTIFACT_HANDOFF_BUSY => break,
                    result => panic!("unexpected confined read result: {result:?}"),
                };
            assert!(retained.value.starts_with("OK 64\n"), "{}", retained.value);
            retained
                .retention
                .as_mut()
                .expect("frame reply owns publication retention")
                .prepare_write()
                .expect("closed frame identities revalidate at the wire edge");
            replies.push(retained);
        }
        assert_eq!(
            replies.len(),
            expected,
            "admission must spend the exact canonical-depth charge"
        );
        assert!(
            matches!(
                super::confined_video_reader::read(temp.path(), INSTANCE, 64, &budgets),
                Err(error) if error == super::super::ARTIFACT_HANDOFF_BUSY
            ),
            "one more read must fail before doing descriptor-heavy work"
        );
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_revalidates_selected_handles_at_wire_edge() {
        const INSTANCE: &str = "p303-wire";
        let tmp = video_test_dir("wire-revalidate");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(9, 10)]);

        let mut reply = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(reply.starts_with("OK 1\n"));
        let recording = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
        let frame = recording.join("frame_0001.png");
        std::fs::rename(&frame, recording.join("frame_moved.png")).unwrap();
        std::fs::write(&frame, b"replacement").unwrap();

        let error = reply
            .prepare_retention_for_test()
            .expect_err("a replacement after formatting cannot receive the queued OK");
        assert!(error.contains("video frame identity changed"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_wire_guard_survives_later_recording_retention() {
        let tmp = video_test_dir("wire-retention");
        let _ = std::fs::remove_dir_all(&tmp);
        super::control_auth::ensure_private_dir(&tmp).unwrap();

        let mut oldest = super::control_auth::confine_video_dir(&tmp).unwrap();
        let oldest_path = oldest.path().to_path_buf();
        let frame = oldest
            .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
            .unwrap();
        let index = oldest
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                br#"{"frames":[{"n":1,"seq":1,"t_us":10,"fp":0,"delta":9,"file":"frame_0001.png"}]}"#,
            )
            .unwrap();
        oldest
            .publish(std::slice::from_ref(&frame), &index)
            .unwrap();
        drop(oldest.publish_marker().unwrap());
        drop(index);
        drop(frame);
        drop(oldest);
        // The last lease's drop hands the recording's retention sweep to the
        // cleanup worker, and a reader that arrives while it runs is refused
        // ("retention sweep is in progress"): wait for the sweep to end.
        super::control_auth::wait_for_artifact_cleanup_for_test(&oldest_path);

        // A budget of the test's own: no sibling holds its retention slots.
        let mut reply = video_frames_for_instance(
            &tmp,
            super::control_auth::process_instance_id(),
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(reply.starts_with("OK 1\n"), "{reply:?}");
        assert!(reply.contains(oldest_path.to_string_lossy().as_ref()));

        for _ in 0..11 {
            let mut later = super::control_auth::confine_video_dir(&tmp).unwrap();
            let later_index = later
                .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
                .unwrap();
            later.publish(&[], &later_index).unwrap();
            drop(later.publish_marker().unwrap());
            drop(later_index);
            drop(later);
        }

        assert!(
            oldest_path.join("index.json").is_file(),
            "sibling retention cannot quarantine a recording named by a queued frames reply"
        );
        reply
            .prepare_retention_for_test()
            .expect("the queued frame paths still validate at the wire edge");
        drop(reply);
        super::control_auth::wait_for_artifact_cleanup_for_test(&oldest_path);

        let mut next = super::control_auth::confine_video_dir(&tmp).unwrap();
        let next_path = next.path().to_path_buf();
        let next_index = next
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        next.publish(&[], &next_index).unwrap();
        drop(next.publish_marker().unwrap());
        drop(next_index);
        drop(next);
        super::control_auth::wait_for_artifact_cleanup_for_test(&next_path);
        assert!(
            !oldest_path.exists(),
            "after the wire guard drops, the oldest recording becomes eligible"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_release_sweep_stays_on_pinned_root_after_replacement() {
        const INSTANCE: &str = "p304-pinned-release";
        const RECORDINGS: usize = 12;
        let tmp = video_test_dir("pinned-release");
        let _ = std::fs::remove_dir_all(&tmp);
        for sequence in 0..RECORDINGS {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(sequence as u64 + 1, 10)],
            );
        }

        let mut reply = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(reply.starts_with("OK 1\n"), "{reply:?}");
        reply
            .prepare_retention_for_test()
            .expect("reader identities validate before namespace replacement");

        let original = instance_root(&tmp, INSTANCE);
        let selected_key = original.join(format!("rec-{:020}-000", RECORDINGS - 1));
        let moved = original.with_file_name(format!("{INSTANCE}-original"));
        std::fs::rename(&original, &moved).unwrap();
        for sequence in 100..(100 + RECORDINGS) {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(999, 99)],
            );
        }
        let replacement = instance_root(&tmp, INSTANCE);
        std::fs::write(replacement.join("sentinel"), b"replacement").unwrap();

        drop(reply);
        super::control_auth::wait_for_artifact_cleanup_for_test(&selected_key);

        let recording_count = |root: &std::path::Path| {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
                .count()
        };
        assert!(
            recording_count(&moved) < RECORDINGS,
            "last-reader retention makes progress in the exact namespace that was read"
        );
        assert_eq!(
            recording_count(&replacement),
            RECORDINGS,
            "the replacement installed at the old lexical path is never swept"
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"replacement"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn failed_video_frames_read_before_arm_does_not_run_retention() {
        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p305-failed-prearm";
        const RECORDINGS: usize = 12;
        let tmp = video_test_dir("failed-prearm");
        let _ = std::fs::remove_dir_all(&tmp);
        for sequence in 0..RECORDINGS {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(sequence as u64 + 1, 10)],
            );
        }

        let newest = instance_root(&tmp, INSTANCE).join(format!("rec-{:020}-000", RECORDINGS - 1));
        let frame = newest.join("frame_0001.png");
        let mut replaced = false;
        let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
            if replaced || stage != ReadStage::FramePinned {
                return;
            }
            replaced = true;
            std::fs::rename(&frame, newest.join("frame.original")).unwrap();
            std::fs::write(&frame, b"replacement").unwrap();
        });

        assert!(replaced, "the post-lease, pre-arm frame hook was reached");
        match result {
            Err(error) => assert_eq!(error, "recording namespace changed during read"),
            Ok(_) => panic!("the replaced frame identity must fail the reader"),
        }
        let remaining = std::fs::read_dir(instance_root(&tmp, INSTANCE))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
            .count();
        assert_eq!(
            remaining, RECORDINGS,
            "dropping an unarmed failed-read lease performs no retention sweep"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_arm_revalidates_the_selected_recording_identity() {
        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p306-arm-identity";
        const RECORDINGS: usize = 12;
        let tmp = video_test_dir("arm-identity");
        let _ = std::fs::remove_dir_all(&tmp);
        for sequence in 0..RECORDINGS {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(sequence as u64 + 1, 10)],
            );
        }

        let root = instance_root(&tmp, INSTANCE);
        let newest_name = format!("rec-{:020}-000", RECORDINGS - 1);
        let newest = root.join(&newest_name);
        let original = root.join(".selected-original");
        let oldest = root.join("rec-00000000000000000000-000");
        let mut replaced = false;
        let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
            if replaced || stage != ReadStage::SelectionValidated {
                return;
            }
            replaced = true;
            std::fs::rename(&newest, &original).unwrap();
            write_fake_recording(&tmp, INSTANCE, &newest_name, &[(999, 99)]);
            std::fs::write(newest.join("sentinel"), b"replacement").unwrap();
        });

        assert!(replaced, "the final pre-Arm hook was reached");
        assert_eq!(
            result,
            Err("recording namespace changed while arming retention"),
            "Arm itself rejects a recording replaced after selection validation"
        );
        assert!(
            oldest.is_dir(),
            "a rejected Arm cannot request retention on final lease drop"
        );
        assert_eq!(
            std::fs::read(newest.join("sentinel")).unwrap(),
            b"replacement",
            "the replacement at the old lexical path is untouched"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// With no recordings at all, `video frames` is a clean `ERR`, not a panic.
    #[test]
    fn video_frames_errors_when_no_recording() {
        const INSTANCE: &str = "p303-empty";
        let tmp = video_test_dir("empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(instance_root(&tmp, INSTANCE)).unwrap();
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with("ERR video frames:"), "{out:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A launch that has not recorded yet has no `video/<instance>` at all, and on a fresh
    /// machine no `video/`. That is the ordinary empty state: it reads like the empty
    /// namespace above and names the one action. Negative control: a `video/` that is a
    /// file, not a directory, is still refused.
    #[test]
    fn video_frames_before_the_first_recording_says_there_is_none() {
        const INSTANCE: &str = "p305-fresh";
        const NONE_YET: &str =
            "ERR video frames: no finished recording found (run `video <seconds>` first)\n";
        let tmp = video_test_dir("fresh");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert_eq!(out, NONE_YET, "no `video/` yet");

        std::fs::create_dir_all(instance_root(&tmp, "p306-sibling")).unwrap();
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert_eq!(
            out, NONE_YET,
            "`video/` exists, this launch's namespace does not"
        );

        let not_a_dir = video_test_dir("fresh-file");
        let _ = std::fs::remove_dir_all(&not_a_dir);
        std::fs::create_dir_all(&not_a_dir).unwrap();
        std::fs::write(not_a_dir.join(super::control_auth::VIDEO_DIR), b"not a dir").unwrap();
        let out = video_frames_for_instance(
            &not_a_dir,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.contains("recording namespace is not confined"),
            "a `video` that is not a directory is refused: {out:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&not_a_dir);
    }

    #[test]
    fn video_frames_caps_index_and_falls_back_to_valid_prior_recording() {
        const INSTANCE: &str = "p404-capped";
        let tmp = video_test_dir("cap");
        let _ = std::fs::remove_dir_all(&tmp);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);
        let oversized = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir_all(&oversized).unwrap();
        std::fs::write(
            oversized.join("index.json"),
            vec![b' '; VIDEO_INDEX_MAX_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            oversized.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=17"),
            "oversized newest index is skipped for the valid prior recording: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_skips_newest_index_until_publication_marker_exists() {
        const INSTANCE: &str = "p405-marker";
        let tmp = video_test_dir("marker");
        let _ = std::fs::remove_dir_all(&tmp);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);

        let queued = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir_all(&queued).unwrap();
        std::fs::write(queued.join("frame_0001.png"), b"queued").unwrap();
        std::fs::write(
            queued.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":20,"fp":0,"delta":999,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        assert!(
            !queued
                .join(super::control_auth::VIDEO_PUBLISHED_FILE)
                .exists(),
            "fixture is a fully encoded reply still queued before wire publication"
        );

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=17"),
            "the reader falls back to the newest marker-visible recording: {out:?}"
        );
        assert!(
            !out.contains("delta=999") && !out.contains(queued.to_string_lossy().as_ref()),
            "an index alone never makes a queued recording discoverable: {out:?}"
        );

        drop(out);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_bounds_recording_namespace_scan() {
        const INSTANCE: &str = "p505-bounded";
        let tmp = video_test_dir("scan");
        let root = instance_root(&tmp, INSTANCE);
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..=VIDEO_RECORDING_SCAN_MAX {
            // Invalid names count too: otherwise an attacker could hide unbounded
            // work behind entries the server later ignores.
            std::fs::create_dir(root.join(format!("junk-{index:04}"))).unwrap();
        }

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.contains("recording namespace has too many entries"),
            "bounded scan fails closed: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_rejects_traversal_and_non_frame_manifest_names() {
        const INSTANCE: &str = "p606-names";
        let tmp = video_test_dir("names");
        let rec = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(rec.join("frame_0001.png"), b"png").unwrap();
        std::fs::write(
            rec.join("index.json"),
            br#"{"frames":[
                {"n":1,"seq":1,"t_us":1,"delta":10,"file":"frame_0001.png"},
                {"n":2,"seq":2,"t_us":2,"delta":999,"file":"../../escape.png"},
                {"n":3,"seq":3,"t_us":3,"delta":998,"file":"/absolute.png"},
                {"n":4,"seq":4,"t_us":4,"delta":997,"file":"notes.txt"}
            ]}"#,
        )
        .unwrap();
        std::fs::write(
            rec.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();

        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with("OK 1\n"), "only one safe row: {out:?}");
        assert!(
            out.contains("frame_0001.png"),
            "safe frame retained: {out:?}"
        );
        for forbidden in ["escape.png", "absolute.png", "notes.txt"] {
            assert!(!out.contains(forbidden), "{forbidden} rejected: {out:?}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_rejects_linked_roots_recordings_indexes_and_frames() {
        use std::os::unix::fs::symlink;

        const INSTANCE: &str = "p707-links";

        // A link at the `video/` root must not redirect the reader outside the
        // canonical socket directory.
        let linked_root = video_test_dir("linked-root");
        let outside_root = video_test_dir("outside-root");
        let _ = std::fs::remove_dir_all(&linked_root);
        let _ = std::fs::remove_dir_all(&outside_root);
        std::fs::create_dir_all(outside_root.join(INSTANCE)).unwrap();
        std::fs::create_dir_all(&linked_root).unwrap();
        symlink(
            &outside_root,
            linked_root.join(super::control_auth::VIDEO_DIR),
        )
        .unwrap();
        let out = video_frames_for_instance(
            &linked_root,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.contains("recording namespace is not confined"),
            "linked root refused: {out:?}"
        );

        // Inside a real namespace, a linked recording and linked index are
        // skipped. A linked frame in an otherwise valid newest index is omitted.
        let tmp = video_test_dir("linked-entries");
        let outside = video_test_dir("linked-outside");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(7, 10)]);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":999,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        std::fs::write(
            outside.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        symlink(
            &outside,
            instance_root(&tmp, INSTANCE).join("rec-1000000002-000"),
        )
        .unwrap();
        let linked_index = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir(&linked_index).unwrap();
        symlink(outside.join("index.json"), linked_index.join("index.json")).unwrap();
        std::fs::write(
            linked_index.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=7"),
            "linked recording and index fall back safely: {out:?}"
        );

        let linked_frame = instance_root(&tmp, INSTANCE).join("rec-1000000003-000");
        std::fs::create_dir(&linked_frame).unwrap();
        std::fs::write(outside.join("frame.png"), b"png").unwrap();
        symlink(
            outside.join("frame.png"),
            linked_frame.join("frame_0001.png"),
        )
        .unwrap();
        std::fs::write(
            linked_frame.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":500,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        std::fs::write(
            linked_frame.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert_eq!(
            out, "OK 0\n",
            "a linked frame is never returned even from a valid completion index"
        );

        for dir in [linked_root, outside_root, tmp, outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Retained directory/file descriptors close every in-request check→open
    /// window. If a same-user process renames and replaces any lexical namespace
    /// component after its handle is acquired, the reader continues on the exact
    /// original objects and then refuses to emit paths whose names no longer
    /// match those objects.
    #[cfg(unix)]
    #[test]
    fn video_frames_handle_anchors_fail_closed_on_concurrent_swaps() {
        use std::os::unix::fs::symlink;

        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p808-race";
        const RECORDING: &str = "rec-1000000000-000";

        for stage in [
            ReadStage::NamespacePinned,
            ReadStage::RecordingPinned,
            ReadStage::IndexPinned,
            ReadStage::FramePinned,
        ] {
            let tag = match stage {
                ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
                ReadStage::NamespacePinned => "namespace",
                ReadStage::RecordingPinned => "recording",
                ReadStage::IndexPinned => "index",
                ReadStage::FramePinned => "frame",
                ReadStage::SelectionValidated => {
                    unreachable!("covered by the final Arm race test")
                }
            };
            let tmp = video_test_dir(&format!("race-{tag}"));
            let moved = video_test_dir(&format!("race-{tag}-moved"));
            let outside = video_test_dir(&format!("race-{tag}-outside"));
            for dir in [&tmp, &moved, &outside] {
                let _ = std::fs::remove_dir_all(dir);
                let _ = std::fs::remove_file(dir);
            }
            write_fake_recording(&tmp, INSTANCE, RECORDING, &[(17, 10)]);
            write_fake_recording(&outside, INSTANCE, RECORDING, &[(999, 99)]);

            let recording = instance_root(&tmp, INSTANCE).join(RECORDING);
            let outside_recording = instance_root(&outside, INSTANCE).join(RECORDING);
            let mut swapped = false;
            let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
                if swapped || seen != stage {
                    return;
                }
                swapped = true;
                match stage {
                    ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
                    ReadStage::NamespacePinned => {
                        std::fs::rename(&tmp, &moved).unwrap();
                        symlink(&outside, &tmp).unwrap();
                    }
                    ReadStage::RecordingPinned => {
                        std::fs::rename(&recording, recording.with_extension("original")).unwrap();
                        symlink(&outside_recording, &recording).unwrap();
                    }
                    ReadStage::IndexPinned => {
                        let index = recording.join("index.json");
                        std::fs::rename(&index, recording.join("index.original")).unwrap();
                        symlink(outside_recording.join("index.json"), index).unwrap();
                    }
                    ReadStage::FramePinned => {
                        let frame = recording.join("frame_0001.png");
                        std::fs::rename(&frame, recording.join("frame.original")).unwrap();
                        symlink(outside_recording.join("frame_0001.png"), frame).unwrap();
                    }
                    ReadStage::SelectionValidated => {
                        unreachable!("covered by the final Arm race test")
                    }
                }
            });

            assert!(swapped, "{stage:?} hook was reached");
            assert_eq!(
                result,
                Err("recording namespace changed during read"),
                "{stage:?} replacement is never emitted"
            );
            let debug = format!("{result:?}");
            assert!(
                !debug.contains("999") && !debug.contains(&outside.display().to_string()),
                "{stage:?} never discloses attacker data/path: {debug}"
            );

            // The namespace-stage case leaves `tmp` as a symlink and the real
            // original tree at `moved`; all other cases leave links inside tmp.
            let _ = std::fs::remove_file(&tmp);
            for dir in [tmp, moved, outside] {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Opening an absolute path with O_NOFOLLOW once protects only its final
    /// component. Resolve-then-swap an intermediate ancestor to a symlink and
    /// prove the component-by-component root walk refuses it before any attacker
    /// recording can be read.
    #[cfg(unix)]
    #[test]
    fn video_frames_rejects_intermediate_ancestor_swap_after_resolution() {
        use std::os::unix::fs::symlink;

        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p909-ancestor";
        const RECORDING: &str = "rec-1000000000-000";

        let outer = video_test_dir("ancestor-race");
        let outside = video_test_dir("ancestor-race-outside");
        let ancestor = outer.join("controlled");
        let moved = outer.join("controlled-original");
        let socket = ancestor.join("socket");
        let outside_socket = outside.join("socket");
        for dir in [&outer, &outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
        write_fake_recording(&socket, INSTANCE, RECORDING, &[(17, 10)]);
        write_fake_recording(&outside_socket, INSTANCE, RECORDING, &[(999, 99)]);

        let mut swapped = false;
        let result = read_for_test(&socket, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
            if swapped || seen != ReadStage::SocketPathResolved {
                return;
            }
            swapped = true;
            std::fs::rename(&ancestor, &moved).unwrap();
            symlink(&outside, &ancestor).unwrap();
        });

        assert!(swapped, "post-canonicalization hook was reached");
        assert_eq!(
            result,
            Err("recording namespace is not confined"),
            "a swapped intermediate ancestor is rejected"
        );
        assert!(
            !format!("{result:?}").contains("999"),
            "attacker index was not read"
        );

        let _ = std::fs::remove_file(&ancestor);
        for dir in [outer, outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// The socket itself is pinned before the resolution hook. Replacing its
    /// name with another ordinary directory (not merely a symlink) must still
    /// fail the identity checkpoint before any child is inspected.
    #[cfg(unix)]
    #[test]
    fn video_frames_rejects_socket_directory_replacement_after_pin() {
        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p910-socket-replacement";
        const RECORDING: &str = "rec-1000000000-000";

        let socket = video_test_dir("socket-replacement");
        let moved = video_test_dir("socket-replacement-original");
        let attacker = video_test_dir("socket-replacement-attacker");
        for dir in [&socket, &moved, &attacker] {
            let _ = std::fs::remove_dir_all(dir);
        }
        write_fake_recording(&socket, INSTANCE, RECORDING, &[(17, 10)]);
        write_fake_recording(&attacker, INSTANCE, RECORDING, &[(999, 99)]);

        let mut swapped = false;
        let result = read_for_test(&socket, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
            if swapped || stage != ReadStage::SocketPathResolved {
                return;
            }
            swapped = true;
            std::fs::rename(&socket, &moved).unwrap();
            std::fs::rename(&attacker, &socket).unwrap();
        });

        assert!(swapped, "post-pin hook was reached");
        assert_eq!(result, Err("recording namespace is not confined"));
        assert!(
            !format!("{result:?}").contains("999"),
            "the replacement recording was not inspected"
        );

        for dir in [socket, moved, attacker] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod call_main_within_tests {
    use std::sync::{Arc, OnceLock, mpsc};
    use std::time::Duration;

    use super::recv_within;

    /// A main thread that does not answer within the deadline is the degrade
    /// signal (any `Err` drives the roster's `window=-` dashes), named as a
    /// timeout; the deadline is `recv_timeout`'s own argument. A live sender
    /// makes it a genuine TIMEOUT; a dropped one (the event loop gone mid-hop)
    /// is reported distinctly.
    #[test]
    fn recv_within_tells_a_timeout_from_a_dropped_reply() {
        let (_tx, rx) = mpsc::channel::<u8>();
        let timed_out = recv_within(&rx, Duration::from_millis(20));
        let (tx, rx) = mpsc::channel::<u8>();
        drop(tx);
        let dropped = recv_within(&rx, Duration::from_secs(60));
        assert!(timed_out.is_err() && dropped.is_err());
        assert_ne!(timed_out, dropped);
    }
    /// A main-thread verb is refused at once while the modal park is the
    /// breadcrumb, and only then.
    #[test]
    fn a_main_thread_verb_is_refused_while_a_dialog_stands() {
        use crate::watchdog::Breadcrumb;
        assert!(super::dialog_refusal_for(Breadcrumb::Modal).is_some());
        for other in [
            Breadcrumb::Startup,
            Breadcrumb::AboutToWait,
            Breadcrumb::WindowEvent,
            Breadcrumb::UserEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::ResizeSettle,
            Breadcrumb::UpdateHandoff,
            Breadcrumb::AboutToWaitWork,
        ] {
            assert_eq!(super::dialog_refusal_for(other), None, "{other:?}");
        }
    }

    /// The hop count behind the stall refusal: a run of outstanding hops
    /// keeps the instant it began for as long as ANY hop of it waits, every
    /// guard that drops counts itself out, and the next run starts afresh.
    #[test]
    fn the_hop_count_keeps_when_its_run_began_until_the_last_hop_ends() {
        let hops = super::Hops::new();
        assert_eq!(hops.since_ns(), None, "nothing outstanding");
        let first = hops.post(10);
        assert_eq!(hops.since_ns(), Some(10));
        let second = hops.post(20);
        assert_eq!(hops.since_ns(), Some(10), "the run began with the first");
        drop(first);
        assert_eq!(
            hops.since_ns(),
            Some(10),
            "a hop still waits: the run goes on"
        );
        drop(second);
        assert_eq!(hops.since_ns(), None, "every guard counted itself out");
        let next = hops.post(30);
        assert_eq!(hops.since_ns(), Some(30), "a new run, a new start");
        drop(next);
        assert_eq!(hops.since_ns(), None);
        // A clock reading of 0 still counts as a start.
        let at_zero = hops.post(0);
        assert!(hops.since_ns().is_some());
        drop(at_zero);
    }

    /// A hop leaves the count when the main thread TAKES it, not when its
    /// worker gets the reply: `settings set` queues its write and is answered
    /// turns later, by the config worker's completion. [`super::take_hop`] is
    /// what `user_event` runs first on the posted `Wake::Hop`, and it hands
    /// back the request itself. Whichever of the take and the guard comes
    /// first counts the hop out, and only once: a reply after the take, or a
    /// take after the worker gave up, never counts out another hop.
    #[test]
    fn a_hop_stops_counting_when_the_main_thread_takes_it() {
        let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
        let parked = hops.post(10);
        let request = super::take_hop(crate::Wake::Hop {
            mark: parked.mark(),
            wake: Box::new(crate::Wake::WarmupResult),
        });
        assert!(
            matches!(request, crate::Wake::WarmupResult),
            "the main thread handles the request itself: {request:?}"
        );
        assert_eq!(
            hops.since_ns(),
            None,
            "taken, its reply queued: its worker still waits, and it is not counted"
        );
        let waiting = hops.post(20);
        assert_eq!(hops.since_ns(), Some(20));
        // The queued reply comes now.
        drop(parked);
        assert_eq!(
            hops.since_ns(),
            Some(20),
            "a reply after the take counts out nothing more"
        );
        let late = waiting.mark();
        // The worker gives up first.
        drop(waiting);
        assert_eq!(hops.since_ns(), None);
        let next = hops.post(30);
        // The main thread takes the abandoned request after all.
        late.taken();
        assert_eq!(
            hops.since_ns(),
            Some(30),
            "a take after the give-up counts out nothing"
        );
        drop(next);
        assert_eq!(hops.since_ns(), None);

        // One hop of a run taken while the next still waits untaken: the run
        // goes on. The reader measures from the later of its start and the
        // beat that took the first, so the older start refuses nothing.
        let first = hops.post(40);
        let second = hops.post(50);
        first.mark().taken();
        assert_eq!(hops.since_ns(), Some(40));
        drop(first);
        assert_eq!(hops.since_ns(), Some(40), "the second still waits");
        drop(second);
        assert_eq!(hops.since_ns(), None);
    }

    /// The refusal a stalled main thread answers with, as the wire carries it
    /// (`ERR {reason}`), and a fixed reason unchanged.
    #[test]
    fn a_stalled_main_thread_answers_a_named_retryable_err() {
        use crate::watchdog::{Breadcrumb, MainStall};
        let stall = MainStall {
            stalled: Duration::from_secs(12),
            root: Breadcrumb::NewEvents,
            returned: true,
        };
        assert_eq!(
            format!("ERR {}\n", super::MainHopError::Stalled(stall)),
            "ERR main thread stalled 12s; retry\n"
        );
    }

    /// An `image` request for these tests, answered on `reply`. Its frame
    /// metadata cell is the caller's, whose strong count tells whether the
    /// request still lives.
    fn image_request(
        reply: mpsc::Sender<crate::control::ImageReply>,
        frame_metadata: &Arc<OnceLock<crate::control::ImageFrameMetadata>>,
    ) -> crate::Wake {
        crate::Wake::Image(Box::new(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: Arc::clone(frame_metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        }))
    }

    /// A pixel capture is a main-thread hop like every other: the `image`
    /// request [`super::post_main`] posts is counted from the post until the
    /// main thread takes it, so the stall refusal sees a capture that waits on
    /// a stuck main thread, and the main thread answers it on the request's own
    /// channel. Negative control: the same request handed to the loop raw, as
    /// `image` handed its queue's `Wake::Control` before, leaves the count
    /// empty, so what the count sees is the hop.
    #[test]
    fn capture_hops_are_counted_until_taken() {
        let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
        let metadata = Arc::new(OnceLock::new());
        let mut posted = vec![image_request(mpsc::channel().0, &metadata)];
        assert_eq!(hops.since_ns(), None, "a raw post is not counted");
        posted.clear();

        let (rx, guard) = super::post_counted(
            hops,
            None,
            |wake| {
                posted.push(wake);
                true
            },
            |reply| image_request(reply, &metadata),
        )
        .expect("an unrefused capture is posted");
        assert!(
            hops.since_ns().is_some(),
            "the posted capture waits on the main thread"
        );
        let [wake] = <[crate::Wake; 1]>::try_from(posted).expect("one wake posted");
        let crate::Wake::Image(request) = super::take_hop(wake) else {
            panic!("the main thread handles the image request itself");
        };
        assert_eq!(hops.since_ns(), None, "taken: the main thread has it");
        request
            .reply
            .send(Err("answered".to_string()))
            .expect("the worker still waits");
        let reply = rx.recv().expect("the main thread's answer");
        assert_eq!(reply.err().as_deref(), Some("answered"));
        drop(guard);
        assert_eq!(hops.since_ns(), None, "the take counted it out once");
    }

    /// A refused hop is answered before its request is built, posted or
    /// counted: [`super::post_main`] asks [`super::main_thread_refusal`]
    /// first, so a stalled main thread or a standing dialog is never handed a
    /// request it cannot answer, a `video` recording now as much as `image`.
    /// A loop that is gone gives the wake back: the request drops with its
    /// guards and its hop counts itself out. Negative control: unrefused, the
    /// same call builds once, posts one `Wake::Hop` and counts it.
    #[test]
    fn post_main_refuses_before_posting() {
        use crate::watchdog::{Breadcrumb, MainStall};
        let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
        let stalled = super::MainHopError::Stalled(MainStall {
            stalled: Duration::from_secs(6),
            root: Breadcrumb::NewEvents,
            returned: false,
        });
        let (mut built, mut sent) = (0, 0);
        let refused = super::post_counted::<u8>(
            hops,
            Some(stalled),
            |_| {
                sent += 1;
                true
            },
            |_| {
                built += 1;
                crate::Wake::WarmupResult
            },
        );
        assert_eq!(refused.err(), Some(stalled));
        assert_eq!((built, sent), (0, 0), "never built, never posted");
        assert_eq!(hops.since_ns(), None, "never counted");

        let metadata = Arc::new(OnceLock::new());
        let gone = super::post_counted(
            hops,
            None,
            |_| false,
            |reply| image_request(reply, &metadata),
        );
        assert_eq!(
            gone.err(),
            Some(super::MainHopError::Reason("event loop gone"))
        );
        assert_eq!(
            Arc::strong_count(&metadata),
            1,
            "the unsent request dropped with its guards"
        );
        assert_eq!(hops.since_ns(), None, "its hop counted itself out");

        let mut posted = Vec::new();
        let (_rx, guard) = super::post_counted::<u8>(
            hops,
            None,
            |wake| {
                posted.push(wake);
                true
            },
            |_| {
                built += 1;
                crate::Wake::WarmupResult
            },
        )
        .expect("unrefused, the hop is posted");
        assert_eq!(built, 1, "built once");
        assert!(
            matches!(posted.as_slice(), [crate::Wake::Hop { .. }]),
            "one counted hop posted: {posted:?}"
        );
        assert!(hops.since_ns().is_some(), "and counted");
        drop(guard);
        assert_eq!(hops.since_ns(), None);
    }
}

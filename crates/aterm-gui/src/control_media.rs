// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket media capture (`image`, `window`, `video`), UI inspection, and session
//! `spawn`/`close` verbs.

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

/// A minute hang detector for a required reply from this instance's main thread.
const MAIN_THREAD_REPLY_HANG: std::time::Duration = std::time::Duration::from_secs(60);
const MAIN_THREAD_REPLY_LATE: &str = "main-thread reply did not arrive within 60s";

/// Wire-ready error reason returned when a main-thread RPC hop fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainHopError {
    Reason(&'static str),
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

pub(crate) const CLIENT_HUNG_UP: &str = "client hung up before the request reached the main thread";

/// Counter and oldest-timestamp tracker for in-flight main-thread RPC hops.
pub(crate) struct Hops {
    outstanding: AtomicUsize,
    run_since_ns: AtomicU64,
}

impl Hops {
    /// Creates an empty [`Hops`] tracker.
    pub(crate) const fn new() -> Self {
        Self {
            outstanding: AtomicUsize::new(0),
            run_since_ns: AtomicU64::new(0),
        }
    }

    /// Records a newly posted hop at `now_ns` and returns its [`HopGuard`].
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

    /// Returns the timestamp in nanoseconds when the current run of unserved hops began.
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

/// Token carried by [`Wake::Hop`] that decrements [`Hops`] once the main thread takes the event.
pub(crate) struct HopMark<'a> {
    hops: &'a Hops,
    counted: Arc<AtomicBool>,
}

impl HopMark<'_> {
    /// Marks this hop as taken by the main thread.
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

/// Worker-side guard that decrements [`Hops`] on drop if the main thread has not yet taken the hop.
pub(crate) struct HopGuard<'a> {
    mark: HopMark<'a>,
}

impl<'a> HopGuard<'a> {
    /// Creates the [`HopMark`] paired with this guard for [`Wake::Hop`].
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

static HOPS: Hops = Hops::new();

/// Unwraps a [`Wake::Hop`] on the main thread and marks its hop as taken.
pub(crate) fn take_hop(ev: Wake) -> Wake {
    match ev {
        Wake::Hop { mark, wake } => {
            mark.taken();
            take_hop(*wake)
        }
        other => other,
    }
}

/// Posts a closure to the main thread via `proxy` and returns the reply receiver and [`HopGuard`].
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

/// Main-thread event sender abstraction supporting injected test transports.
pub(crate) struct MainPost<'a> {
    send: Box<dyn FnMut(Wake) -> bool + 'a>,
    hops: &'static Hops,
}

impl<'a> MainPost<'a> {
    /// Wraps an [`EventLoopProxy<Wake>`] counted on the process-wide [`Hops`] tracker.
    pub(crate) fn new(proxy: &'a EventLoopProxy<Wake>) -> Self {
        Self {
            send: Box::new(|wake| proxy.send_event(wake).is_ok()),
            hops: &HOPS,
        }
    }

    /// Wraps a custom send closure counted on `hops` for unit tests.
    #[cfg(test)]
    pub(crate) fn injected(send: impl FnMut(Wake) -> bool + 'a, hops: &'static Hops) -> Self {
        Self {
            send: Box::new(send),
            hops,
        }
    }

    /// Posts a counted main-thread request without pre-checking [`main_thread_refusal`].
    pub(crate) fn ask<T>(
        &mut self,
        make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
    ) -> Result<(std::sync::mpsc::Receiver<T>, HopGuard<'static>), MainHopError> {
        post_counted(self.hops, None, &mut self.send, make)
    }

    /// Posts an uncounted [`Wake`] event and returns whether the receiver accepted it.
    pub(crate) fn tell(&mut self, wake: Wake) -> bool {
        (self.send)(wake)
    }
}

/// Posts a closure to the main thread and waits up to the default allowance for its reply.
pub(crate) fn call_main<T>(
    proxy: &EventLoopProxy<Wake>,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<T, MainHopError> {
    let (rx, _hop) = post_main(proxy, make)?;
    recv_main(&rx)
}

/// Waits under [`MAIN_THREAD_REPLY_HANG`] for a required main-thread reply.
///
/// # Errors
///
/// The reply did not arrive within the allowance, or the main thread dropped its channel
/// unanswered.
pub(crate) fn recv_main<T>(rx: &std::sync::mpsc::Receiver<T>) -> Result<T, MainHopError> {
    recv_main_at(rx, std::time::Instant::now())
}

fn recv_main_at<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    started: std::time::Instant,
) -> Result<T, MainHopError> {
    let within = MAIN_THREAD_REPLY_HANG
        .checked_sub(started.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or(MainHopError::Reason(MAIN_THREAD_REPLY_LATE))?;
    match rx.recv_timeout(within) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(MainHopError::Reason(MAIN_THREAD_REPLY_LATE))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(MainHopError::Reason("main-thread reply dropped"))
        }
    }
}

/// One main-thread request decided exactly once: the main thread takes it ([`Claim::take`]) or
/// the lane that posted it gives it up ([`Claim::give_up`]), whichever comes first, so a request
/// whose lane answered its client without it never acts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Claim(Arc<AtomicBool>);

impl Claim {
    /// Whether the main thread takes the request: `false` once its lane gave it up.
    pub(crate) fn take(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    /// Whether the lane gives the request up: `false` once the main thread took it, whose answer
    /// then follows on the request's reply channel.
    pub(crate) fn give_up(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
}

/// How a parked lane stopped waiting for the main thread's answer ([`park_for_answer`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Waited<T> {
    Answer(T),
    HungUp,
    Deadline,
    /// The main thread dropped the channel unanswered (the event loop ended).
    Dropped,
}

/// Wait on `answer` until `deadline`, re-checking `hung_up` between slices.
pub(crate) fn park_for_answer<T>(
    answer: &std::sync::mpsc::Receiver<T>,
    deadline: std::time::Instant,
    hung_up: &mut dyn FnMut() -> bool,
    park: fn(std::time::Duration) -> std::time::Duration,
) -> Waited<T> {
    loop {
        if hung_up() {
            return Waited::HungUp;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match answer.recv_timeout(park(left)) {
            Ok(answer) => return Waited::Answer(answer),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Waited::Dropped,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if std::time::Instant::now() >= deadline {
                    return Waited::Deadline;
                }
            }
        }
    }
}

/// Posts a closure to the main thread and waits up to `within` for its reply.
pub(crate) fn call_main_within<T>(
    proxy: &EventLoopProxy<Wake>,
    within: std::time::Duration,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<T, MainHopError> {
    let (rx, _hop) = post_main(proxy, make)?;
    recv_within(&rx, within).map_err(MainHopError::Reason)
}

/// Returns the refusal for a main-thread hop: the client hung up (unless its input is
/// committed), the main thread is stalled, or (macOS) aterm is showing a modal dialog.
pub(crate) fn main_thread_refusal() -> Option<MainHopError> {
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

#[cfg(any(target_os = "macos", test))]
fn dialog_refusal_for(breadcrumb: crate::watchdog::Breadcrumb) -> Option<&'static str> {
    (breadcrumb == crate::watchdog::Breadcrumb::Modal)
        .then_some("aterm is showing a dialog; answer it, then retry")
}

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

pub(crate) const MAX_IMAGE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Encodes an [`ImageData`] into a size-capped base64 [`ImagePayload`] using the process memo.
pub(crate) fn image_payload(img: &Arc<ImageData>) -> ImagePayload {
    image_payloads(&[img])
        .pop()
        .unwrap_or(ImagePayload::TRUNCATED)
}

/// Formats a single `image read` output line (without trailing newline).
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

const IMAGE_READ_LINE_BYTES: usize = 96;

/// Handles `image read [<r> [<c>]]`, returning inline terminal images as base64 lines.
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

    if let (Some(rs), Some(cs)) = (r_tok, c_tok) {
        if rs.is_empty()
            || cs.is_empty()
            || !rs.bytes().all(|b| b.is_ascii_digit())
            || !cs.bytes().all(|b| b.is_ascii_digit())
        {
            return "ERR bad args\n".to_string();
        }
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

    let placements: Vec<(usize, Vec<(usize, ImageRef)>)> = match r_tok {
        Some(rs) => {
            if rs.is_empty() || !rs.bytes().all(|b| b.is_ascii_digit()) {
                return "ERR bad args\n".to_string();
            }
            match rs.parse::<usize>() {
                Ok(r) if r < rows => vec![(r, t.images_row(r))],
                Ok(_) => {
                    return format!(
                        "ERR out of range: rows 0-{}, cols 0-{}\n",
                        rows.saturating_sub(1),
                        cols.saturating_sub(1)
                    );
                }
                Err(_) => return "ERR bad args\n".to_string(),
            }
        }
        None => t.images_frame(rows).into_iter().enumerate().collect(),
    };
    let mut seen: std::collections::HashSet<*const ImageData> = std::collections::HashSet::new();
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
        image_read_line(&mut out, *anchor_r, *anchor_c, 0, 0, img, payload);
        out.push('\n');
    }
    out
}

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
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => match rx.recv() {
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
        },
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
    let path = target.display_path().to_string_lossy().into_owned();
    Ok((target, Some(path)))
}

/// Handles `image`, capturing the client frame of the window showing the target session (the
/// front window by default) as a PNG file, or inline with `--bytes`.
pub(crate) fn cmd_image(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    session: Option<u64>,
) -> crate::control::ControlReply {
    let options = parse_image_options(rest);
    let want_bytes = options.want_bytes;
    let want_metadata = options.want_metadata;
    let rest = options.rest.as_str();
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
                return "ERR path: give a bare filename in images/ (not `auto`, `.capture-locks` or \
                        a symlink), or omit it to auto-name one\n"
                    .into();
            }
        };
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
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
    match recv_capture_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs(120),
        "image",
    ) {
        Ok(retained) if matches!(retained.value, (0, 0, _)) => {
            "ERR image: nothing to capture; retry\n".into()
        }
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

const IMAGE_BYTE_ROW_OVERHEAD: usize = 64;
const MAX_IMAGE_BYTE_REPLY_PNG_BYTES: usize =
    ((aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES - IMAGE_BYTE_ROW_OVERHEAD) / 4) * 3;

const fn image_byte_reply_png_fits(png_len: usize) -> bool {
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

fn default_stem(aux: crate::app_introspect::AuxTarget) -> String {
    match aux {
        crate::app_introspect::AuxTarget::Front => "aterm-window".to_string(),
        target => format!("aterm-{}", target.keyword()),
    }
}

/// Handles `window [<target>] [path]`, capturing a composited window PNG.
pub(crate) fn cmd_window(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
) -> crate::control::ControlReply {
    use crate::app_introspect::AuxTarget;
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
                return "ERR path: give a bare filename in images/ (not `auto`, `.capture-locks` or \
                        a symlink), or omit it to auto-name one\n"
                    .into();
            }
        }
    };
    let path = confined.display_path().to_string_lossy().into_owned();
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
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
        Err(msg) => format!("ERR {msg}\n").into(),
    }
}

#[cfg(any(target_os = "macos", windows, test))]
impl crate::App {
    /// Returns an error string if the front window does not match the requested `window <target>`.
    pub(crate) fn window_target_refusal(
        &self,
        target: crate::app_introspect::AuxTarget,
    ) -> Option<String> {
        use crate::app_introspect::AuxTarget;
        use crate::native_settings::SettingsRoute;
        let shown = match target {
            AuxTarget::Front => return None,
            AuxTarget::Prefs => self.native_settings_front_view_target().is_some(),
            AuxTarget::About => self.front_about_view().is_some(),
            AuxTarget::Update => self
                .native_settings_route_view_target(SettingsRoute::SoftwareUpdate)
                .is_some(),
            AuxTarget::Menu => self.front().is_some_and(|ws| ws.palette().is_some()),
            AuxTarget::TabMenu => self.front().is_some_and(|ws| ws.tab_menu.is_some()),
            AuxTarget::FactMenu => self
                .autopilot_menu
                .as_ref()
                .is_some_and(|m| Some(m.wid) == self.frontmost_window),
            AuxTarget::Overview => self.front().is_some_and(|ws| ws.overview().is_some()),
        };
        if shown {
            return None;
        }
        let name = target.keyword();
        Some(match target {
            AuxTarget::FactMenu => {
                format!("{name} is not open in the front window; use `open {name} <fact>` first")
            }
            _ => format!("{name} is not open in the front window; use `open {name}` first"),
        })
    }
}

#[cfg(test)]
#[path = "control_media_window_target_refusal_tests.rs"]
mod window_target_refusal_tests;

#[derive(Debug)]
struct VideoArgs {
    secs: f64,
    full_res: bool,
    keys: bool,
    pace: bool,
    fps: Option<u32>,
    budget_bytes: usize,
}

pub(crate) const VIDEO_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(60);

const VIDEO_USAGE: &str = "usage: video [<seconds>] [full|half] [keys] [pace] [fps=<n>] [budget=<MiB>] (seconds default 3) | video status|stop | video frames [count=N]";

const VIDEO_FRAMES_DEFAULT: usize = 8;
const VIDEO_FRAMES_MAX: usize = 64;

const VIDEO_RECORDING_SCAN_MAX: usize = 128;

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
        let video = match socket.child(std::ffi::OsStr::new(control_auth::VIDEO_DIR)) {
            Ok(video) => video,
            Err(error) => return absent_or_unconfined(&error, &socket),
        };
        match video.child(std::ffi::OsStr::new(instance)) {
            Ok(namespace) => Ok(Some(namespace)),
            Err(error) => absent_or_unconfined(&error, &video),
        }
    }

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
            match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<usize>().ok())
                .flatten()
            {
                Some(n) if n >= 1 => count = n.min(VIDEO_FRAMES_MAX),
                _ => {
                    return format!(
                        "ERR video frames: bad count '{t}' (1 or more; at most {VIDEO_FRAMES_MAX} are listed)\n"
                    )
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
                    match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse::<u32>().ok())
                        .flatten()
                    {
                        Some(n) => args.fps = Some(n.clamp(1, 120)),
                        None => {
                            return Err(format!("ERR video: bad fps '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else if let Some(v) = t.strip_prefix("budget=") {
                    match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse::<u64>().ok())
                        .flatten()
                    {
                        Some(mib) => args.budget_bytes = (mib.clamp(64, 4096) as usize) << 20,
                        None => {
                            return Err(format!("ERR video: bad budget '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else {
                    match (!t.starts_with('+'))
                        .then(|| t.parse::<f64>().ok())
                        .flatten()
                    {
                        Some(s) if s.is_finite() => args.secs = s,
                        Some(_) => {
                            return Err(format!(
                                "ERR video: non-finite duration '{t}' ({VIDEO_USAGE})\n"
                            ));
                        }
                        None => {
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
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => match rx.recv() {
            Ok(reply) => {
                cancel_on_drop.disarm();
                Ok(reply)
            }
            Err(_) => {
                cancel_on_drop.disarm();
                Err("recording worker disconnected after commit authorization".to_string())
            }
        },
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("recording worker disconnected".to_string())
        }
    }
}

/// Handles `video [<seconds>] ...` (and `status`, `stop`, `frames`), recording the front
/// window's presented frames to `frame_NNNN.png` files plus an `index.json`.
pub(crate) fn cmd_video(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    owner: bool,
) -> crate::control::ControlReply {
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
    let (wire, written) = crate::control::ReplyWire::new();
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
    match recv_video_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::try_from_secs_f64(args.secs + 120.0)
            .unwrap_or_else(|_| std::time::Duration::from_secs(3600)),
    ) {
        Ok(reply) => {
            let (body, retention) = reply.into_parts();
            crate::control::ControlReply::with_handoff(body, retention)
        }
        Err(error) => crate::control::ControlReply::from(format!("ERR video: {error}\n")),
    }
    .releasing_on_write(written)
}

/// Handles `controls <target>`, dumping semantic controls for a compatibility GUI surface.
pub(crate) fn cmd_controls(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    use crate::app_introspect::AuxTarget;
    let trimmed = rest.trim();
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
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Handles `inspect`, reading the versioned semantic surface of a native tab view.
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

/// Handles `act`, dispatching a semantic action to a native view on the main thread.
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

fn open_target(trimmed: &str) -> Result<(crate::app_introspect::AuxTarget, bool), String> {
    use crate::app_introspect::AuxTarget;
    use aterm_types::control_verbs::OPEN_TARGETS;
    let (target_tok, close) = match trimmed.split_once(char::is_whitespace) {
        Some((t, r)) if r.trim() == "close" => (t, true),
        Some(_) => {
            return Err(format!(
                "ERR usage: open <{OPEN_TARGETS}> [close] | open fact-menu <fact> | open app <…>\n"
            ));
        }
        None => (trimmed, false),
    };
    AuxTarget::parse(target_tok)
        .filter(|t| t.opens())
        .map(|t| (t, close))
        .ok_or_else(|| format!("ERR unknown target (use: {OPEN_TARGETS})\n"))
}

fn fact_menu_operand(trimmed: &str) -> Option<Result<Option<crate::claude_lights::Fact>, String>> {
    use crate::claude_lights::Fact;
    let mut words = trimmed.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("fact-menu") {
        return None;
    }
    let operand = words.next().map(str::to_ascii_lowercase);
    let usage = || {
        let names: Vec<&str> = Fact::EVERY.into_iter().map(Fact::key).collect();
        format!(
            "ERR usage: open fact-menu <{}> | open fact-menu close\n",
            names.join(" | ")
        )
    };
    if words.next().is_some() {
        return Some(Err(usage()));
    }
    Some(match operand.as_deref() {
        Some("close") => Ok(None),
        Some(word) => Fact::parse(word).map(Some).ok_or_else(usage),
        None => Err(usage()),
    })
}

/// What an `open` line asks of the main thread, read off its words before any hop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenRoute {
    /// `open app <…>` ([`crate::app_control::parse_open`]).
    App(crate::app_control::OpenRequest),
    /// `open fact-menu <fact>` (`Some`) or `open fact-menu close` (`None`) ([`fact_menu_operand`]).
    FactMenu(Option<crate::claude_lights::Fact>),
    /// `open <target> [close]` ([`open_target`]).
    Aux {
        target: crate::app_introspect::AuxTarget,
        close: bool,
    },
    /// The client's answer, with no hop made.
    Refused(String),
}

impl OpenRoute {
    /// Whether the route brings something up over what the window shows, which the desk door
    /// judges ([`crate::desk_door::Effect::Open`]): every route but a close and a refusal.
    pub(crate) const fn opens(&self) -> bool {
        match self {
            Self::App(_) | Self::FactMenu(Some(_)) => true,
            Self::Aux { close, .. } => !*close,
            Self::FactMenu(None) | Self::Refused(_) => false,
        }
    }
}

/// Route `open`'s words: a native app, fact menu, or auxiliary surface.
pub(crate) fn open_route(rest: &str) -> OpenRoute {
    let trimmed = rest.trim();
    if trimmed.split_whitespace().next() == Some("app") {
        return match crate::app_control::parse_open(trimmed) {
            Ok(request) => OpenRoute::App(request),
            Err(error) => OpenRoute::Refused(format!("ERR {error}\n")),
        };
    }
    if let Some(operand) = fact_menu_operand(trimmed) {
        return operand.map_or_else(OpenRoute::Refused, OpenRoute::FactMenu);
    }
    open_target(trimmed).map_or_else(OpenRoute::Refused, |(target, close)| OpenRoute::Aux {
        target,
        close,
    })
}

/// Handles `open` as [`open_route`] read it: a native tab or auxiliary surface opened or closed.
pub(crate) fn cmd_open(proxy: &EventLoopProxy<Wake>, route: OpenRoute) -> String {
    match route {
        OpenRoute::Refused(refusal) => refusal,
        OpenRoute::App(request) => {
            match call_main(proxy, |reply| Wake::OpenApp { request, reply }) {
                Ok(Ok(message)) => format!("OK {message}\n"),
                Ok(Err(error)) => format!("ERR {error}\n"),
                Err(error) => format!("ERR {error}\n"),
            }
        }
        OpenRoute::FactMenu(fact) => {
            match call_main(proxy, |reply| Wake::OpenFactMenu { fact, reply }) {
                Ok(Ok(())) => format!(
                    "OK {} fact-menu\n",
                    if fact.is_none() { "closed" } else { "opened" }
                ),
                Ok(Err(msg)) => format!("ERR {msg}\n"),
                Err(e) => format!("ERR {e}\n"),
            }
        }
        OpenRoute::Aux { target, close } => match call_main(proxy, |tx| Wake::OpenAuxWindow {
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
        },
    }
}

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

fn parse_settings_section_route(input: &str) -> Option<crate::native_settings::SettingsRoute> {
    use crate::native_settings::SettingsRoute;

    let normalized = normalize_settings_section_name(input);
    if let Some(route) = SettingsRoute::ALL.into_iter().find(|route| {
        normalize_settings_section_name(route.label()) == normalized
            || normalize_settings_section_name(route.path()) == normalized
    }) {
        return Some(route);
    }
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
        "kitty" | "cat" | "cursor cat" | "cursor pet" | "kitty pet" | "cursor kitty"
        | "cursor style" | "styles" | "trail" | "trails" | "cursor trail" => {
            Some(SettingsRoute::CursorStyles)
        }
        "sounds" | "audio" | "music" | "music effects" | "volume" => Some(SettingsRoute::Sound),
        "windows" | "window and tabs" | "window & tabs" => Some(SettingsRoute::WindowTabs),
        "input" | "keyboard" | "keyboard and input" => Some(SettingsRoute::KeyboardInput),
        "update" | "software updates" => Some(SettingsRoute::SoftwareUpdate),
        "log" | "logs" | "history" | "history log" | "activity log" | "message log"
        | "messages log" => Some(SettingsRoute::Messages),
        "coding agent harness" | "claude code harness" | "autopilot" | "supervisor" => {
            Some(SettingsRoute::Harness)
        }
        "ai cli" | "ai reviews" | "ai logins" => Some(SettingsRoute::Ai),
        "tabs" | "tab control" | "control panel" | "fleet" | "monitor" | "fleet monitor"
        | "system monitor" => Some(SettingsRoute::Tabs),
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

/// Handles `settings ...`: open, close or toggle the Settings tab, show a section, or set or
/// unset an `aterm.toml` key.
pub(crate) fn cmd_settings_overlay(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
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
    if word == "card" {
        let clean = match tail {
            "" => false,
            "clean" => true,
            _ => return "ERR usage: settings card [clean]\n".to_string(),
        };
        return match call_main(proxy, |tx| Wake::SettingsCard { clean, reply: tx }) {
            Ok(Ok(lines)) => {
                let mut out = format!("OK {}\n", lines.len());
                for line in lines {
                    out.push_str(&line);
                    out.push('\n');
                }
                out
            }
            Ok(Err(e)) => format!("ERR {e}\n"),
            Err(e) => format!("ERR {e}\n"),
        };
    }
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
                 section <name> | set <key> <value…> | unset <key> | card [clean])\n"
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

/// Returns a pre-hop refusal for a retired `invoke <name>`, or for a person-only action
/// ([`crate::menu::InvokeAuthority::PersonOnly`]) without the person stand-in.
pub(crate) fn invoke_refusal(name: &str) -> Option<String> {
    // A retired name answers with what to use instead, before any hop.
    if let Some(instead) = crate::menu::retired_invoke_name(name) {
        return Some(format!("ERR retired: {instead}\n"));
    }
    let action = crate::menu::MenuAction::from_invoke_name(name)?;
    (action.invoke_authority() == crate::menu::InvokeAuthority::PersonOnly
        && !person_invoke_act(name))
    .then(|| {
        let at = match action {
            crate::menu::MenuAction::ToggleStatsLens if cfg!(target_os = "macos") => {
                "View \u{25b8} Stats Lens, \u{2318}I"
            }
            crate::menu::MenuAction::ToggleStatsLens => "View \u{25b8} Stats Lens",
            crate::menu::MenuAction::RecordUnseenKeys => {
                "View \u{25b8} Record Keys Not Echoed\u{2026}, then aterm's alert"
            }
            crate::menu::MenuAction::RenameMachine => {
                "the machine menu \u{25b8} Rename This Mac\u{2026}; from a shell, `aterm name set`"
            }
            _ => "Agents \u{25b8} Take Over This Tab / Hand Back This Tab",
        };
        format!("ERR denied: {name} is the person's own act at the window ({at})\n")
    })
}

/// Returns whether `invoke <name>` counts as a user-initiated action in development builds: a
/// person-only action, or a copy or paste ([`crate::menu::InvokeAuthority::ClipboardWrite`]),
/// whose clipboard is the person's own, under `ATERM_DEBUG_PERSON_INVOKE=1`.
pub(crate) fn person_invoke_act(name: &str) -> bool {
    use crate::menu::InvokeAuthority;
    person_seam_armed()
        && crate::menu::MenuAction::from_invoke_name(name.trim()).is_some_and(|action| {
            matches!(
                action.invoke_authority(),
                InvokeAuthority::PersonOnly | InvokeAuthority::ClipboardWrite
            )
        })
}

fn person_seam_armed() -> bool {
    static SEAM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEAM.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEBUG_PERSON_INVOKE").is_some_and(|value| value == "1")
    })
}

const PERSON_BUILD: bool = cfg!(any(test, debug_assertions));

pub(crate) const PERSON_NEEDS_DEBUG: &str = "person needs a debug build";

pub(crate) const PERSON_DENIED: &str = "denied: person is a development build's stand-in for \
     the person at the window (ATERM_DEBUG_PERSON_INVOKE=1 at launch)";

const PERSON_USAGE: &str =
    "usage: person look <window>|off | person menubar press <at> | person menubar click";

/// Parsed `person` control request for development-build UI simulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersonAsked {
    Look(Option<u64>),
    Press(crate::status_item::MenuAt),
    /// A left click on the menu-bar lock.
    Click,
}

/// Parses a `person` control command when development simulation is built and enabled.
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
        ["menubar", "click"] => Ok(PersonAsked::Click),
        _ => Err(PERSON_USAGE.to_string()),
    }
}

/// Handles `person look|menubar ...` in development builds.
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

/// Handles `invoke <action>`, firing a named `MenuAction` on the main thread.
pub(crate) fn cmd_invoke(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    by: crate::session_store::ExitActor,
    typist: Option<std::sync::Arc<crate::typed_by::Caller>>,
) -> String {
    let name = rest.trim();
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return "ERR usage: invoke <action> (list actions with `controls menu`)\n".to_string();
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

/// Handles `confirm yes|no`, answering a pending close/quit confirmation sheet.
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

/// Handles `rain [status|on|off|toggle]`, reading or toggling the matrix-rain effect.
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

pub(crate) const THEME_USAGE: &str = "usage: theme [status] | theme list | theme preview <name> \
                                      [ttl=<secs>] | theme revert | theme commit | theme set \
                                      <value> | theme palette [session] | theme session [status] \
                                      | theme session set <name> | theme session clear";

/// Parsed `theme` control verb (`Op` on the main thread or durable `Set`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeForm {
    Op(crate::control_theme::ThemeCtlOp),
    Set(String),
}

/// Parses the argument tail of the `theme` control verb.
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

/// Handles `theme ...`, querying, previewing, or persisting theme settings.
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

/// Parses the argument tail of the `fx` control verb into an [`crate::FxCtlOp`].
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
            "bars" => match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<u8>().ok())
                .flatten()
            {
                Some(b) if (1..=CELEBRATE_MAX_BARS).contains(&b) => bars.replace(b).is_some(),
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

/// Handles `fx [status|celebrate ...]`, reading or triggering a one-shot celebration effect.
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

/// Handles `streak [status]`, reporting the front window's output-streak state.
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

/// Handles `tone [status]`, reporting the front window's typing-tone state.
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

/// Parsed `trail` control query (`Status`, `Light`, or `Admissions(n)`: the last `n` admission
/// records, all when `n` is omitted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailForm {
    Admissions(Option<usize>),
    Status,
    /// The front window's light latch (`docs/design/PRISM-GLASS.md` §15.2 1.3).
    Light,
}

/// Parses the argument tail of the `trail` control verb.
pub(crate) fn parse_trail_form(rest: &str) -> Result<TrailForm, String> {
    match rest.trim() {
        "" => Ok(TrailForm::Admissions(None)),
        "status" => Ok(TrailForm::Status),
        "light" => Ok(TrailForm::Light),
        n => n
            .parse::<usize>()
            .map(|n| TrailForm::Admissions(Some(n)))
            .map_err(|_| "usage: trail [status|light|<n>]".to_string()),
    }
}

/// Handles `trail [status|light|<n>]`, reporting cursor-trail diagnostics for the focused window.
pub(crate) fn cmd_trail(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let count = match parse_trail_form(rest) {
        Ok(TrailForm::Admissions(count)) => count,
        Ok(TrailForm::Status) => {
            return match call_main(proxy, |tx| Wake::TrailStatus { reply: tx }) {
                Ok(Ok(msg)) => format!("OK 1\n{msg}\n"),
                Ok(Err(e)) => format!("ERR {e}\n"),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Ok(TrailForm::Light) => {
            return match call_main(proxy, |tx| Wake::TrailLight { reply: tx }) {
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

/// Handles `kitty [wear <key>]`, listing or equipping cursor-cat wearables.
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

/// Handles `@<sid> status`, returning the session's classified status key-value line.
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

/// Splits `rest` into whitespace-separated `key=value` tokens, honoring double-quoted values.
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

/// `spawn`'s usage reply, naming the worker words a spawn starts
/// ([`aterm_agent::harness::worker::started_words`]) and a program worker's form
/// ([`crate::program_worker`]).
pub(crate) fn spawn_usage() -> String {
    format!(
        "ERR usage: spawn [window=<id>] [raise=<t|f>] [cwd=<path>] [split=<v|h>] [theme=<name>] \
         [why=<one line>] [req=<id>] [connected=controlled|controller place=window|tab of=<sid> | agent=<{}> [model=<m>] \
         [autopilot=<land|cruise>] [from=<sid>] len=<n> | program=<pct> [arg=<pct>]... \
         [from=<sid>] len=<n>]\n",
        aterm_agent::harness::worker::started_words()
    )
}

/// `spawn`'s usage reply to a line it refuses, naming the word of it `refused` where one word
/// breaks it ([`parse_spawn_args`]), as one line of its first 32 characters.
pub(crate) fn spawn_refusal(refused: Option<&str>) -> String {
    let usage = spawn_usage();
    match refused {
        Some(word) => format!(
            "{}; not `{}`\n",
            usage.trim_end(),
            aterm_codec::tty::one_line(word, 32)
        ),
        None => usage,
    }
}

/// Parsed worker specification for `spawn`: an agent's (`agent=<word>`) or a program's
/// (`program=<p> [arg=<a>]…`, [`crate::program_worker`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentSpec {
    Worker {
        agent: aterm_phase::Program,
        model: Option<String>,
        autopilot: Option<aterm_agent::supervise::Autopilot>,
        from: Option<aterm_session::SessionId>,
    },
    Program {
        run: crate::program_worker::ProgramRun,
        from: Option<aterm_session::SessionId>,
    },
}

impl AgentSpec {
    /// The word a `kind=spawn` telemetry row names the worker by: its agent's, or `program`.
    const fn word(&self) -> &'static str {
        match self {
            Self::Worker { agent, .. } => agent.name(),
            Self::Program { .. } => "program",
        }
    }
}

/// A `program=` or `arg=` value as the wire spells it, percent-decoded: `None` for one holding a
/// control character or a NUL, which no line types. A `program=` is also refused empty, as an
/// option (`-…`) and starting `~`, which a quoted line never expands.
fn program_word(value: &str, program: bool) -> Option<String> {
    let decoded = aterm_codec::pct::decode(value);
    let typed = !decoded
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'));
    let named = !program || !(decoded.is_empty() || decoded.starts_with(['-', '~']));
    (typed && named).then_some(decoded)
}

fn agent_spec(word: &str) -> Option<AgentSpec> {
    use aterm_phase::Program;
    let program = Program::ALL.into_iter().find(|p| p.name() == word)?;
    aterm_agent::harness::worker::worker_cli(program).map(|_| AgentSpec::Worker {
        agent: program,
        model: None,
        autopilot: None,
        from: None,
    })
}

fn valid_model(model: &str) -> bool {
    (1..=64).contains(&model.len())
        && !model.starts_with('-')
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

const TAB_USAGE: &str = "ERR usage: tab <new|N|next|prev|close [N]|move <from> <to>>\n";

/// A `spawn` line's form ([`parse_spawn_args`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SpawnForm {
    Plain {
        window: Option<u64>,
        raise: Option<bool>,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
        theme: Option<String>,
        agent: Option<AgentSpec>,
        /// The request's one line (`why=`, [`aterm_types::control_verbs::request_why`]), which
        /// the tab's open records as `why_text=`.
        why: Option<String>,
        /// The request this spawn's tab joins (`req=`, [`aterm_types::control_verbs::request_id`]),
        /// one its requester opened (`crate::requests_host::prepare`).
        req: Option<String>,
    },
    Connected {
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: aterm_session::SessionId,
        cwd: Option<String>,
        theme: Option<String>,
    },
}

/// `spawn`'s arguments as one form, or the word of them refused: `Err(Some(word))` names the
/// token a refusal breaks on (a repeated `theme=` or `autopilot=` names the repeat), and
/// `Err(None)` a line no one word breaks (a quote left open, a worker's option with no `agent=`,
/// a connected form left incomplete or mixed with a plain one).
pub(crate) fn parse_spawn_args(rest: &str) -> Result<SpawnForm, Option<String>> {
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
    let (mut cwd, mut split, mut kind, mut place, mut origin) = (None, None, None, None, None);
    let (mut window, mut raise, mut theme) = (None, None, None);
    let (mut agent, mut model, mut from, mut framed) = (None, None, None, false);
    let (mut autopilot, mut why, mut req) = (None, None, None);
    let (mut program, mut args) = (None, Vec::new());
    let Some(tokens) = split_quoted_tokens(rest) else {
        return Err(None);
    };
    for tok in tokens {
        let refused = || Some(tok.clone());
        if let Some(v) = tok.strip_prefix("cwd=") {
            cwd = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("theme=") {
            // A name or a `dark:…,light:…` split, quoted when it holds spaces;
            // validated on the main thread against the catalog. Empty, or given
            // twice, is the usage line.
            if v.is_empty() || theme.replace(v.to_string()).is_some() {
                return Err(refused());
            }
        } else if let Some(v) = tok.strip_prefix("window=") {
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(refused());
            }
            window = Some(v.parse::<u64>().map_err(|_| refused())?);
        } else if let Some(v) = tok.strip_prefix("raise=") {
            raise = Some(match v {
                "t" | "true" => true,
                "f" | "false" => false,
                _ => return Err(refused()),
            });
        } else if let Some(v) = tok.strip_prefix("split=") {
            split = Some(match v {
                "v" | "vertical" => crate::pane::SplitDir::Vertical,
                "h" | "horizontal" => crate::pane::SplitDir::Horizontal,
                _ => return Err(refused()),
            });
        } else if let Some(v) = tok.strip_prefix("connected=") {
            kind = Some(match v {
                "controlled" => ConnectedSpawnKind::Controlled,
                "controller" => ConnectedSpawnKind::Controller,
                _ => return Err(refused()),
            });
        } else if let Some(v) = tok.strip_prefix("place=") {
            place = Some(match v {
                "window" => ConnectedSpawnPlace::Window,
                "tab" => ConnectedSpawnPlace::Tab,
                _ => return Err(refused()),
            });
        } else if let Some(v) = tok.strip_prefix("of=") {
            // A bare sid, as `from=` and a hold's argument name one.
            if v.is_empty() || v.starts_with('@') {
                return Err(refused());
            }
            origin = Some(aterm_session::SessionId::new(v));
        } else if let Some(v) = tok.strip_prefix("agent=") {
            if program.is_some() {
                return Err(refused());
            }
            agent = Some(agent_spec(v).ok_or_else(refused)?);
        } else if let Some(v) = tok.strip_prefix("program=") {
            if agent.is_some() || program.is_some() {
                return Err(refused());
            }
            program = Some(program_word(v, true).ok_or_else(refused)?);
        } else if let Some(v) = tok.strip_prefix("arg=") {
            args.push(program_word(v, false).ok_or_else(refused)?);
        } else if let Some(v) = tok.strip_prefix("model=") {
            if !valid_model(v) {
                return Err(refused());
            }
            model = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("autopilot=") {
            let word = match v {
                "land" => aterm_agent::supervise::Autopilot::Land,
                "cruise" => aterm_agent::supervise::Autopilot::Cruise,
                _ => return Err(refused()),
            };
            if autopilot.replace(word).is_some() {
                return Err(refused());
            }
        } else if let Some(v) = tok.strip_prefix("from=") {
            // A session id, as a task record's `from=` reads one.
            if v == crate::task_record::FROM_CTL || !crate::task_record::valid_from(v) {
                return Err(refused());
            }
            from = Some(aterm_session::SessionId::new(v));
        } else if let Some(v) = tok.strip_prefix("why=") {
            let said = aterm_types::control_verbs::request_why(v).map_err(|_| refused())?;
            if why.replace(said.to_owned()).is_some() {
                return Err(refused());
            }
        } else if let Some(v) = tok.strip_prefix("req=") {
            let id = aterm_types::control_verbs::request_id(v).map_err(|_| refused())?;
            if req.replace(id.to_owned()).is_some() {
                return Err(refused());
            }
        } else if tok.starts_with("len=") {
            framed = true;
        } else {
            return Err(refused());
        }
    }
    let agent = match (agent, program) {
        (Some(AgentSpec::Worker { agent, .. }), None) if args.is_empty() => {
            Some(AgentSpec::Worker {
                agent,
                model,
                autopilot,
                from,
            })
        }
        // A program worker has no model and no Autopilot: no harness of its reads either.
        (None, Some(program)) if model.is_none() && autopilot.is_none() => {
            Some(AgentSpec::Program {
                run: crate::program_worker::ProgramRun { program, args },
                from,
            })
        }
        (None, None)
            if model.is_none()
                && autopilot.is_none()
                && from.is_none()
                && !framed
                && args.is_empty() =>
        {
            None
        }
        _ => return Err(None),
    };
    match (kind, place, origin) {
        (None, None, None) => Ok(SpawnForm::Plain {
            window,
            raise,
            cwd,
            split,
            theme,
            agent,
            why,
            req,
        }),
        (Some(kind), Some(place), Some(origin))
            if split.is_none()
                && window.is_none()
                && raise.is_none()
                && agent.is_none()
                && why.is_none()
                && req.is_none() =>
        {
            Ok(SpawnForm::Connected {
                kind,
                place,
                origin,
                cwd,
                theme,
            })
        }
        _ => Err(None),
    }
}

fn spawn_cwd_refusal(form: &SpawnForm) -> Option<String> {
    use crate::spawn_folder::Fault;
    let (SpawnForm::Plain { cwd, .. } | SpawnForm::Connected { cwd, .. }) = form;
    let cwd = cwd.as_deref()?;
    let shown = if cwd.is_empty() { "\"\"" } else { cwd };
    match crate::spawn_folder::folder_fault(cwd)? {
        Fault::Missing => Some(format!("ERR no such folder {shown}\n")),
        Fault::Shut => Some(format!("ERR no access to folder {shown}\n")),
        Fault::Unresponsive | Fault::HomeUnresponsive => None,
    }
}

/// Target window selection for an aimed `spawn` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnAim {
    Front,
    Window(u64),
    Session(u64),
}

impl SpawnAim {
    const fn from_request(window: Option<u64>, session: Option<u64>) -> Self {
        match (window, session) {
            (Some(id), _) => Self::Window(id),
            (None, Some(local)) => Self::Session(local),
            (None, None) => Self::Front,
        }
    }

    /// Returns whether the caller explicitly specified a target window.
    pub(crate) const fn is_aimed(self) -> bool {
        !matches!(self, Self::Front)
    }
}

/// Returns whether a newly spawned tab should raise its host window.
pub(crate) const fn raise_after_spawn(window_named: bool, raise: Option<bool>) -> bool {
    match raise {
        Some(explicit) => explicit,
        None => !window_named,
    }
}

/// Whether a spawned tab lands in front: it was asked to (`raise`, [`raise_after_spawn`]) and
/// the desk door admits its client (`in_front`, read only then, since it hops to the main
/// thread).
pub(crate) fn lands_in_front(raise: bool, in_front: &dyn Fn() -> bool) -> bool {
    raise && in_front()
}

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

/// The form of a `spawn ...` line, or its refusal: the usage line
/// naming the word it refuses ([`spawn_refusal`]), else a program a harness here concludes
/// ([`spawn_harnessed_refusal`]), else a folder that is not there.
fn spawn_form(rest: &str) -> Result<SpawnForm, String> {
    let form = parse_spawn_args(rest).map_err(|refused| spawn_refusal(refused.as_deref()))?;
    if let Some(refusal) = spawn_harnessed_refusal(&form) {
        return Err(refusal);
    }
    spawn_cwd_refusal(&form).map_or(Ok(form), Err)
}

/// The refusal of a program worker whose program is a CLI `spawn agent=` starts
/// ([`aterm_agent::harness::worker::harness_of_program`]): under the program form its harness would
/// supervise nothing of its task (a harness's mark on a program worker's task is refused,
/// [`crate::task_record::PROGRAM_MARK_BY_HARNESS`]), so the reply names the agent form, under
/// which it does.
fn spawn_harnessed_refusal(form: &SpawnForm) -> Option<String> {
    let SpawnForm::Plain {
        agent: Some(AgentSpec::Program { run, .. }),
        ..
    } = form
    else {
        return None;
    };
    let cli = aterm_agent::harness::worker::harness_of_program(&run.program)?;
    Some(format!(
        "ERR spawn {}: {} has a harness here; spawn it as `agent={}`\n",
        crate::task_launch::program_form(&run.program),
        cli.agent.product(),
        cli.word
    ))
}

/// `spawn rest`, answered, with its `kind=spawn` telemetry row: the worker's program (`shell` for
/// a plain tab), its folder, and how long the verb took to answer.
pub(crate) fn cmd_spawn_metered(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    session: Option<u64>,
    by: crate::session_store::ExitActor,
    in_front: &dyn Fn() -> bool,
    asker: Option<crate::requests_host::Asker>,
) -> String {
    let began = std::time::Instant::now();
    let reply = cmd_spawn(proxy, rest, session, by, in_front, asker.as_ref());
    let (agent, cwd) = match parse_spawn_args(rest) {
        Ok(SpawnForm::Plain { agent, cwd, .. }) => {
            (agent.as_ref().map_or("shell", AgentSpec::word), cwd)
        }
        Ok(SpawnForm::Connected { cwd, .. }) => ("connected", cwd),
        Err(_) => ("-", None),
    };
    crate::telemetry_host::spawned(
        agent,
        cwd.as_deref(),
        u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
        &reply,
    );
    reply
}

/// A worker's `order` as the spawn that served it over `driver`'s fleet drive gives it: a manager
/// it names (`from=`) runs on the driving Mac ([`crate::task_launch::TaskOrder::manager_node`]);
/// a spawn made on this Mac, or one that names no manager, names no Mac.
fn under_drive(
    order: crate::task_launch::TaskOrder,
    driver: Option<crate::fabric::Driver>,
) -> crate::task_launch::TaskOrder {
    crate::task_launch::TaskOrder {
        manager_node: order.caller.as_ref().and(driver).map(|driver| driver.node),
        ..order
    }
}

/// Handles `spawn ...`, creating a new terminal session or worker on the main thread; `by` the
/// caller its tab's open names (`crate::control::control_session::caller_actor_of`). A tab that
/// would land in front ([`raise_after_spawn`]) does so only for a client the desk door admits
/// (`in_front`, [`crate::desk_door::admit`], asked only then): for one whose own tab is behind, it
/// opens behind what the window shows, as `raise=false` asks for, and the spawn goes on. `asker`
/// is who asks where a request is made for it (`crate::requests_host::asker`): its request is
/// opened, or joined (`req=`), before the spawn reaches the main thread, and the reply names it
/// (`OK <sid> req=<id>`).
pub(crate) fn cmd_spawn(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    session: Option<u64>,
    by: crate::session_store::ExitActor,
    in_front: &dyn Fn() -> bool,
    asker: Option<&crate::requests_host::Asker>,
) -> String {
    use crate::requests_host::Made;
    let form = match spawn_form(rest) {
        Ok(form) => form,
        Err(refusal) => return refusal,
    };
    let (sent, receipt) = match form {
        SpawnForm::Plain {
            window,
            raise,
            cwd,
            split,
            theme,
            agent,
            why,
            req,
        } => {
            let frame = agent
                .as_ref()
                .and_then(|_| crate::task_launch::take_frame());
            let made = match &agent {
                None => Made::Shell,
                Some(AgentSpec::Worker { agent, .. }) => Made::Agent(agent.name()),
                Some(AgentSpec::Program { .. }) => Made::Program,
            };
            let prepared = match crate::requests_host::prepare(
                asker,
                req.as_deref(),
                made,
                why.as_deref(),
                frame.as_deref(),
            ) {
                Ok(prepared) => prepared,
                Err(refusal) => return refusal,
            };
            let task = match agent {
                None => None,
                Some(AgentSpec::Worker {
                    agent,
                    model,
                    autopilot,
                    from,
                }) => match worker_order(frame, agent, model, autopilot, from, cwd.as_deref()) {
                    Ok(order) => Some(order),
                    Err(refusal) => return refusal,
                },
                Some(AgentSpec::Program { run, from }) => {
                    match crate::task_launch::TaskOrder::of_program_frame(
                        frame,
                        run,
                        cwd.as_deref(),
                        from,
                    ) {
                        Ok(order) => Some(order),
                        Err(refusal) => return refusal,
                    }
                }
            };
            let task = task.map(|order| under_drive(order, crate::fabric::serving_driver()));
            let fleet = crate::fabric::serving_requester();
            let shell = task
                .is_none()
                .then(crate::fabric::serving_driver)
                .flatten()
                .map(|driver| driver.node);
            let aim = SpawnAim::from_request(window, session);
            let raise = lands_in_front(raise_after_spawn(aim.is_aimed(), raise), in_front);
            let begun = match crate::requests_host::begin(prepared) {
                Ok(begun) => begun,
                Err(refusal) => return refusal,
            };
            let (request, receipt) = crate::requests_host::hand(begun, cwd.as_deref());
            let sent = call_main(proxy, |tx| Wake::SpawnSession {
                aim,
                cwd,
                split,
                raise,
                theme,
                task,
                shell,
                fleet,
                why,
                by,
                reply: tx,
                request,
            });
            (sent, receipt)
        }
        SpawnForm::Connected {
            kind,
            place,
            origin,
            cwd,
            theme,
        } => {
            let begun =
                match crate::requests_host::prepare(asker, None, Made::Connected, None, None)
                    .and_then(crate::requests_host::begin)
                {
                    Ok(begun) => begun,
                    Err(refusal) => return refusal,
                };
            let (request, receipt) = crate::requests_host::hand(begun, cwd.as_deref());
            let sent = call_main(proxy, |tx| Wake::SpawnConnectedSession {
                kind,
                place,
                origin,
                cwd,
                theme,
                by,
                reply: tx,
                request,
            });
            (sent, receipt)
        }
    };
    // The main thread settled the request as the spawn answered: its tab, or its refusal. A
    // reply that came too late here leaves that to the main thread's answer.
    let req = match &sent {
        Ok(Ok(_)) => receipt.and_then(crate::requests_host::Receipt::named),
        Ok(Err(_)) | Err(_) => None,
    };
    match (sent, req) {
        (Ok(Ok(sid)), Some(req)) => format!("OK {sid} req={req}\n"),
        (Ok(Ok(sid)), None) => format!("OK {sid}\n"),
        (Ok(Err(e)), _) => format!("ERR {e}\n"),
        (Err(e), _) => format!("ERR {e}\n"),
    }
}

/// Handles `@<sid> tab ...`, driving tabs in the window hosting `session`.
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
        Ok(Ok((active, count))) => super::control_input::tab_state_reply(active, count),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(error) => format!("ERR tab command failed: {error}\n"),
    }
}

/// Reason this instance refuses to launch a spawned worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerBlock {
    Unhosted,
    Unsupervised(&'static str),
    /// Nothing would see the worker's task end
    /// ([`aterm_agent::harness::worker::WorkerCli::end_unseen`]).
    Unseen(String),
    NoIntegration,
    Shell(String),
    OldFish,
}

impl WorkerBlock {
    /// Formats the wire `ERR ...` line for this worker block reason, naming the spawn's `form`
    /// (`agent=<word>` or `program=<p>`, [`crate::task_launch::spawn_form_word`]).
    pub(crate) fn refusal(self, form: &str) -> String {
        match self {
            Self::Unhosted => {
                "unsupervised: the aterm of the Mac asked to start it runs no window harness \
                 (no control socket)"
                    .to_string()
            }
            Self::Unsupervised(switch) => format!("unsupervised: {switch}"),
            Self::Unseen(why) => format!("spawn {form}: {why}"),
            Self::NoIntegration => {
                format!("spawn {form}: shell integration is off, so no prompt is marked to type at")
            }
            Self::Shell(name) => {
                format!("spawn {form}: the shell {name} is not zsh, bash or fish")
            }
            Self::OldFish => format!(
                "spawn {form}: the shell is a fish older than 3.4, which cannot take the \
                 task's operand"
            ),
        }
    }
}

/// Prechecked shell dialect and environment needed to launch a spawned worker.
pub(crate) struct WorkerStart {
    order: crate::task_launch::TaskOrder,
    from: String,
    caller: Option<u64>,
    shell: std::path::PathBuf,
    dialect: aterm_agent::harness::upgrade::Dialect,
}

impl WorkerStart {
    fn seed(&self) -> crate::task_launch::TaskSeed {
        let from = self.order.fact_line.then_some(self.from.as_str());
        crate::task_launch::TaskSeed::new(crate::task_launch::task_text(
            &self.order.task,
            from,
            self.order.autopilot,
            self.order.agent,
        ))
    }
}

/// Main-thread outcome of spawning a session, paired with any pending worker launch.
pub(crate) struct Spawned {
    sid: String,
    worker: Option<PendingWorker>,
}

#[cfg(test)]
impl Spawned {
    /// A plain session `sid` spawned with no worker.
    pub(crate) const fn plain_for_test(sid: String) -> Self {
        Self { sid, worker: None }
    }
}

/// A fleet driver's plain shell `landed`: recorded as `node`'s and marked `ctl spawn`'s before
/// its `OK <sid>` goes out, or refused when the person reached it first.
pub(crate) fn fleet_shell_landed(
    store: &crate::session_store::Store,
    landed: Result<String, String>,
    node: &str,
) -> Result<String, String> {
    landed.and_then(|sid| {
        if crate::control::record_driven_shell(store, &sid, node) {
            crate::control::mark_ctl_spawned(store, &sid);
            Ok(sid)
        } else {
            Err("the plain shell was adopted before ownership could be recorded".to_owned())
        }
    })
}

/// Spawned worker session waiting for its launcher thread to start.
pub(crate) struct PendingWorker {
    launch: crate::task_launch::Launch,
    seed: crate::task_launch::TaskSeed,
}

/// Placement options (`cwd`, `split`, `raise`, `admission`) for [`crate::App::book_spawn`].
pub(crate) struct SpawnBooking {
    pub(crate) cwd: Option<String>,
    pub(crate) split: Option<crate::pane::SplitDir>,
    pub(crate) raise: bool,
    pub(crate) admission: crate::tab_spawn::SpawnAdmission,
    /// Asked over a fleet drive ([`crate::tab_spawn::SpawnAsk::fleet`]).
    pub(crate) fleet: bool,
}

/// Validated spawn parameters resolved on the main thread prior to booking.
pub(crate) struct SpawnPlan {
    owner: crate::WindowId,
    theme: Option<crate::session_theme::SessionTheme>,
    worker: Option<WorkerStart>,
}

impl SpawnPlan {
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

/// Continuation invoked on the main thread once a booked spawn completes.
pub(crate) type SpawnTaken = Box<dyn FnOnce(&mut crate::App, Result<Spawned, String>)>;

/// Reply channel for a main-thread `tab` command's `(active_index, tab_count)` result.
pub(crate) type TabCmdReply = std::sync::mpsc::Sender<Result<(usize, usize), String>>;

/// `ask` as `booking` places it. A worker that parks lands behind what the window shows, whatever
/// `raise=` says about the window, so nothing the person types is held for it either.
pub(crate) fn placed_ask(
    ask: crate::tab_spawn::SpawnAsk,
    booking: SpawnBooking,
) -> crate::tab_spawn::SpawnAsk {
    let SpawnBooking {
        cwd,
        split,
        raise,
        admission,
        fleet,
    } = booking;
    let ask = crate::tab_spawn::SpawnAsk {
        admission,
        cwd,
        fleet,
        ..ask
    };
    let parked = ask.parks() && split.is_none();
    crate::tab_spawn::SpawnAsk {
        background: (!raise || parked) && split.is_none(),
        unasked: ask.unasked || parked,
        refuses_worker_split: split.is_some(),
        ..ask
    }
}

impl crate::App {
    /// Resolves a [`SpawnAim`] to a live [`crate::WindowId`], or `Ok(None)` for the front window.
    pub(crate) fn aimed_window(&self, aim: SpawnAim) -> Result<Option<crate::WindowId>, String> {
        match aim {
            SpawnAim::Front => Ok(None),
            SpawnAim::Window(id) => self.named_window(id).map(Some),
            SpawnAim::Session(local) => self.hosting_window(local).map(Some),
        }
    }

    fn named_window(&self, id: u64) -> Result<crate::WindowId, String> {
        let wid = crate::WindowId(id);
        if self.windows.contains_key(&wid) {
            Ok(wid)
        } else {
            Err(format!("no such window {id}"))
        }
    }

    fn hosting_window(&self, local: u64) -> Result<crate::WindowId, String> {
        self.window_of_session(local)
            .ok_or_else(|| "no window hosts the target session".to_string())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the spawn verb's arguments as the control thread parsed them — aim, cwd, \
                  split, raise, theme and a worker's task — and `reply` its answer; \
                  a struct would only rename the Wake's fields"
    )]
    /// Handles `Wake::SpawnSession`: validates the theme, aim and worker, then books the tab or
    /// split off the event loop and replies once it lands.
    pub(crate) fn spawn_session_aimed(
        &mut self,
        aim: SpawnAim,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
        raise: bool,
        theme: Option<String>,
        task: Option<crate::task_launch::TaskOrder>,
        shell: Option<String>,
        fleet: bool,
        reply: crate::tab_spawn::SpawnReply,
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
            fleet,
        };
        match self.resolve_spawn(aim, task) {
            Ok(SpawnPlan {
                owner,
                worker: None,
                ..
            }) => {
                let ask = SpawnPlan::ask(theme, None);
                let (ask, then) = if let Some(node) = shell {
                    let then = crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| {
                        let _ = reply.send(fleet_shell_landed(&app.store, landed, &node));
                    }));
                    (ask.for_fleet_shell(), then)
                } else {
                    (ask, crate::tab_spawn::TabSpawnThen::Reply { reply, raise })
                };
                self.place_spawn(owner, ask, booking, then);
            }
            Ok(mut plan) => {
                plan.theme = theme;
                self.book_spawn(
                    plan,
                    booking,
                    Box::new(move |app, spawned| app.answer_spawn(spawned, reply)),
                );
            }
            Err(why) => {
                let _ = reply.send(Err(why));
            }
        }
    }

    /// Validates a `theme=` selector for `spawn`.
    pub(crate) fn spawn_theme(
        &self,
        theme: Option<String>,
    ) -> Result<Option<crate::session_theme::SessionTheme>, String> {
        let Some(selector) = theme else {
            return Ok(None);
        };
        if let Some(why) = self.session_theme_unavailable(&selector) {
            return Err(format!("theme {selector:?}: {why}"));
        }
        Ok(Some(crate::session_theme::SessionTheme {
            selector,
            origin: crate::session_theme::SessionThemeOrigin::Spawn,
        }))
    }

    /// Resolves the host window and, for a worker, checks that this instance can launch it,
    /// before booking a spawn.
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

    /// Books an asynchronous session or worker spawn off the event loop and invokes `taken` on
    /// completion.
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

    fn place_spawn(
        &mut self,
        owner: crate::WindowId,
        ask: crate::tab_spawn::SpawnAsk,
        booking: SpawnBooking,
        then: crate::tab_spawn::TabSpawnThen,
    ) {
        let split = booking.split;
        let ask = placed_ask(ask, booking);
        match split {
            None => self.open_off_loop(owner, crate::tab_spawn::SpawnPlace::Tab, ask, then),
            Some(_) if self.worker_on_show(owner) => {
                let why = crate::tab_spawn::WORKER_SPLIT_REFUSED.to_string();
                then.answer(self, owner, Err(why));
            }
            Some(dir) => {
                let _ = self.split_focused_pane_in_window(owner, dir, ask, then);
            }
        }
    }

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

    /// Sends the `OK <sid>` spawn reply to `reply` and starts the worker launcher if the client
    /// received it.
    pub(crate) fn answer_spawn(
        &mut self,
        spawned: Result<Spawned, String>,
        reply: crate::tab_spawn::SpawnReply,
    ) {
        let Spawned { sid, worker } = match spawned {
            Ok(spawned) => spawned,
            Err(why) => {
                let _ = reply.send(Err(why));
                return;
            }
        };
        crate::control::mark_ctl_spawned(&self.store, &sid);
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

    /// Starts a worker spawned internally by the main thread (such as Summarize in a New Tab).
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

    /// Checks whether this instance can launch a worker for `agent` and returns its shell dialect;
    /// an agent no Mac starts is refused as that before anything of this one is asked.
    pub(crate) fn worker_shell(
        &self,
        agent: aterm_phase::Program,
    ) -> Result<(std::path::PathBuf, aterm_agent::harness::upgrade::Dialect), WorkerBlock> {
        if let Some(why) =
            aterm_agent::harness::worker::worker_cli(agent).and_then(|cli| cli.end_unseen())
        {
            return Err(WorkerBlock::Unseen(why));
        }
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
        let (shell, dialect) = self.program_shell()?;
        if dialect == aterm_agent::harness::upgrade::Dialect::Fish
            && crate::task_launch::fish_known_old(&shell)
        {
            return Err(WorkerBlock::OldFish);
        }
        Ok((shell, dialect))
    }

    /// Checks whether this instance can launch a program worker ([`crate::program_worker`]) and
    /// returns its shell dialect: a hosted harness that supervises ([`harness_unsupervised`]),
    /// whose looks reap its tab and whose startup deadline stalls a line that never ran, and a
    /// shell whose integration marks its prompt and its program's exit status. No agent's switch
    /// is read, since no harness supervises the program, and any fish takes its line, which reads
    /// the task as a redirection.
    ///
    /// [`harness_unsupervised`]: crate::harness_host::harness_unsupervised
    pub(crate) fn program_shell(
        &self,
    ) -> Result<(std::path::PathBuf, aterm_agent::harness::upgrade::Dialect), WorkerBlock> {
        if self.harness.is_none() {
            return Err(WorkerBlock::Unhosted);
        }
        if let Some(switch) = crate::harness_host::harness_unsupervised(
            &self.harness_policy.get().policy,
            self.headless,
        ) {
            return Err(WorkerBlock::Unsupervised(switch));
        }
        if !self.session_factory.integrate {
            return Err(WorkerBlock::NoIntegration);
        }
        let shell =
            crate::spawn::interactive_shell_program(self.session_factory.shell_override.as_deref());
        let dialect = crate::task_launch::worker_dialect(&shell).map_err(WorkerBlock::Shell)?;
        Ok((shell, dialect))
    }

    fn worker_precheck(&self, order: crate::task_launch::TaskOrder) -> Result<WorkerStart, String> {
        let (shell, dialect) = match &order.program {
            None => self
                .worker_shell(order.agent)
                .map_err(|block| block.refusal(&format!("agent={}", order.agent.name())))?,
            Some(run) => {
                let form = crate::task_launch::program_form(&run.program);
                let (shell, dialect) =
                    self.program_shell().map_err(|block| block.refusal(&form))?;
                aterm_agent::harness::worker::program_line(
                    dialect,
                    std::path::Path::new(&run.program),
                    &run.args,
                )
                .map_err(|why| format!("spawn {form}: its line cannot be written: {why}"))?;
                (shell, dialect)
            }
        };
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

    fn ready_worker(
        &mut self,
        sid: &str,
        worker: WorkerStart,
        seed: crate::task_launch::TaskSeed,
    ) -> Result<PendingWorker, String> {
        match self.start_worker(sid, worker) {
            Ok(launch) => Ok(PendingWorker { launch, seed }),
            Err(why) => {
                self.unspawn_worker(sid, Some(&seed));
                Err(why)
            }
        }
    }

    fn start_worker(
        &self,
        sid: &str,
        worker: WorkerStart,
    ) -> Result<crate::task_launch::Launch, String> {
        #[cfg(unix)]
        let account_scope = match worker.order.agent {
            aterm_phase::Program::Agy | aterm_phase::Program::Codex => Some(
                self.account_work
                    .enter()
                    .ok_or("account preparation is paused: a quit or an update closed it, and it opens again once earlier account work ends or aterm restarts")?,
            ),
            aterm_phase::Program::Claude
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Generic => None,
        };
        #[cfg(not(unix))]
        let account_scope = None;
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
        // A program worker's tab keeps the Autopilot it opened with: no harness of its reads it.
        if worker.order.program.is_none() {
            crate::session_timeline::apply_autopilot(
                &ctx,
                Some(worker.order.autopilot.word().to_string()),
                None,
                aterm_slog::Actor::Aterm,
                None,
            );
        }
        let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
        mark.from = Some(worker.from);
        crate::task_record::mark_session_task_within(&ctx, local, &mark, std::time::Duration::ZERO)
            .map_err(|_| "busy update-committing".to_string())?;
        {
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.set_task_worker(worker.order.agent, worker.order.trusted_cwd.clone());
            timeline.set_task_program(
                worker
                    .order
                    .program
                    .as_ref()
                    .and_then(crate::program_worker::ProgramRun::name),
            );
            // A manager that is a session here is here, whatever drive the spawn came over.
            timeline.set_task_manager_node(
                worker
                    .order
                    .manager_node
                    .clone()
                    .filter(|_| worker.caller.is_none()),
            );
        }
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
            caller,
            chrome: worker.order.chrome,
            approve: self.harness_policy.get().policy.approve,
            program: worker.order.program,
            account_scope,
            #[cfg(unix)]
            source_registry: crate::control::source_registry(),
        })
    }

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
                crate::session_store::ExitActor::Harness,
            );
            let _act = crate::tab_marks::ActScope::closing(
                crate::session_store::ExitActor::Harness,
                "unstarted",
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

    /// Executes `@<sid> tab ...` on the window hosting `session`.
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
            return self.tab_new_answered(wid, by, reply);
        }
        let state = wid.and_then(|wid| {
            self.tab_cmd_bracketed(el, by, |app| app.apply_tab_cmd_in(wid, action))
        });
        let _ = reply.send(state);
    }

    /// Executes an unaimed `tab ...` command on the front window.
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
            return self.tab_new_answered(front, by, reply);
        }
        let _ = reply.send(self.tab_cmd_bracketed(el, by, |app| app.apply_tab_cmd(action)));
    }

    /// A control client's `tab new` in `window`, by `by` (the caller
    /// `crate::control::control_session::caller_actor_of` names): its tab opens by that caller
    /// (`why=new-tab`), whose act the spawn's booking keeps until it lands.
    pub(crate) fn tab_new_answered(
        &mut self,
        window: Result<crate::WindowId, String>,
        by: crate::session_store::ExitActor,
        reply: TabCmdReply,
    ) {
        match window {
            Ok(wid) => {
                self.clear_tab_surface_move_license(None);
                let _act = crate::tab_marks::ActScope::enter(by, "new-tab");
                self.open_tab_off_loop(wid, None, crate::tab_spawn::TabSpawnThen::Counts(reply));
            }
            Err(why) => {
                let _ = reply.send(Err(why));
            }
        }
    }

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
        apply(self)
    }

    /// Executes `invoke <action>` with control-close attribution and a controller gesture.
    pub(crate) fn invoke_attributed(
        &mut self,
        by: crate::session_store::ExitActor,
        invoke: impl FnOnce(&mut Self) -> Result<String, String>,
    ) -> Result<String, String> {
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            by,
        );
        self.with_gesture_source(true, invoke)
    }
}

/// Validates that `@<sid> close` received no extra trailing arguments.
pub(crate) fn close_no_arg(rest: &str) -> Result<(), String> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err("ERR usage: close\n".to_string())
    }
}

/// Handles `@<sid> close`, closing the pane hosting `session`.
pub(crate) fn cmd_close(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    sid: &str,
    by: crate::session_store::ExitActor,
) -> String {
    match call_main(proxy, |tx| Wake::CloseSession {
        session,
        by,
        driver: crate::fabric::serving_driver().map(|driver| driver.node),
        reply: tx,
    }) {
        Ok(Ok(())) => format!("OK closed {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

impl crate::App {
    /// Final close admission on the event loop, after any time spent in the control queue.
    pub(crate) fn close_control_session(
        &mut self,
        session: u64,
        driver: Option<&str>,
    ) -> Result<crate::app_tabs::CloseProgress, String> {
        let admitted =
            driver.map(|node| crate::control::drive_close_admission(&self.store, session, node));
        match admitted {
            Some(None) => Err("drive no longer owns this untouched session".to_owned()),
            Some(Some(true)) if self.session_foreground_busy(session) => {
                Err("close refused (a running job in that pane)".to_owned())
            }
            _ => self.close_session_by_id(session),
        }
    }
}

/// Handles `chrome`, dumping the front window's native chrome (toolbar, menu bar or menu model),
/// the drawn autopilot chips and the status-item menu.
pub(crate) fn cmd_chrome(proxy: &EventLoopProxy<Wake>) -> String {
    let lines = match call_main(proxy, |tx| Wake::ReadChrome { reply: tx }) {
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

/// Handles `panes`, dumping the active tab's split-pane layout.
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
#[path = "control_media_trail_parse_tests.rs"]
mod trail_parse_tests;

#[cfg(test)]
#[path = "control_media_fx_parse_tests.rs"]
mod fx_parse_tests;

#[cfg(test)]
#[path = "control_media_spawn_parse_tests.rs"]
mod spawn_parse_tests;

#[cfg(test)]
#[path = "control_media_close_arg_tests.rs"]
mod close_arg_tests;

#[cfg(test)]
#[path = "control_media_wire_tab_close_guard_tests.rs"]
mod wire_tab_close_guard_tests;

#[cfg(test)]
#[path = "control_media_spawn_aim_app_tests.rs"]
mod spawn_aim_app_tests;

#[cfg(test)]
#[path = "control_media_worker_start_app_tests.rs"]
mod worker_start_app_tests;

#[cfg(test)]
#[path = "control_media_video_parse_tests.rs"]
mod video_parse_tests;

#[cfg(test)]
#[path = "control_media_call_main_within_tests.rs"]
mod call_main_within_tests;

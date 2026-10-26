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

const MAIN_THREAD_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    match rx.recv_timeout(MAIN_THREAD_REPLY_TIMEOUT) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(MainHopError::Reason(
            "main-thread reply did not arrive within 30s",
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(MainHopError::Reason("main-thread reply dropped"))
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
            AuxTarget::About => self
                .native_settings_route_view_target(SettingsRoute::About)
                .is_some(),
            AuxTarget::Update => self
                .native_settings_route_view_target(SettingsRoute::SoftwareUpdate)
                .is_some(),
            AuxTarget::Menu => self.front().is_some_and(|ws| ws.palette().is_some()),
            AuxTarget::TabMenu => self.front().is_some_and(|ws| ws.tab_menu.is_some()),
            AuxTarget::AutopilotMenu | AuxTarget::FactMenu => {
                self.autopilot_menu.as_ref().is_some_and(|m| {
                    Some(m.wid) == self.frontmost_window
                        && (m.owner == crate::autopilot_menu::MenuOwner::Autopilot)
                            == (target == AuxTarget::AutopilotMenu)
                })
            }
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
mod window_target_refusal_tests {
    use crate::app_introspect::AuxTarget;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId};

    #[test]
    fn a_bare_word_that_is_no_target_is_refused_not_written() {
        let refusal = super::unknown_window_target("bogus").expect("a bare word is refused");
        assert!(refusal.starts_with("ERR "), "{refusal}");
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

    /// `open fact-menu <name>` takes every name `controls` prints (each fact's `fact=<key>`,
    /// [`crate::claude_lights::Fact::key`]), and the usage names them all. NEGATIVE CONTROL: a
    /// bogus name, a missing one and a trailing word are refused with that usage.
    #[test]
    fn open_fact_menu_names_its_fact() {
        use crate::claude_lights::Fact;
        for (words, fact) in [
            ("fact-menu model", Fact::Model),
            ("fact-menu effort", Fact::Effort),
            ("FACT-MENU path", Fact::Path),
            ("fact-menu  branch", Fact::Branch),
            ("fact-menu context", Fact::Context),
            ("fact-menu usage", Fact::Usage),
            ("fact-menu top-effort", Fact::TopEffort),
            ("fact-menu account", Fact::Account),
        ] {
            let said: Vec<String> = words
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            assert_eq!(
                said,
                ["fact-menu", fact.key()],
                "{words}: the name `controls` prints as `fact={}`",
                fact.key()
            );
            assert_eq!(
                super::fact_menu_operand(words),
                Some(Ok(Some(fact))),
                "{words}"
            );
        }
        assert_eq!(super::fact_menu_operand("fact-menu close"), Some(Ok(None)));
        for refused in ["fact-menu", "fact-menu bogus", "fact-menu path x"] {
            let refusal = super::fact_menu_operand(refused)
                .expect("the fact menu's words")
                .expect_err(refused);
            assert_eq!(
                refusal,
                "ERR usage: open fact-menu <model | effort | path | branch | context | usage \
                 | top-effort | account> | open fact-menu close\n",
                "{refused}"
            );
        }
        assert_eq!(super::fact_menu_operand("autopilot-menu"), None);
        assert_eq!(super::fact_menu_operand("fact-menus"), None);
    }

    /// One list writes an exhaustive `match` over `Fact` and the facts `Fact::EVERY` is held
    /// to, so a fact left out of `EVERY` (which `Fact::parse` walks and the usage prints) fails
    /// here, and a new variant does not compile until it is listed. Each wire name parses back
    /// to its fact.
    #[test]
    fn every_fact_is_in_every_so_each_parses_and_the_usage_names_it() {
        use crate::claude_lights::Fact;
        macro_rules! each_fact {
            ($($variant:ident),+ $(,)?) => {{
                let ident = |fact: Fact| match fact {
                    $(Fact::$variant => stringify!($variant)),+
                };
                [$((Fact::$variant, ident(Fact::$variant))),+]
            }};
        }
        let variants = each_fact!(
            TopEffort, Model, Effort, Path, Branch, Context, Usage, Account
        );
        let usage = super::fact_menu_operand("fact-menu")
            .expect("the fact menu's words")
            .expect_err("a missing name is refused with the usage");
        let named: Vec<&str> = usage
            .split_once('<')
            .and_then(|(_, rest)| rest.split_once('>'))
            .map(|(names, _)| names.split('|').map(str::trim).collect())
            .unwrap_or_default();
        for (fact, ident) in variants {
            assert!(
                Fact::EVERY.contains(&fact),
                "Fact::{ident} is left out of Fact::EVERY: {:?}",
                Fact::EVERY
            );
            assert_eq!(
                Fact::parse(fact.key()),
                Some(fact),
                "Fact::{ident}'s wire name `{}` parses back",
                fact.key()
            );
            assert_eq!(
                super::fact_menu_operand(&format!("fact-menu {}", fact.key())),
                Some(Ok(Some(fact))),
                "Fact::{ident}: `open fact-menu {}`",
                fact.key()
            );
            assert_eq!(
                named.iter().filter(|&&name| name == fact.key()).count(),
                1,
                "Fact::{ident}'s `{}` is in the usage once: {usage}",
                fact.key()
            );
        }
        assert_eq!(
            (Fact::EVERY.len(), named.len()),
            (variants.len(), variants.len()),
            "EVERY and the usage hold each fact once: {:?}, {usage}",
            Fact::EVERY
        );
    }

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

        app.switch_tab_in(WindowId(0), 0);
        assert!(
            app.read_aux_controls(AuxTarget::Prefs)[0].starts_with("state open=true"),
            "`controls prefs` still reads the background Settings tab"
        );
        assert!(
            app.window_target_refusal(AuxTarget::Prefs).is_some(),
            "but the front window shows the terminal"
        );
    }

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
            match v.parse::<usize>() {
                Ok(n) if n >= 1 => count = n.min(VIDEO_FRAMES_MAX),
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

/// Handles `open <target> [close]`, opening or closing a native tab or auxiliary surface.
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
    if let Some(operand) = fact_menu_operand(trimmed) {
        let fact = match operand {
            Ok(fact) => fact,
            Err(refusal) => return refusal,
        };
        return match call_main(proxy, |reply| Wake::OpenFactMenu { fact, reply }) {
            Ok(Ok(())) => format!(
                "OK {} fact-menu\n",
                if fact.is_none() { "closed" } else { "opened" }
            ),
            Ok(Err(msg)) => format!("ERR {msg}\n"),
            Err(e) => format!("ERR {e}\n"),
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
            _ => "Agents \u{25b8} Take Over This Tab / Hand Back This Tab",
        };
        format!("ERR denied: {name} is the person's own act at the window ({at})\n")
    })
}

/// Returns whether `invoke <name>` counts as a user-initiated action in development builds.
pub(crate) fn person_invoke_act(name: &str) -> bool {
    person_seam_armed()
        && crate::menu::MenuAction::from_invoke_name(name.trim()).is_some_and(|action| {
            action.invoke_authority() == crate::menu::InvokeAuthority::PersonOnly
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

const PERSON_USAGE: &str = "usage: person look <window>|off | person menubar press <at>";

/// Parsed `person` control request for development-build UI simulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersonAsked {
    Look(Option<u64>),
    Press(crate::status_item::MenuAt),
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

/// Parsed `trail` control query (`Status`, or `Admissions(n)`: the last `n` admission records,
/// all when `n` is omitted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailForm {
    Admissions(Option<usize>),
    Status,
}

/// Parses the argument tail of the `trail` control verb.
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

/// Handles `trail [status|<n>]`, reporting cursor-trail diagnostics for the focused window.
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
/// ([`aterm_agent::harness::worker::started_words`]).
pub(crate) fn spawn_usage() -> String {
    format!(
        "ERR usage: spawn [window=<id>] [raise=<t|f>] [cwd=<path>] [split=<v|h>] [theme=<name>] \
         [connected=controlled|controller place=window|tab of=<sid> | agent=<{}> [model=<m>] \
         [autopilot=<land|cruise>] [from=<sid>] len=<n>]\n",
        aterm_agent::harness::worker::started_words()
    )
}

/// Parsed `agent=<word>` worker specification for `spawn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentSpec {
    Worker {
        agent: aterm_phase::Program,
        model: Option<String>,
        autopilot: Option<aterm_agent::supervise::Autopilot>,
        from: Option<aterm_session::SessionId>,
    },
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

#[derive(Debug, PartialEq, Eq)]
enum SpawnForm {
    Plain {
        window: Option<u64>,
        raise: Option<bool>,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
        theme: Option<String>,
        agent: Option<AgentSpec>,
    },
    Connected {
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: aterm_session::SessionId,
        cwd: Option<String>,
        theme: Option<String>,
    },
}

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
            framed = true;
        } else {
            return Err(());
        }
    }
    let agent = match agent {
        Some(AgentSpec::Worker { agent, .. }) => Some(AgentSpec::Worker {
            agent,
            model,
            autopilot,
            from,
        }),
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
        _ => Err(()),
    }
}

/// The refusal of a spawn line naming a retired agent (`agent=gemini`), ahead of the usage line.
fn spawn_retired_refusal(rest: &str) -> Option<String> {
    let tokens = split_quoted_tokens(rest)?;
    crate::task_launch::retired_worker(tokens.iter().find_map(|tok| tok.strip_prefix("agent=")))
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

/// Handles `spawn ...`, creating a new terminal session or worker on the main thread.
pub(crate) fn cmd_spawn(proxy: &EventLoopProxy<Wake>, rest: &str, session: Option<u64>) -> String {
    if let Some(refusal) = spawn_retired_refusal(rest) {
        return refusal;
    }
    let Ok(form) = parse_spawn_args(rest) else {
        return spawn_usage();
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
        Ok(Ok((active, count))) => format!("OK {active} {count}\n"),
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
    /// Formats the wire `ERR ...` line for this worker block reason.
    pub(crate) fn refusal(self, agent: aterm_phase::Program) -> String {
        let word = agent.name();
        match self {
            Self::Unhosted => {
                "unsupervised: no window harness runs here (no control socket)".to_string()
            }
            Self::Unsupervised(switch) => format!("unsupervised: {switch}"),
            Self::Unseen(why) => format!("spawn agent={word}: {why}"),
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

    /// Checks whether this instance can launch a worker for `agent` and returns its shell dialect.
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
        if let Some(why) =
            aterm_agent::harness::worker::worker_cli(agent).and_then(|cli| cli.end_unseen())
        {
            return Err(WorkerBlock::Unseen(why));
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
            .set_task_worker(worker.order.agent, worker.order.trusted_cwd.clone());
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
            return self.tab_new_answered(wid, reply);
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
            return self.tab_new_answered(front, reply);
        }
        let _ = reply.send(self.tab_cmd_bracketed(el, by, |app| app.apply_tab_cmd(action)));
    }

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
        reply: tx,
    }) {
        Ok(Ok(())) => format!("OK closed {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Handles `chrome`, dumping the front window's native chrome (toolbar, menu bar or menu model),
/// the drawn autopilot chips, the status-item menu and the presence line.
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
mod trail_parse_tests {
    use super::{TrailForm, parse_trail_form};

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
        assert_eq!(
            parse_fx_form("celebrate on=done"),
            Ok(FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 2,
                on: CelebrateOn::Done
            })
        );
    }

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
    use super::{
        AgentSpec, SpawnForm, parse_spawn_args, spawn_retired_refusal, split_quoted_tokens,
    };
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};

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

    #[test]
    fn unquoted_requests_tokenize_exactly_as_whitespace_splitting_did() {
        for rest in [
            "",
            "   ",
            "cwd=/tmp/x",
            "  cwd=/tmp/x  ",
            "cwd=/tmp/x split=v",
            r#"cwd=/tmp/a"b"#,
            r#"cwd=/tmp/a"b"c"#,
            r#"cwd=/tmp/say"hi split=h"#,
        ] {
            let expected: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            assert_eq!(split_quoted_tokens(rest), Some(expected), "{rest:?}");
        }
    }

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
            agent_of("agent=agy model=gemini-2.5-pro- from=s-ab12 len=3"),
            Some(AgentSpec::Worker {
                agent: aterm_phase::Program::Agy,
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
            aterm_phase::Program::Codex,
            aterm_phase::Program::Agy,
            aterm_phase::Program::Muse,
            aterm_phase::Program::Grok,
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
            "agent=gemini len=1",
            "agent=gemini-cli len=1",
            "agent=codex-old len=1",
            "agent=muse model=a/b len=1",
            "agent=gemini model=--yolo len=1",
            "agent=muse model=-x len=1",
            "agent=claude model=- len=1",
            "agent=Grok len=1",
            "agent=grok model=--always-approve len=1",
            "agent=kimi len=1",
            "connected=controlled place=tab of=s-a len=3",
        ] {
            assert_eq!(parse_spawn_args(bad), Err(()), "{bad}");
        }
        let usage = super::spawn_usage();
        assert!(
            usage.contains(
                "agent=<claude|codex|agy|muse> [model=<m>] [autopilot=<land|cruise>] \
                 [from=<sid>] len=<n>"
            ) || aterm_phase::Program::Grok.supervisable(),
            "{usage}"
        );
    }

    /// `agent=grok` IS A WORKER WORD: `spawn agent=grok` parses to a Grok Build worker with its
    /// model, Autopilot and starter, as every other row's word does (it was `ERR usage` while
    /// `WORKERS` had no Grok row); the usage line names it only once a spawn starts it.
    #[test]
    fn agent_grok_parses_as_a_grok_build_worker() {
        assert_eq!(
            parse_spawn_args("agent=grok model=grok-code-fast-1 from=s-ab12 len=9"),
            Ok(SpawnForm::Plain {
                window: None,
                raise: None,
                cwd: None,
                split: None,
                theme: None,
                agent: Some(AgentSpec::Worker {
                    agent: aterm_phase::Program::Grok,
                    model: Some("grok-code-fast-1".to_string()),
                    autopilot: None,
                    from: Some(aterm_session::SessionId::new("s-ab12")),
                }),
            })
        );
        assert_eq!(
            crate::task_launch::worker_word(Some("grok")),
            "grok",
            "the frame's replies name grok, not the claude fallback"
        );
        let usage = super::spawn_usage();
        assert_eq!(
            usage.contains("|grok>"),
            aterm_phase::Program::Grok.supervisable(),
            "{usage}"
        );
    }

    /// A SPAWN NAMING THE RETIRED GEMINI CLI IS REFUSED BY NAME, with agy, which
    /// replaces it, named; never the usage line and never another worker.
    #[test]
    fn a_spawn_of_the_retired_gemini_cli_is_refused_with_agy_named() {
        for (rest, word) in [
            ("agent=gemini len=3", "gemini"),
            ("cwd=/w agent=Gemini model=m-1 len=1", "Gemini"),
            ("agent=gemini-cli", "gemini-cli"),
        ] {
            assert_eq!(
                spawn_retired_refusal(rest).as_deref(),
                Some(
                    format!(
                        "ERR spawn agent={word}: Gemini CLI is no longer supported: agy \
                         replaces it (agent=agy)\n"
                    )
                    .as_str()
                ),
                "{rest}"
            );
            assert_eq!(
                parse_spawn_args(rest),
                Err(()),
                "{rest}: no worker of any agent"
            );
        }
        for rest in [
            "agent=agy len=1",
            "agent=geminix len=1",
            "agent=/bin/gemini len=1",
            "",
        ] {
            assert_eq!(spawn_retired_refusal(rest), None, "the control: {rest}");
        }
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
        assert_eq!(parse_spawn_args(r#"cwd="C:\Program Files"#), Err(()));
    }

    #[test]
    fn connected_spawn_requires_the_full_explicit_form() {
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
        assert_eq!(parse_spawn_args("connected=controlled place=tab"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled of=s-abc"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled"), Err(()));
        assert_eq!(parse_spawn_args("place=tab"), Err(()));
        assert_eq!(parse_spawn_args("of=s-abc"), Err(()));
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

#[cfg(test)]
mod wire_tab_close_guard_tests {
    use crate::app_window::{CloseConfirm, WIRE_CLOSE_REFUSED, WIRE_TAB_CLOSE_REFUSED};
    use crate::session_store::{ExitActor, ExitReason};
    use crate::{App, TabAction, WindowId};

    use crate::feed_session_for_test as feed;

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

    fn wire_tab_close(app: &mut App, which: Option<usize>) -> Result<(usize, usize), String> {
        wire_tab_close_by(app, which, ExitActor::Ctl)
    }

    fn wire_tab_close_by(
        app: &mut App,
        which: Option<usize>,
        by: ExitActor,
    ) -> Result<(usize, usize), String> {
        let state = app.tab_cmd_applied(by, |app| app.apply_tab_cmd(TabAction::Close(which)));
        app.close_confirm = CloseConfirm::Interactive;
        state
    }

    /// The newest close's reason and closer, from the closed index.
    fn last_exit(app: &App) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let row = g
            .closed_logs()
            .exits_since(0)
            .next_back()
            .expect("an exit row after the close");
        (row.reason, row.actor.as_wire().to_string())
    }

    #[test]
    fn a_wire_tab_close_journals_its_caller() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));

        wire_tab_close(&mut app, Some(1)).expect("a non-last tab closes");
        assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

        wire_tab_close_by(&mut app, None, ExitActor::Sid("s-edge-caller".to_string()))
            .expect("an idle last-tab close proceeds");
        assert!(app.windows[&wid].pending_close);
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        assert_eq!(
            last_exit(&app),
            (ExitReason::CtlClose, "s-edge-caller".to_string())
        );
    }

    #[test]
    fn an_invoked_close_tab_journals_its_caller_not_a_human() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));

        assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
        assert_eq!(last_exit(&app), (ExitReason::UiClose, "human".to_string()));

        let invoked = app.invoke_attributed(ExitActor::Ctl, |app| {
            assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
            Ok("invoked CloseTab".to_string())
        });
        assert_eq!(invoked, Ok("invoked CloseTab".to_string()));
        assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

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
        app.pool.attach(1);
        let state = wire_tab_close(&mut app, Some(1)).expect("a co-viewed busy tab closes");
        assert_eq!(state.1, 1, "the co-viewed tab closed");
        assert!(
            app.pool.get(1).is_some(),
            "its session lives on in the other view"
        );
    }

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

    #[test]
    fn aimed_tab_commands_drive_the_hosting_window_not_the_front_one() {
        let mut app = two_windows();
        app.headless = false;
        let (front, back) = (WindowId(1), WindowId(0));
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Next), Ok((0, 2)));
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Prev), Ok((1, 2)));
        assert_eq!(app.apply_tab_cmd_in(back, TabAction::Select(0)), Ok((0, 2)));
        assert_eq!(app.windows[&back].tab_set.active_index(), Some(0));
        assert_eq!(app.windows[&front].tab_set.len(), 1);
        assert_eq!(app.windows[&front].tab_set.active_index(), Some(0));
        assert_eq!(app.frontmost_window, Some(front));
        assert_eq!(app.apply_tab_cmd(TabAction::Next), Ok((0, 1)));
        assert_eq!(
            app.apply_tab_cmd_in(WindowId(9), TabAction::Next),
            Ok((0, 0))
        );
        assert!(app.structural_invariants_ok());
    }

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

    /// A headless app that would start a worker of any supervised agent: the harness hosted and
    /// on, shell integration on, zsh.
    fn supervising_app() -> App {
        let mut app = App::headless_for_test();
        app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        ));
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = true\nheadless = true\n"),
        );
        app.session_factory.integrate = true;
        app.session_factory.shell_override = Some("/bin/zsh".to_string());
        app
    }

    /// `spawn agent=grok` IS REFUSED, BEFORE ANY TAB OPENS, WHILE NO READER SEES GROK BUILD'S
    /// TURN END: its task could never be marked done and `await task` would never wake, so the
    /// reply says why (fail closed) instead of starting a worker nobody can see end. Muse Code,
    /// whose turn end aterm reads, passes the same check in the same app (the control). The test
    /// does not follow `Program::supervisable`: flipping it for Grok Build lifts this refusal, so
    /// the flip turns this red and is made on purpose
    /// (`docs/DESIGN-harness-repos-2026-10-24.md` D40).
    #[test]
    fn a_grok_worker_is_refused_while_its_turn_end_is_unseen() {
        use super::{SpawnAim, WorkerBlock};
        let app = supervising_app();
        assert!(
            app.worker_shell(aterm_phase::Program::Muse).is_ok(),
            "PRECONDITION: this app starts a supervised agent's worker"
        );
        let block = app.worker_shell(aterm_phase::Program::Grok).expect_err(
            "refused: lifting this is a deliberate change that reads Grok Build's turn end and \
             keeps WorkerCli::signed_out on the sign-in gate (D40)",
        );
        assert!(
            matches!(&block, WorkerBlock::Unseen(why) if why.contains("Grok Build")),
            "{block:?}"
        );
        let refusal = block.refusal(aterm_phase::Program::Grok);
        assert!(refusal.starts_with("spawn agent=grok: "), "{refusal}");
        assert!(refusal.contains("await task"), "{refusal}");
        let order = TaskOrder::of_frame(
            Some(b"the task".to_vec()),
            aterm_phase::Program::Grok,
            None,
            None,
            None,
        )
        .expect("a well-formed order");
        let Err(why) = app.resolve_spawn(SpawnAim::Front, Some(order)) else {
            panic!("a Grok Build worker was planned");
        };
        assert_eq!(why, refusal);
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

            let sid = sid_of(&app, 3);
            let requested = worker();
            let seed = requested.seed();
            let task = seed.write_for(&SessionId::new(sid.as_str())).unwrap();
            let pending = app.ready_worker(&sid, requested, seed).unwrap();
            assert!(task.exists(), "the task file precedes the launcher");
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
            assert!(!task.exists(), "the task file does not reappear");
        });
    }

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
                (3, "agent=muse autopilot=cruise len=8", Autopilot::Cruise),
                (4, "agent=agy len=8", Autopilot::Land),
                (5, "agent=muse len=8", Autopilot::Land),
                (6, "agent=codex autopilot=cruise len=8", Autopilot::Cruise),
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
            app.push_stub_tab(wid, crate::stub_session(7));
            assert_eq!(
                ctx_of(&app, 7)
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

    #[cfg(unix)]
    fn drained(fd: i32) -> Vec<u8> {
        let mut bytes = [0u8; 4096];
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        usize::try_from(read).map_or_else(|_| Vec::new(), |n| bytes[..n].to_vec())
    }

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

    #[test]
    fn video_frames_before_any_recording_says_none_was_found() {
        const INSTANCE: &str = "p303-fresh";
        let tmp = video_test_dir("fresh");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let expected = "ERR video frames: no finished recording found";
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with(expected), "no video/ dir: {out:?}");
        std::fs::create_dir_all(tmp.join(super::control_auth::VIDEO_DIR)).unwrap();
        let out = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(out.starts_with(expected), "no instance dir: {out:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_ranks_by_delta_picks_newest_and_isolates_instance() {
        const INSTANCE: &str = "p101-current";
        const SIBLING: &str = "p202-sibling";
        let tmp = video_test_dir("rank");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(5, 10), (1, 20)]);
        std::fs::create_dir_all(instance_root(&tmp, INSTANCE).join("rec-1000000002-000")).unwrap();
        write_fake_recording(
            &tmp,
            INSTANCE,
            "rec-1000000001-000",
            &[(3, 0), (40, 16), (7, 33), (2, 50)],
        );
        write_fake_recording(&tmp, SIBLING, "rec-9999999999-000", &[(999, 99)]);

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
        assert!(
            !out.contains("delta=5") && !out.contains("delta=999"),
            "ignored the older recording and live sibling: {out:?}"
        );
        assert!(!out.contains(SIBLING), "no sibling path disclosed: {out:?}");

        let all = video_frames_for_instance(
            &tmp,
            INSTANCE,
            std::iter::empty(),
            &crate::control::ArtifactBudgets::shipping(),
        );
        assert!(all.starts_with("OK 4\n"), "all frames: {all:?}");

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
            .saturating_add(2)
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
        super::control_auth::wait_for_artifact_cleanup_for_test(&oldest_path);

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

            let _ = std::fs::remove_file(&tmp);
            for dir in [tmp, moved, outside] {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

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
        let at_zero = hops.post(0);
        assert!(hops.since_ns().is_some());
        drop(at_zero);
    }

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
        drop(parked);
        assert_eq!(
            hops.since_ns(),
            Some(20),
            "a reply after the take counts out nothing more"
        );
        let late = waiting.mark();
        drop(waiting);
        assert_eq!(hops.since_ns(), None);
        let next = hops.post(30);
        late.taken();
        assert_eq!(
            hops.since_ns(),
            Some(30),
            "a take after the give-up counts out nothing"
        );
        drop(next);
        assert_eq!(hops.since_ns(), None);

        let first = hops.post(40);
        let second = hops.post(50);
        first.mark().taken();
        assert_eq!(hops.since_ns(), Some(40));
        drop(first);
        assert_eq!(hops.since_ns(), Some(40), "the second still waits");
        drop(second);
        assert_eq!(hops.since_ns(), None);
    }

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

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! File drag-and-drop reception for the Wayland backend.
//!
//! Added by the aterm project in 2026; see the repository NOTICE.
//!
//! Upstream winit 0.30 implements file drag-and-drop only on macOS
//! (`NSDraggingDestination`) and X11 (XDND). Its Wayland backend shipped no
//! `wl_data_device` handling at all, so `WindowEvent::DroppedFile` (and the
//! hover events) never fired on a native-Wayland session and a dropped file was
//! silently ignored. This module — added by aterm to its copy of winit (vendored
//! under `vendor/winit` until it became `crates/aterm-window`) — fills that gap,
//! over sctk's `data_device_manager` until LW.7 and over first-party
//! `aterm_wayland::selection`'s device since
//! (docs/AUDIT-third-party-2026-10-15.md, LW.7).
//!
//! Flow: a `wl_data_device` is created for every seat. When a drag enters one of
//! our surfaces we accept the `text/uri-list` mime type and the `Copy` action
//! (required, or the compositor cancels the source). On drop we `receive` the
//! offer's pipe and stream it through the event loop's poll, accumulating
//! the bytes until EOF; then we percent-decode + parse the `file://` URIs (the
//! same rules as the X11 backend's `dnd.rs`) and push one
//! `WindowEvent::DroppedFile` per path for the target window — so downstream code
//! sees the identical event sequence it already gets on X11/macOS.
//!
//! Scope: only the RECEIVING side: `DroppedFile` per path, with an empty-path
//! `HoveredFile` when a file drag enters and `HoveredFileCancelled` when it
//! leaves or drops. winit never offers a drag *source*.

use std::io::{ErrorKind, PipeReader, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

use aterm_wayland::protocols::wayland::wl_data_device_manager::dnd_action;
use aterm_wayland::selection::{DragEvent, Dropped};

use crate::event::WindowEvent;
use crate::platform_impl::common::percent::{percent_decode_utf8, uri_list_entries};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, WindowId};

/// The mime type aterm/iTerm-style file drops are delivered through. File
/// managers advertise dragged files as a newline-separated list of `file://`
/// URIs under this type (RFC 2483).
const URI_LIST: &str = "text/uri-list";

/// Cap on a single drop's `text/uri-list` payload. A hostile or buggy source
/// could otherwise stream unbounded bytes into the event loop; 1 MiB is far more
/// than any real multi-file selection of paths and bounds the accumulation.
const MAX_URI_LIST_BYTES: usize = 1 << 20;

/// An in-flight read of a dropped offer's `text/uri-list` pipe. The event loop
/// polls the pipe with the display and appends its bytes to `data` each time it
/// is readable, until EOF; then [`WinitState`] parses them into paths and emits
/// one `DroppedFile` per path for `window_id`.
pub struct DndPipe {
    /// The window the drop landed on, resolved from the drag's surface.
    window_id: WindowId,
    /// Bytes read from the offer pipe so far.
    data: Vec<u8>,
    /// The pipe's read end, non-blocking.
    pipe: PipeReader,
    /// The drop offer, kept alive so it can be `finish`ed on EOF (the Wayland
    /// protocol requires the destination to finish a dropped offer); it is
    /// destroyed when it drops.
    offer: Dropped,
}

impl DndPipe {
    /// The descriptor the loop polls.
    pub(crate) fn fd(&self) -> RawFd {
        self.pipe.as_raw_fd()
    }
}

/// What one read of a drop pipe came to.
enum PipeRead {
    /// More may come.
    Open,
    /// The source closed its end: the bytes are whole.
    Eof,
    /// The read failed, or the payload passed its cap: give the drop up.
    Failed,
}

impl WinitState {
    /// One drag-and-drop event of `seat`'s data device, where the inbox has it.
    pub(crate) fn drag_event(&mut self, seat: u32, event: DragEvent) {
        match event {
            DragEvent::Enter { surface, offer, .. } => self.dnd_enter(seat, surface, offer),
            DragEvent::Leave => self.dnd_leave(),
            // The accept + action set on `enter` persists for the drag; per-motion
            // updates are unnecessary for file drops.
            DragEvent::Motion { .. } => {},
            DragEvent::Drop => self.dnd_drop(seat),
        }
    }

    fn dnd_enter(
        &mut self,
        seat: u32,
        surface: Option<aterm_wayland::proxy::Proxy>,
        offer: Option<aterm_wayland::selection::OfferInfo>,
    ) {
        let Some(offer) = offer else {
            // No offer means a source-less / internal drag — nothing to accept.
            return;
        };

        // Only engage when the drag is over a surface that is one of our windows.
        let Some(window_id) = surface.map(wayland::make_wid) else { return };
        if !self.windows.get_mut().contains_key(&window_id) {
            return;
        }
        let Some(device) = self.seats.get(&seat).and_then(|s| s.data_device.as_ref()) else {
            return;
        };

        // Accept the drop IFF the source offers files. Accepting a mime type and a
        // DnD action is REQUIRED for the compositor to deliver the drop at all;
        // rejecting (None) tells the source we are not a valid target.
        let offers_files = offer.mime_types.iter().any(|m| m == URI_LIST);
        if offers_files {
            device.accept_drag(Some(c"text/uri-list"), dnd_action::COPY, dnd_action::COPY);
            // Emit the `HoveredFile` edge so downstream can light a drop-target
            // highlight, matching the X11/AppKit backends. Wayland reveals the file
            // paths only on the actual drop, so the path here is EMPTY — it is the
            // hover-on signal, not the payload (the real paths arrive as
            // `DroppedFile`). Remember the window so `leave` can address the paired
            // `HoveredFileCancelled` (the Wayland `leave` carries no surface).
            self.dnd_hover_window = Some(window_id);
            self.events_sink.push_window_event(WindowEvent::HoveredFile(PathBuf::new()), window_id);
        } else {
            device.reject_drag();
        }
    }

    fn dnd_leave(&mut self) {
        // A file drag left WITHOUT dropping: cancel the drop-target highlight on the
        // window we lit up on `enter` (the Wayland `leave` carries no surface, hence
        // the remembered id). aterm-wayland's device destroys the un-dropped offer.
        if let Some(window_id) = self.dnd_hover_window.take() {
            self.events_sink.push_window_event(WindowEvent::HoveredFileCancelled, window_id);
        }
    }

    fn dnd_drop(&mut self, seat: u32) {
        // The drag was released: end the drop-target highlight NOW (the gesture is
        // over) regardless of whether the data read below succeeds, so the highlight
        // can never get stuck on. The dropped path(s) arrive separately as
        // `DroppedFile` once the pipe read completes.
        let window_id = self.dnd_hover_window.take();
        if let Some(window_id) = window_id {
            self.events_sink.push_window_event(WindowEvent::HoveredFileCancelled, window_id);
        }

        let offer = self
            .seats
            .get(&seat)
            .and_then(|s| s.data_device.as_ref())
            .and_then(|device| device.take_dropped());
        let Some(offer) = offer else {
            return;
        };

        let offers_files = offer.info().mime_types.iter().any(|m| m == URI_LIST);
        let window_id = match window_id {
            Some(window_id) if offers_files => window_id,
            // Not a file drop (or one over no window of ours). We rejected this
            // offer on `enter` (NULL mime, no action), so finishing it here would
            // raise the FATAL `invalid_finish` protocol error (finish requires a
            // non-NULL accepted mime + a selected action) and the compositor would
            // disconnect us — crashing the whole process on a common non-file drag
            // (a text selection, a browser link). Just destroy the offer (it drops).
            _ => return,
        };

        // Request the file list. `receive` is always permitted after a drop.
        let pipe = match offer.receive(c"text/uri-list") {
            Ok(pipe) => pipe,
            // Couldn't open the transfer pipe; release the offer unfinished (the sctk
            // example did too on this error path), so `invalid_finish` is never risked
            // if no action was negotiated.
            Err(_) => return,
        };
        if let Err(_error) = set_nonblocking(&pipe) {
            tracing::warn!("wayland: a drop's pipe stays blocking: {_error}");
            return;
        }

        // Stream the pipe through the event loop (non-blocking): one read per
        // readiness, accumulating into the entry, until EOF.
        self.dnd_pipes.push(DndPipe { window_id, data: Vec::new(), pipe, offer });
    }

    /// Read every drop pipe the loop polled readable (`ready` names their descriptors), and
    /// finish the drops whose source is done.
    pub(crate) fn read_dnd_pipes(&mut self, ready: &[RawFd]) {
        let mut i = 0;
        while i < self.dnd_pipes.len() {
            if !ready.contains(&self.dnd_pipes[i].fd()) {
                i += 1;
                continue;
            }
            match read_some(&mut self.dnd_pipes[i]) {
                PipeRead::Open => i += 1,
                // EOF: the source closed its end. Parse + emit, then finish the offer.
                PipeRead::Eof => {
                    let pipe = self.dnd_pipes.remove(i);
                    for path in parse_uri_list(&pipe.data) {
                        self.events_sink
                            .push_window_event(WindowEvent::DroppedFile(path), pipe.window_id);
                    }
                    pipe.offer.finish();
                },
                PipeRead::Failed => {
                    let pipe = self.dnd_pipes.remove(i);
                    pipe.offer.finish();
                },
            }
        }
    }
}

/// One read of a drop's pipe (the loop polled it readable), bounded by the payload cap.
fn read_some(pipe: &mut DndPipe) -> PipeRead {
    let mut buf = [0u8; 4096];
    match pipe.pipe.read(&mut buf) {
        Ok(0) => PipeRead::Eof,
        Ok(n) => {
            // Bound the accumulation against a runaway source.
            if pipe.data.len() + n > MAX_URI_LIST_BYTES {
                return PipeRead::Failed;
            }
            pipe.data.extend_from_slice(&buf[..n]);
            PipeRead::Open
        },
        Err(ref e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
            PipeRead::Open
        },
        Err(_) => PipeRead::Failed,
    }
}

/// Make a pipe end non-blocking, so a drained readiness or a stalled peer cannot
/// block the loop or a clipboard worker in a pipe read or write.
pub(crate) fn set_nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fcntl(2) on a descriptor the pipe owns; F_GETFL takes no argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above; F_SETFL takes the new flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Parse a `text/uri-list` payload into local filesystem paths, mirroring the X11
/// backend (`dnd.rs::local_paths`): frame raw entries before decoding, accept
/// only `file://` URIs without a hostname, and skip comment lines (`#…`) and any
/// entry that names no local file, keeping the rest. Each path is the one the
/// source named: a symlink dropped is the link, not its target, and nothing reads
/// the filesystem on the event loop's thread (this canonicalised each path).
fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for uri in uri_list_entries(data) {
        let Some(rest) = uri.strip_prefix(b"file://") else {
            continue; // only the file:// scheme is supported, like X11
        };
        // The form is file://host/path; a non-empty host (rest not starting with
        // '/') is unsupported (matches X11's HostnameSpecified rejection).
        if !rest.starts_with(b"/") {
            continue;
        }
        let Ok(decoded) = percent_decode_utf8(rest) else {
            continue;
        };
        // A NUL (`%00`) is in no file's path.
        if decoded.contains('\0') {
            continue;
        }
        paths.push(PathBuf::from(decoded));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_list_extracts_file_paths() {
        // A typical file-manager payload: CRLF-separated, percent-encoded spaces.
        let data = b"file:///tmp/one.txt\r\nfile:///tmp/two%20three.txt\r\n";
        let paths = parse_uri_list(data);
        // /tmp exists on the build host, but the files may not, so compare the
        // fallback (uncanonicalised) form by checking the trailing components.
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("one.txt"), "got {:?}", paths[0]);
        assert!(paths[1].ends_with("two three.txt"), "got {:?}", paths[1]);

        // A filename may contain CRLF, literal URI-looking text, a leading
        // hash or trailing whitespace. Only the raw list frames entries.
        let paths = parse_uri_list(
            b"file:///tmp/aterm-uri-a%0D%0Afile:///tmp/aterm-uri-b%20\r\nfile:///tmp/%23aterm-uri-c\r\n",
        );
        assert_eq!(paths.len(), 2);
        assert!(paths[0]
            .as_os_str()
            .to_string_lossy()
            .ends_with("aterm-uri-a\r\nfile:///tmp/aterm-uri-b "));
        assert!(paths[1].ends_with("#aterm-uri-c"));
    }

    #[test]
    fn parse_uri_list_skips_comments_hostnames_and_other_schemes() {
        let data = b"#comment\r\nhttp://example.com/x\r\nfile://host/share/f\r\nfile:///ok/f\r\n";
        let paths = parse_uri_list(data);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("ok/f"), "got {:?}", paths[0]);
        assert!(parse_uri_list(b"%66ile:///tmp/f\r\nfile://%2Ftmp/f\r\n").is_empty());
    }

    /// A dropped symlink is the link the source named, not its target. Negative
    /// control: canonicalising each path, as this did, yields the target.
    #[test]
    fn parse_uri_list_keeps_a_symlink_as_named() {
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"").expect("a file");
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("a symlink");
        let uri = format!("file://{}\r\nfile:///tmp/a%00b\r\n", link.display());
        assert_eq!(parse_uri_list(uri.as_bytes()), [link]);
    }

    #[test]
    fn parse_uri_list_empty_is_empty() {
        assert!(parse_uri_list(b"").is_empty());
        assert!(parse_uri_list(b"\r\n\r\n").is_empty());
    }
}

//! File drag-and-drop reception for the Wayland backend.
//!
//! Added by the aterm project in 2026; see the repository NOTICE.
//!
//! Upstream winit 0.30 implements file drag-and-drop only on macOS
//! (`NSDraggingDestination`) and X11 (XDND). Its Wayland backend shipped no
//! `wl_data_device` handling at all, so `WindowEvent::DroppedFile` (and the
//! hover events) never fired on a native-Wayland session and a dropped file was
//! silently ignored. This module — added to the vendored copy under
//! `vendor/winit` — fills that gap using sctk's `data_device_manager`.
//!
//! Flow: a `wl_data_device` is created for every seat. When a drag enters one of
//! our surfaces we accept the `text/uri-list` mime type and the `Copy` action
//! (required, or the compositor cancels the source). On drop we `receive` the
//! offer's pipe and stream it through winit's calloop event loop, accumulating
//! the bytes until EOF; then we percent-decode + parse the `file://` URIs (the
//! same rules as the X11 backend's `dnd.rs`) and push one
//! `WindowEvent::DroppedFile` per path for the target window — so downstream code
//! sees the identical event sequence it already gets on X11/macOS.
//!
//! Scope: only the RECEIVING side, and only `DroppedFile` is emitted — that is
//! the event downstream apps act on. The `HoveredFile`/`HoveredFileCancelled`
//! previews are not emitted on Wayland (there is no consumer that renders a hover
//! affordance, and emitting them would require a speculative pre-drop pipe read).
//! winit never offers a drag *source*, so the `DataSourceHandler` impl below is
//! inert and exists only to satisfy `delegate_data_device!`'s trait bounds.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use calloop::PostAction;
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::WritePipe;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, QueueHandle};

use crate::event::WindowEvent;
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

/// An in-flight read of a dropped offer's `text/uri-list` pipe. The calloop event
/// loop appends the offer's bytes to `data` each time the pipe is readable, until
/// EOF; then [`WinitState`] parses them into paths and emits one `DroppedFile`
/// per path for `window_id`.
pub struct DndPipe {
    /// Identity captured by the read callback so it can find its own entry in
    /// `WinitState::dnd_pipes` (calloop does not echo a source's token back into
    /// its callback, so we can't key on the token from inside it).
    id: u64,
    /// The window the drop landed on, resolved from the offer's surface.
    window_id: WindowId,
    /// Bytes read from the offer pipe so far.
    data: Vec<u8>,
    /// The drop offer, kept alive so it can be `finish`ed + `destroy`ed on EOF
    /// (the Wayland protocol requires the destination to finish a dropped offer).
    offer: DragOffer,
}

impl WinitState {
    /// The [`DataDevice`] bound on whichever seat owns `wl_data_device`, if any.
    fn data_device_for(&self, wl_data_device: &WlDataDevice) -> Option<&DataDevice> {
        self.seats
            .values()
            .filter_map(|seat| seat.data_device.as_ref())
            .find(|dd| dd.inner() == wl_data_device)
    }
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        surface: &WlSurface,
    ) {
        // Take an OWNED `DragOffer` (it clones the `wl_data_offer` proxy) so the
        // borrow of `self.seats` is released before we touch `self.windows`.
        let offer = self.data_device_for(wl_data_device).and_then(|dd| dd.data().drag_offer());
        let Some(offer) = offer else {
            // No offer means a source-less / internal drag — nothing to accept.
            return;
        };

        // Only engage when the drag is over a surface that is one of our windows.
        let window_id = wayland::make_wid(surface);
        if !self.windows.get_mut().contains_key(&window_id) {
            return;
        }

        // Accept the drop IFF the source offers files. Accepting a mime type and a
        // DnD action is REQUIRED for the compositor to deliver the drop at all;
        // rejecting (None) tells the source we are not a valid target.
        let offers_files = offer.with_mime_types(|mimes| mimes.iter().any(|m| m == URI_LIST));
        if offers_files {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.to_string()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
            // Emit the `HoveredFile` edge so downstream can light a drop-target
            // highlight, matching the X11/AppKit backends. Wayland reveals the file
            // paths only on the actual drop, so the path here is EMPTY — it is the
            // hover-on signal, not the payload (the real paths arrive as
            // `DroppedFile`). Remember the window so `leave` can address the paired
            // `HoveredFileCancelled` (the Wayland `leave` carries no surface).
            self.dnd_hover_window = Some(window_id);
            self.events_sink
                .push_window_event(WindowEvent::HoveredFile(PathBuf::new()), window_id);
        } else {
            offer.accept_mime_type(offer.serial, None);
        }
    }

    fn leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _wl_data_device: &WlDataDevice) {
        // A file drag left WITHOUT dropping: cancel the drop-target highlight on the
        // window we lit up on `enter` (the Wayland `leave` carries no surface, hence
        // the remembered id). sctk destroys the un-dropped offer on our behalf.
        if let Some(window_id) = self.dnd_hover_window.take() {
            self.events_sink
                .push_window_event(WindowEvent::HoveredFileCancelled, window_id);
        }
    }

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _wl_data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
        // The accept + action set on `enter` persists for the drag; per-motion
        // updates are unnecessary for file drops.
    }

    fn selection(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _wl_data_device: &WlDataDevice) {
        // A clipboard selection offer — winit exposes no clipboard API, so ignore.
    }

    fn drop_performed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_data_device: &WlDataDevice,
    ) {
        // The drag was released: end the drop-target highlight NOW (the gesture is
        // over) regardless of whether the data read below succeeds, so the highlight
        // can never get stuck on. The dropped path(s) arrive separately as
        // `DroppedFile` once the pipe read completes.
        if let Some(window_id) = self.dnd_hover_window.take() {
            self.events_sink
                .push_window_event(WindowEvent::HoveredFileCancelled, window_id);
        }

        let offer = self.data_device_for(wl_data_device).and_then(|dd| dd.data().drag_offer());
        let Some(offer) = offer else {
            return;
        };
        let window_id = wayland::make_wid(&offer.surface);

        let offers_files = offer.with_mime_types(|mimes| mimes.iter().any(|m| m == URI_LIST));
        if !offers_files {
            // Not a file drop. We rejected this offer on `enter` (NULL mime, no
            // action), so calling `finish()` here would raise the FATAL
            // `invalid_finish` protocol error (finish requires a non-NULL accepted
            // mime + a selected action) and the compositor would disconnect us —
            // crashing the whole process on a common non-file drag (a text
            // selection, a browser link). Just `destroy()` the offer.
            offer.destroy();
            return;
        }

        // Request the file list. `receive` is always permitted after a drop.
        let read_pipe = match offer.receive(URI_LIST.to_string()) {
            Ok(pipe) => pipe,
            Err(_) => {
                // Couldn't open the transfer pipe; release the offer. Skip
                // `finish()` (the sctk example does too on this error path) so we
                // never risk `invalid_finish` if no action was negotiated.
                offer.destroy();
                return;
            },
        };

        // Stream the pipe through the event loop (non-blocking): one read per
        // readiness, accumulating into the entry found by `id`, until EOF.
        let id = self.dnd_next_id;
        self.dnd_next_id = self.dnd_next_id.wrapping_add(1);
        self.dnd_pipes.push(DndPipe { id, window_id, data: Vec::new(), offer });

        let _ = self.loop_handle.insert_source(read_pipe, move |_event, file, state| {
            let Some(idx) = state.dnd_pipes.iter().position(|p| p.id == id) else {
                return PostAction::Remove;
            };

            // SAFETY: calloop's `NoIoDrop` only forbids CLOSING the wrapped fd;
            // reading through `&mut File` is sound (sctk's own example does this).
            let file: &mut fs::File = unsafe { file.get_mut() };
            let mut buf = [0u8; 4096];
            match file.read(&mut buf) {
                // EOF: the source closed its end. Parse + emit, then finish the offer.
                Ok(0) => {
                    let pipe = state.dnd_pipes.remove(idx);
                    for path in parse_uri_list(&pipe.data) {
                        state
                            .events_sink
                            .push_window_event(WindowEvent::DroppedFile(path), pipe.window_id);
                    }
                    pipe.offer.finish();
                    pipe.offer.destroy();
                    PostAction::Remove
                },
                Ok(n) => {
                    let entry = &mut state.dnd_pipes[idx];
                    // Bound the accumulation against a runaway source.
                    if entry.data.len() + n > MAX_URI_LIST_BYTES {
                        let pipe = state.dnd_pipes.remove(idx);
                        pipe.offer.finish();
                        pipe.offer.destroy();
                        return PostAction::Remove;
                    }
                    entry.data.extend_from_slice(&buf[..n]);
                    PostAction::Continue
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
                Err(_) => {
                    let pipe = state.dnd_pipes.remove(idx);
                    pipe.offer.finish();
                    pipe.offer.destroy();
                    PostAction::Remove
                },
            }
        });
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
        // We always request `Copy` (set on `enter`), so nothing to renegotiate.
    }

    fn selected_action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

// winit never creates a drag-and-drop or clipboard SOURCE on Wayland, so none of
// these are ever invoked; they exist only to satisfy the trait bounds of
// `delegate_data_device!` (which dispatches `wl_data_source` to this handler).
impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: String,
        _fd: WritePipe,
    ) {
    }

    fn cancelled(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _source: &WlDataSource) {}

    fn dnd_dropped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _source: &WlDataSource) {}

    fn dnd_finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _source: &WlDataSource) {}

    fn action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

sctk::delegate_data_device!(WinitState);

/// Parse a `text/uri-list` payload into local filesystem paths, mirroring the X11
/// backend (`dnd.rs::parse_data`): percent-decode, split on CRLF, accept only
/// `file://` URIs without a hostname, and skip RFC 2483 comment lines (`#…`).
/// Each path is canonicalised (resolving symlinks) as X11 does, but a path whose
/// canonicalisation fails (e.g. the file was moved between drop and read) falls
/// back to the decoded path instead of discarding the whole list.
fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    let Some(text) = percent_decode_utf8(data) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for uri in text.split("\r\n").map(str::trim).filter(|u| !u.is_empty()) {
        if uri.starts_with('#') {
            continue; // uri-list comment line
        }
        let Some(rest) = uri.strip_prefix("file://") else {
            continue; // only the file:// scheme is supported, like X11
        };
        // The form is file://host/path; a non-empty host (rest not starting with
        // '/') is unsupported (matches X11's HostnameSpecified rejection).
        if !rest.starts_with('/') {
            continue;
        }
        let path = Path::new(rest);
        paths.push(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    }
    paths
}

/// Percent-decode bytes (`%XX` → the byte) and interpret the result as UTF-8.
/// Returns `None` if the decoded bytes are not valid UTF-8. Written inline (a few
/// lines) so the Wayland backend needs no extra `percent-encoding` dependency
/// feature; an incomplete or non-hex `%` escape is left verbatim.
fn percent_decode_utf8(data: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'%' && i + 2 < data.len() {
            if let (Some(hi), Some(lo)) = (hex_val(data[i + 1]), hex_val(data[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(data[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// One hex digit → its 0..=15 value, or `None` if `b` is not `[0-9a-fA-F]`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic_and_incomplete() {
        assert_eq!(percent_decode_utf8(b"a%20b").as_deref(), Some("a b"));
        // A complete escape at the very end decodes.
        assert_eq!(percent_decode_utf8(b"x%2F").as_deref(), Some("x/"));
        // Incomplete / non-hex escapes are left verbatim.
        assert_eq!(percent_decode_utf8(b"100%").as_deref(), Some("100%"));
        assert_eq!(percent_decode_utf8(b"a%zzb").as_deref(), Some("a%zzb"));
    }

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
    }

    #[test]
    fn parse_uri_list_skips_comments_hostnames_and_other_schemes() {
        let data = b"#comment\r\nhttp://example.com/x\r\nfile://host/share/f\r\nfile:///ok/f\r\n";
        let paths = parse_uri_list(data);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("ok/f"), "got {:?}", paths[0]);
    }

    #[test]
    fn parse_uri_list_empty_is_empty() {
        assert!(parse_uri_list(b"").is_empty());
        assert!(parse_uri_list(b"\r\n\r\n").is_empty());
    }
}

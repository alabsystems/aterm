// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/dnd.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): percent-decoding is `common/percent.rs`, not the `percent-encoding` crate,
// and the requests are aterm-x11's encoders, where upstream used x11rb's. A path is the one the
// source named, not canonicalized; an entry naming no local file is skipped, a list naming none is
// refused, and a list too large for one property is read through ICCCM INCR.

use std::os::raw::*;
use std::path::PathBuf;
use std::str::Utf8Error;
use std::sync::Arc;

use aterm_x11::proto::xproto::{cw, event_mask};
use aterm_x11::proto::ValueList;
use aterm_x11::proto::{atom, xproto};

use super::atoms::AtomName::None as DndNone;
use super::atoms::*;
use super::{util, IgnoreError as _, X11Error, XConnection};
use crate::platform_impl::common::percent::{percent_decode_utf8, uri_list_entries};

#[derive(Debug, Clone, Copy)]
pub enum DndState {
    Accepted,
    Rejected,
}

const MAX_INCR_BYTES: usize = 64 * 1024 * 1024;

/// What the source put in the drag's property (an aterm addition).
#[derive(Debug)]
pub enum Answer {
    /// The whole list.
    List(Vec<u8>),
    /// An ICCCM INCR transfer began; the list arrives in chunks ([`Dnd::take_chunk`]).
    Incr,
}

/// Why an entry of a `text/uri-list` names no local file.
#[derive(Debug)]
pub enum DndDataParseError {
    InvalidUtf8(#[allow(dead_code)] Utf8Error),
    HostnameSpecified(#[allow(dead_code)] String),
    UnexpectedProtocol(#[allow(dead_code)] String),
    /// A path holds a NUL (`%00`), which no file's path can.
    NulInPath,
}

impl From<Utf8Error> for DndDataParseError {
    fn from(e: Utf8Error) -> Self {
        DndDataParseError::InvalidUtf8(e)
    }
}

pub struct Dnd {
    xconn: Arc<XConnection>,
    pub version: Option<c_long>,
    pub type_list: Option<Vec<xproto::Atom>>,
    pub source_window: Option<xproto::Window>,
    /// The application window the drag entered, independent of its conversion's requestor.
    pub target_window: Option<xproto::Window>,
    /// Timestamp of the drag's outstanding `ConvertSelection` request.
    pub requested: Option<xproto::Timestamp>,
    /// Whether the drag dropped before its data arrived.
    pub dropped: bool,
    /// One conversion owns its requestor until its data completes or the drag ends.
    requestor: Option<Requestor>,
    /// The answer is coming in ICCCM INCR chunks: the bytes so far.
    incoming: Option<Vec<u8>>,
    /// The drag's local files once its data arrived, or empty when refused.
    pub result: Option<Vec<PathBuf>>,
}

struct Requestor {
    xconn: Arc<XConnection>,
    window: xproto::Window,
}

impl Requestor {
    fn new(xconn: Arc<XConnection>, target: xproto::Window) -> Result<Self, X11Error> {
        let window = xconn.dnd_requestor_id()?;
        let requestor = Self { xconn, window };
        requestor
            .xconn
            .send_void(xproto::create_window(
                0,
                window,
                target,
                0,
                0,
                1,
                1,
                0,
                xproto::window_class::INPUT_ONLY,
                0,
                &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
            ))?
            .check()?;
        Ok(requestor)
    }
}

impl Drop for Requestor {
    fn drop(&mut self) {
        if let Ok(cookie) = self.xconn.send_void(xproto::destroy_window(self.window)) {
            cookie.ignore_error();
        }
        let _ = self.xconn.flush_requests();
    }
}

impl Dnd {
    pub fn new(xconn: Arc<XConnection>) -> Result<Self, X11Error> {
        Ok(Dnd {
            xconn,
            version: None,
            type_list: None,
            source_window: None,
            target_window: None,
            requested: None,
            dropped: false,
            requestor: None,
            incoming: None,
            result: None,
        })
    }

    pub fn reset(&mut self) {
        self.version = None;
        self.type_list = None;
        self.source_window = None;
        self.target_window = None;
        self.requested = None;
        self.dropped = false;
        self.incoming = None;
        self.requestor = None;
        self.result = None;
    }

    /// Ask the source for the drag's data, once per drag, as of `time`.
    pub fn request(&mut self, window: xproto::Window, time: xproto::Timestamp) {
        if self.result.is_none() && self.requested.is_none() && self.incoming.is_none() {
            let requestor = match Requestor::new(self.xconn.clone(), window) {
                Ok(requestor) => requestor,
                Err(_error) => {
                    tracing::warn!("Failed to make a drag requestor: {_error}");
                    self.result = Some(Vec::new());
                    return;
                },
            };
            let request_window = requestor.window;
            self.requestor = Some(requestor);
            if let Err(_error) = self.convert_selection(request_window, time) {
                tracing::warn!("Failed to request a drag's data: {_error}");
                self.complete_request();
                self.result = Some(Vec::new());
                return;
            }
            self.requested = Some(time);
        }
    }

    pub fn matches_drag(&self, target: xproto::Window, source: xproto::Window) -> bool {
        self.target_window == Some(target) && self.source_window == Some(source)
    }

    /// An answer or chunk belongs to the one currently owned requestor.
    pub fn is_requestor(&self, window: xproto::Window) -> bool {
        self.requestor.as_ref().is_some_and(|requestor| requestor.window == window)
    }

    pub fn begin_incr(&mut self) {
        self.incoming = Some(Vec::new());
    }

    pub fn takes_chunk(&self, window: xproto::Window) -> bool {
        self.is_requestor(window) && self.incoming.is_some()
    }

    pub fn complete_request(&mut self) {
        self.requested = None;
        self.incoming = None;
        self.requestor = None;
    }

    /// Whether a `SelectionNotify` of `time` answers this drag's outstanding request.
    pub fn answers(&mut self, window: xproto::Window, time: xproto::Timestamp) -> bool {
        if !self.is_requestor(window) {
            return false;
        }
        match self.requested {
            Some(asked) if asked == time || asked == aterm_x11::proto::CURRENT_TIME => {
                self.requested = None;
                true
            },
            _ => false,
        }
    }

    pub unsafe fn send_status(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndStatus] as _, None, [
                this_window,
                accepted,
                0,
                0,
                action as _,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn send_finished(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndFinished] as _, None, [
                this_window,
                accepted,
                action as _,
                0,
                0,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn get_type_list(
        &self,
        source_window: xproto::Window,
    ) -> Result<Vec<xproto::Atom>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(source_window, atoms[XdndTypeList], atom::ATOM)
    }

    fn convert_selection(
        &self,
        window: xproto::Window,
        time: xproto::Timestamp,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        self.xconn
            .send_void(xproto::convert_selection(
                window,
                atoms[XdndSelection],
                atoms[TextUriList],
                atoms[XdndSelection],
                time,
            ))?
            .check()?;
        Ok(())
    }

    /// Reads and deletes the selection property on `window` as an INCR header or `text/uri-list`.
    pub fn take_data(&self, window: xproto::Window) -> Option<Answer> {
        let reply = self.take_property(window)?;
        if reply.type_ == self.xconn.atoms()[Incr] {
            Some(Answer::Incr)
        } else {
            (reply.type_ == self.xconn.atoms()[TextUriList] && reply.format == 8)
                .then_some(Answer::List(reply.value))
        }
    }

    /// Reads and deletes the next INCR chunk from `window` (an empty chunk ends the transfer).
    pub fn take_chunk(&self, window: xproto::Window) -> Option<Vec<u8>> {
        let reply = self.take_property(window)?;
        (reply.type_ == self.xconn.atoms()[TextUriList] && reply.format == 8).then_some(reply.value)
    }

    /// Appends `chunk` to the INCR transfer, returning the completed list when finished.
    pub fn add_chunk(&mut self, chunk: Option<Vec<u8>>) -> Option<Vec<u8>> {
        let data = self.incoming.as_mut()?;
        match chunk {
            Some(chunk) if chunk.is_empty() => self.incoming.take(),
            Some(chunk) if data.len().saturating_add(chunk.len()) <= MAX_INCR_BYTES => {
                data.extend_from_slice(&chunk);
                None
            },
            _ => {
                self.incoming = None;
                Some(Vec::new())
            },
        }
    }

    fn take_property(&self, window: xproto::Window) -> Option<xproto::GetPropertyReply> {
        let property = self.xconn.atoms()[XdndSelection];
        self.xconn
            .xcb_connection()
            .send(xproto::get_property(true, window, property, atom::ANY, 0, u32::MAX))
            .ok()?
            .reply()
            .ok()
    }

    /// Whether the drag is refused now: its data came and names no local file.
    pub fn refused(&self) -> bool {
        self.result.as_ref().is_some_and(Vec::is_empty)
    }
}

/// Extracts local file paths from a `text/uri-list` payload, skipping non-local entries.
pub fn local_paths(data: &[u8]) -> Vec<PathBuf> {
    uri_list_entries(data).filter_map(|uri| file_uri_path(uri).ok()).collect()
}

fn file_uri_path(uri: &[u8]) -> Result<PathBuf, DndDataParseError> {
    let uri = std::str::from_utf8(uri)?;
    let Some(path) = uri.strip_prefix("file://") else {
        return Err(DndDataParseError::UnexpectedProtocol(uri.to_owned()));
    };
    if !path.starts_with('/') {
        return Err(DndDataParseError::HostnameSpecified(path.to_owned()));
    }
    let path = percent_decode_utf8(path.as_bytes())?;
    if path.contains('\0') {
        return Err(DndDataParseError::NulInPath);
    }
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_decodes_paths_after_framing_and_preserves_literal_uri_text() {
        let paths = uri_list_entries(
            b"#comment\r\nfile:///tmp/a%0D%0Afile:///tmp/b%20\r\nfile:///tmp/%23c\r\n",
        )
        .map(file_uri_path)
        .collect::<Result<Vec<_>, _>>()
        .expect("two local paths");
        assert_eq!(paths, [PathBuf::from("/tmp/a\r\nfile:///tmp/b "), PathBuf::from("/tmp/#c")]);
        assert!(matches!(
            file_uri_path(b"%66ile:///tmp/f"),
            Err(DndDataParseError::UnexpectedProtocol(_))
        ));
        assert!(matches!(
            file_uri_path(b"file://%2Ftmp/f"),
            Err(DndDataParseError::HostnameSpecified(_))
        ));
        assert!(matches!(
            file_uri_path(b"file:///tmp/%FF"),
            Err(DndDataParseError::InvalidUtf8(_))
        ));
        assert!(matches!(file_uri_path(b"file:///tmp/a%00b"), Err(DndDataParseError::NulInPath)));
    }

    #[test]
    fn a_list_keeps_its_local_files_and_skips_the_rest() {
        let list = b"https://example.com/cat.png\r\nfile:///tmp/Screenshot%20from%202026.png\r\n\
                     trash:///old.txt\r\nfile:///tmp/%FF\r\nfile://host/etc/hosts\r\n\
                     sftp://host/home/f\r\nfile:///tmp/a%00b\r\nfile:///tmp/missing\r\n";
        assert_eq!(local_paths(list), [
            PathBuf::from("/tmp/Screenshot from 2026.png"),
            PathBuf::from("/tmp/missing")
        ]);
        assert!(local_paths(b"https://example.com/\r\ntrash:///x\r\n").is_empty());
        assert!(local_paths(b"").is_empty());
    }
}

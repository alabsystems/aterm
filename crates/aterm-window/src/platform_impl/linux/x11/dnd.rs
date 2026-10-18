// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/dnd.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Percent-decoding
// through the backend's own helper, `common/percent.rs`, not the `percent-encoding` crate.
// The requests are aterm-x11's encoders, where upstream used x11rb's.)

use std::io;
use std::os::raw::*;
use std::path::PathBuf;
use std::str::Utf8Error;
use std::sync::Arc;

use aterm_x11::proto::{atom, xproto};

use super::atoms::AtomName::None as DndNone;
use super::atoms::*;
use super::{util, CookieResultExt, IgnoreError as _, X11Error, XConnection};
use crate::platform_impl::common::percent::{percent_decode_utf8, uri_list_entries};

#[derive(Debug, Clone, Copy)]
pub enum DndState {
    Accepted,
    Rejected,
}

#[derive(Debug)]
pub enum DndDataParseError {
    EmptyData,
    InvalidUtf8(#[allow(dead_code)] Utf8Error),
    HostnameSpecified(#[allow(dead_code)] String),
    UnexpectedProtocol(#[allow(dead_code)] String),
    UnresolvablePath(#[allow(dead_code)] io::Error),
}

impl From<Utf8Error> for DndDataParseError {
    fn from(e: Utf8Error) -> Self {
        DndDataParseError::InvalidUtf8(e)
    }
}

impl From<io::Error> for DndDataParseError {
    fn from(e: io::Error) -> Self {
        DndDataParseError::UnresolvablePath(e)
    }
}

pub struct Dnd {
    xconn: Arc<XConnection>,
    // Populated by XdndEnter event handler
    pub version: Option<c_long>,
    pub type_list: Option<Vec<xproto::Atom>>,
    // Populated by XdndPosition event handler
    pub source_window: Option<xproto::Window>,
    // Populated by SelectionNotify event handler (triggered by XdndPosition event handler)
    pub result: Option<Result<Vec<PathBuf>, DndDataParseError>>,
}

impl Dnd {
    pub fn new(xconn: Arc<XConnection>) -> Result<Self, X11Error> {
        Ok(Dnd { xconn, version: None, type_list: None, source_window: None, result: None })
    }

    pub fn reset(&mut self) {
        self.version = None;
        self.type_list = None;
        self.source_window = None;
        self.result = None;
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

    pub unsafe fn convert_selection(&self, window: xproto::Window, time: xproto::Timestamp) {
        let atoms = self.xconn.atoms();
        self.xconn
            .send_void(xproto::convert_selection(
                window,
                atoms[XdndSelection],
                atoms[TextUriList],
                atoms[XdndSelection],
                time,
            ))
            .expect_then_ignore_error("Failed to send XdndSelection event")
    }

    pub unsafe fn read_data(
        &self,
        window: xproto::Window,
    ) -> Result<Vec<c_uchar>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(window, atoms[XdndSelection], atoms[TextUriList])
    }

    pub fn parse_data(&self, data: &mut [c_uchar]) -> Result<Vec<PathBuf>, DndDataParseError> {
        if !data.is_empty() {
            let mut path_list = Vec::new();
            for uri in uri_list_entries(data) {
                path_list.push(file_uri_path(uri)?.canonicalize()?);
            }
            Ok(path_list)
        } else {
            Err(DndDataParseError::EmptyData)
        }
    }
}

/// Decode only the path of a local file URI; escaped bytes cannot change its
/// scheme or authority, and URI-looking text within the path stays intact.
fn file_uri_path(uri: &[u8]) -> Result<PathBuf, DndDataParseError> {
    let uri = std::str::from_utf8(uri)?;
    let Some(path) = uri.strip_prefix("file://") else {
        return Err(DndDataParseError::UnexpectedProtocol(uri.to_owned()));
    };
    if !path.starts_with('/') {
        return Err(DndDataParseError::HostnameSpecified(path.to_owned()));
    }
    Ok(PathBuf::from(percent_decode_utf8(path.as_bytes())?))
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
    }
}

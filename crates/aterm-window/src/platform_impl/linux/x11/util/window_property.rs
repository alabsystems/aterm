// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/window_property.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (GetProperty and ChangeProperty
// are aterm-x11's encoders, and a property's items `PropertyItem`s, where upstream used x11rb's
// requests and bytemuck's `Pod`.)

use std::error::Error;
use std::fmt;

use aterm_x11::proto::xproto::{GetPropertyReply, PropMode};

use super::*;

pub const CARDINAL_SIZE: usize = mem::size_of::<u32>();

pub type Cardinal = u32;

/// An 8-bit (`u8`) or 32-bit (`u32`) property item in wire byte order.
pub trait PropertyItem: Copy {
    /// The property format, in bits.
    const FORMAT: u8;

    /// Append the reply's whole items to `out`; a trailing partial item is dropped.
    fn extend_from(bytes: &[u8], out: &mut Vec<Self>);

    /// The items as the bytes ChangeProperty sends.
    fn to_bytes(items: &[Self]) -> Vec<u8>;
}

impl PropertyItem for u8 {
    const FORMAT: u8 = 8;

    fn extend_from(bytes: &[u8], out: &mut Vec<Self>) {
        out.extend_from_slice(bytes);
    }

    fn to_bytes(items: &[Self]) -> Vec<u8> {
        items.to_vec()
    }
}

impl PropertyItem for u32 {
    const FORMAT: u8 = 32;

    fn extend_from(bytes: &[u8], out: &mut Vec<Self>) {
        out.extend(bytes.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)));
    }

    fn to_bytes(items: &[Self]) -> Vec<u8> {
        items.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }
}

#[derive(Debug, Clone)]
pub enum GetPropertyError {
    X11Error(aterm_x11::Error),
    TypeMismatch(xproto::Atom),
    FormatMismatch(c_int),
}

impl GetPropertyError {
    pub fn is_actual_property_type(&self, t: xproto::Atom) -> bool {
        if let GetPropertyError::TypeMismatch(actual_type) = *self {
            actual_type == t
        } else {
            false
        }
    }
}

impl From<aterm_x11::Error> for GetPropertyError {
    fn from(e: aterm_x11::Error) -> Self {
        Self::X11Error(e)
    }
}

impl fmt::Display for GetPropertyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GetPropertyError::X11Error(err) => err.fmt(f),
            GetPropertyError::TypeMismatch(err) => write!(f, "type mismatch: {err}"),
            GetPropertyError::FormatMismatch(err) => write!(f, "format mismatch: {err}"),
        }
    }
}

impl Error for GetPropertyError {}

const PROPERTY_BUFFER_SIZE: u32 = 1024; // 4k of RAM ought to be enough for anyone!

impl XConnection {
    pub fn get_property<T: PropertyItem>(
        &self,
        window: xproto::Window,
        property: xproto::Atom,
        property_type: xproto::Atom,
    ) -> Result<Vec<T>, GetPropertyError> {
        let mut data = vec![];
        let mut offset = 0;
        loop {
            let reply: GetPropertyReply = self
                .xcb_connection()
                .send(xproto::get_property(
                    false,
                    window,
                    property,
                    property_type,
                    offset,
                    PROPERTY_BUFFER_SIZE,
                ))?
                .reply()?;

            if reply.type_ != property_type {
                return Err(GetPropertyError::TypeMismatch(reply.type_));
            }

            if reply.format != T::FORMAT {
                return Err(GetPropertyError::FormatMismatch(reply.format.into()));
            }

            T::extend_from(&reply.value, &mut data);

            if reply.bytes_after == 0 {
                break;
            }
            offset += PROPERTY_BUFFER_SIZE;
        }

        Ok(data)
    }

    pub fn change_property<'a, T: PropertyItem>(
        &'a self,
        window: xproto::Window,
        property: xproto::Atom,
        property_type: xproto::Atom,
        mode: PropMode,
        new_value: &[T],
    ) -> Result<VoidCookie<'a>, X11Error> {
        self.send_void(xproto::change_property(
            mode,
            window,
            property,
            property_type,
            T::FORMAT,
            &T::to_bytes(new_value),
        ))
    }
}

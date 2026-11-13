// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Desktop setting reader via `org.freedesktop.portal.Settings` over `aterm-dbus`.

use std::time::{Duration, Instant};

use aterm_dbus::message::flags::NO_AUTO_START;
use aterm_dbus::{Connection, Error, Message, Value};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";

const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

/// GNOME's interface schema and its animations key, which GTK desktops share.
pub(crate) const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
pub(crate) const ENABLE_ANIMATIONS: &str = "enable-animations";

/// Read GNOME's header button layout (`button-layout`, [`crate::client_frame::ButtonLayout`]).
pub(crate) const GNOME_WM_PREFERENCES: &str = "org.gnome.desktop.wm.preferences";
pub(crate) const BUTTON_LAYOUT: &str = "button-layout";

/// `namespace`'s `key` as the portal on the session bus answers it.
pub(crate) fn read(namespace: &str, key: &str, bound: Duration) -> Option<Value> {
    let addresses = aterm_dbus::address::session_bus().ok()?;
    read_at(&addresses, namespace, key, bound)
}

/// [`read`] on the bus at `addresses`.
pub(crate) fn read_at(
    addresses: &[aterm_dbus::address::Address],
    namespace: &str,
    key: &str,
    bound: Duration,
) -> Option<Value> {
    let deadline = Instant::now().checked_add(bound)?;
    let mut conn = Connection::open_within(addresses, bound).ok()?;
    read_on(&mut conn, namespace, key, deadline).ok()
}

fn read_on(
    conn: &mut Connection,
    namespace: &str,
    key: &str,
    deadline: Instant,
) -> aterm_dbus::Result<Value> {
    let mut ask = |member: &str| {
        let call = Message::method_call(Some(DESKTOP), PATH, Some(SETTINGS), member)?
            .with_flags(NO_AUTO_START)
            .with_body(vec![namespace.into(), key.into()]);
        conn.call(call, deadline.saturating_duration_since(Instant::now()))
    };
    let reply = match ask("ReadOne") {
        Err(Error::Remote { name, .. }) if name == UNKNOWN_METHOD => ask("Read")?,
        answered => answered?,
    };
    let mut value = reply
        .body
        .into_iter()
        .next()
        .ok_or(Error::Invalid("the Settings portal answered no value"))?;
    while let Value::Variant(inner) = value {
        value = *inner;
    }
    Ok(value)
}

/// Whether GNOME's `enable-animations`, as the portal answered it, is the person's reduced motion.
pub(crate) fn animations_disabled(answer: Option<&Value>) -> bool {
    matches!(answer, Some(Value::Bool(false)))
}

#[cfg(test)]
#[path = "portal_settings_tests.rs"]
mod tests;

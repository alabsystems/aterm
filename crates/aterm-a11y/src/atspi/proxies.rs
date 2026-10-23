// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The three calls the publisher makes of others, which accesskit_unix made through atspi-proxies.

use std::time::Duration;

use aterm_dbus::{Connection, Kind, Message, Value};

use super::serve::PROPERTIES;
use super::wire::{self, ObjectId, ROOT_PATH};

/// How long a call waits for its answer.
pub(crate) const CALL: Duration = Duration::from_secs(25);

const A11Y_BUS: &str = "org.a11y.Bus";
const A11Y_BUS_PATH: &str = "/org/a11y/bus";
const STATUS: &str = "org.a11y.Status";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const SOCKET: &str = "org.a11y.atspi.Socket";

/// The session-bus match rule for `org.a11y.Status` changes.
pub(crate) const STATUS_RULE: &str = "type='signal',sender='org.a11y.Bus',path='/org/a11y/bus',\
                                      interface='org.freedesktop.DBus.Properties',\
                                      member='PropertiesChanged',arg0='org.a11y.Status'";

/// The session-bus match rule for `org.a11y.Bus` changing hands.
pub(crate) const OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',\
                                     path='/org/freedesktop/DBus',interface='org.freedesktop.DBus',\
                                     member='NameOwnerChanged',arg0='org.a11y.Bus'";

fn call(
    conn: &mut Connection,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    body: Vec<Value>,
) -> aterm_dbus::Result<Message> {
    let msg = Message::method_call(Some(destination), path, Some(interface), member)?;
    conn.call(msg.with_body(body), CALL)
}

/// `org.a11y.Status.IsEnabled`.
pub(crate) fn is_enabled(session: &mut Connection) -> aterm_dbus::Result<bool> {
    let reply = call(
        session,
        A11Y_BUS,
        A11Y_BUS_PATH,
        PROPERTIES,
        "Get",
        vec![STATUS.into(), "IsEnabled".into()],
    )?;
    reply
        .body
        .first()
        .and_then(Value::as_variant)
        .and_then(Value::as_bool)
        .ok_or(aterm_dbus::Error::Invalid(
            "org.a11y.Status.IsEnabled is not a boolean",
        ))
}

/// The accessibility bus's address, from `org.a11y.Bus.GetAddress`.
pub(crate) fn bus_address(session: &mut Connection) -> aterm_dbus::Result<String> {
    let reply = call(
        session,
        A11Y_BUS,
        A11Y_BUS_PATH,
        A11Y_BUS,
        "GetAddress",
        Vec::new(),
    )?;
    reply
        .first_str()
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .ok_or(aterm_dbus::Error::Invalid(
            "org.a11y.Bus.GetAddress named no address",
        ))
}

/// Lists this application with the registry: `Socket.Embed` of its root.
pub(crate) fn embed(bus: &mut Connection) -> aterm_dbus::Result<()> {
    let plug = wire::object_ref(bus.unique_name(), ObjectId::Root);
    call(bus, REGISTRY, ROOT_PATH, SOCKET, "Embed", vec![plug]).map(drop)
}

/// Whether `msg` says the accessibility status may have changed.
pub(crate) fn status_may_have_changed(msg: &Message) -> bool {
    if msg.kind != Kind::Signal {
        return false;
    }
    match (msg.interface.as_deref(), msg.member.as_deref()) {
        (Some(PROPERTIES), Some("PropertiesChanged")) => {
            msg.path
                .as_ref()
                .is_some_and(|p| p.as_str() == A11Y_BUS_PATH)
                && msg.first_str() == Some(STATUS)
        }
        (Some("org.freedesktop.DBus"), Some("NameOwnerChanged")) => {
            msg.first_str() == Some(A11Y_BUS)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_status_and_launcher_signals_count() {
        let status = Message::signal(A11Y_BUS_PATH, PROPERTIES, "PropertiesChanged")
            .unwrap()
            .with_body(vec![STATUS.into()]);
        assert!(status_may_have_changed(&status));
        let other = Message::signal(A11Y_BUS_PATH, PROPERTIES, "PropertiesChanged")
            .unwrap()
            .with_body(vec!["org.a11y.Bus".into()]);
        assert!(!status_may_have_changed(&other));
        let owner = Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
        )
        .unwrap()
        .with_body(vec![A11Y_BUS.into(), "".into(), ":1.9".into()]);
        assert!(status_may_have_changed(&owner));
        let call = Message::method_call(None, A11Y_BUS_PATH, Some(PROPERTIES), "PropertiesChanged")
            .unwrap()
            .with_body(vec![STATUS.into()]);
        assert!(!status_may_have_changed(&call));
    }
}

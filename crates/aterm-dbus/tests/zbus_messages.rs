// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm-dbus` message encoding and decoding against recorded `zbus` 5.16.0 messages.

use std::collections::BTreeMap;

use crate::support::*;
use aterm_dbus::message::{self, flags};
use aterm_dbus::{Endian, Message, Type, Value};

const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const OBJECT: &str = "org.a11y.atspi.Event.Object";

fn call(
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    serial: u32,
    body: Vec<Value>,
) -> Message {
    let mut m = Message::method_call(Some(destination), path, Some(interface), member).unwrap();
    m.serial = serial;
    m.body = body;
    m
}

fn answer(mut m: Message, sender: Option<&str>, destination: Option<&str>, serial: u32) -> Message {
    m.sender = sender.map(Into::into);
    if let Some(destination) = destination {
        m.destination = Some(destination.into());
    }
    m.serial = serial;
    m
}

fn signal(path: &str, interface: &str, member: &str, serial: u32, body: Vec<Value>) -> Message {
    let mut m = Message::signal(path, interface, member).unwrap();
    m.serial = serial;
    m.body = body;
    m
}

fn big(mut m: Message) -> Message {
    m.endian = Endian::Big;
    m
}

fn cases() -> BTreeMap<&'static str, Message> {
    let hello = call(BUS, BUS_PATH, BUS, "Hello", 1, vec![]);
    let add_match = call(BUS, BUS_PATH, BUS, "AddMatch", 2, vec![RULE.into()]);
    let get_address = call(
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus",
        "GetAddress",
        3,
        vec![],
    );
    let get_prop = call(
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.freedesktop.DBus.Properties",
        "Get",
        4,
        vec!["org.a11y.Status".into(), "ScreenReaderEnabled".into()],
    );
    let embed = call(
        "org.a11y.atspi.Registry",
        ROOT,
        "org.a11y.atspi.Socket",
        "Embed",
        6,
        vec![object_ref(ME, ROOT)],
    );
    let mut get_role = call(
        ME,
        "/org/a11y/atspi/accessible/7",
        "org.a11y.atspi.Accessible",
        "GetRole",
        77,
        vec![],
    );
    get_role.sender = Some(":1.9".into());
    let mut get_items = call(
        ME,
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "GetItems",
        78,
        vec![],
    );
    get_items.sender = Some(":1.9".into());
    let mut get_role_be = big(get_role.clone());
    get_role_be.serial = 0x0102_0304;
    let mut no_reply = call(
        ":1.9",
        ROOT,
        "org.freedesktop.DBus.Peer",
        "Ping",
        15,
        vec![],
    );
    no_reply.flags = flags::NO_REPLY_EXPECTED;
    let cache_add = signal(
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "AddAccessible",
        9,
        terminal_item(),
    );
    let all = sv(vec![
        ("ChildCount", Value::I32(2)),
        ("Name", "Terminal".into()),
        ("Parent", object_ref(ME, ROOT)),
    ]);
    let entries = [
        ("hello", hello.clone()),
        (
            "hello_reply",
            answer(
                Message::method_return(&hello).with_body(vec![ME.into()]),
                Some(BUS),
                Some(ME),
                1,
            ),
        ),
        (
            "name_acquired",
            answer(
                signal(BUS_PATH, BUS, "NameAcquired", 0, vec![ME.into()]),
                Some(BUS),
                Some(ME),
                2,
            ),
        ),
        ("add_match", add_match.clone()),
        (
            "add_match_reply",
            answer(Message::method_return(&add_match), Some(BUS), Some(ME), 3),
        ),
        ("get_address", get_address.clone()),
        (
            "get_address_reply",
            answer(
                Message::method_return(&get_address).with_body(vec![AT_SPI_ADDRESS.into()]),
                Some(":1.7"),
                Some(ME),
                9,
            ),
        ),
        ("get_prop", get_prop.clone()),
        (
            "get_prop_error",
            answer(
                Message::error(
                    &get_prop,
                    "org.freedesktop.DBus.Error.UnknownProperty",
                    "Unknown property 'ScreenReaderEnabled'",
                )
                .unwrap(),
                Some(":1.7"),
                Some(ME),
                10,
            ),
        ),
        (
            "state_changed",
            signal(
                "/org/a11y/atspi/accessible/7",
                OBJECT,
                "StateChanged",
                5,
                event_body("focused", 1, 0, Value::U32(0)),
            ),
        ),
        (
            "get_prop_reply",
            answer(
                Message::method_return(&get_prop).with_body(vec![Value::variant(true.into())]),
                Some(":1.7"),
                Some(ME),
                11,
            ),
        ),
        ("embed", embed.clone()),
        (
            "embed_reply",
            answer(
                Message::method_return(&embed).with_body(vec![object_ref(":1.3", ROOT)]),
                Some(":1.3"),
                Some(ME),
                40,
            ),
        ),
        (
            "children_changed",
            signal(
                ROOT,
                OBJECT,
                "ChildrenChanged",
                7,
                event_body("add", 3, 0, object_ref(ME, "/org/a11y/atspi/accessible/9")),
            ),
        ),
        (
            "text_changed",
            signal(
                "/org/a11y/atspi/accessible/7",
                OBJECT,
                "TextChanged",
                8,
                event_body("insert", 0, 5, "hello".into()),
            ),
        ),
        ("cache_add", cache_add.clone()),
        (
            "props_changed",
            answer(
                signal(
                    "/org/a11y/bus",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    0,
                    vec![
                        "org.a11y.Status".into(),
                        sv(vec![("IsEnabled", true.into())]),
                        Value::strings(Vec::<String>::new()),
                    ],
                ),
                Some(":1.7"),
                None,
                12,
            ),
        ),
        ("get_role", get_role.clone()),
        (
            "get_role_reply",
            answer(
                Message::method_return(&get_role).with_body(vec![43u32.into()]),
                None,
                None,
                10,
            ),
        ),
        (
            "get_role_error",
            answer(
                Message::error(
                    &get_role,
                    "org.freedesktop.DBus.Error.UnknownObject",
                    "Unknown object '/org/a11y/atspi/accessible/7'",
                )
                .unwrap(),
                None,
                None,
                11,
            ),
        ),
        (
            "get_items_reply",
            answer(
                Message::method_return(&get_items).with_body(vec![
                    Value::array(cache_item_type(), vec![Value::Struct(terminal_item())]).unwrap(),
                ]),
                None,
                None,
                12,
            ),
        ),
        (
            "get_string_reply",
            answer(
                Message::method_return(&get_items).with_body(vec![
                    "word".into(),
                    0.into(),
                    4.into(),
                ]),
                None,
                None,
                13,
            ),
        ),
        (
            "get_all_reply",
            answer(
                Message::method_return(&get_items).with_body(vec![all]),
                None,
                None,
                14,
            ),
        ),
        ("no_reply_ping", no_reply),
        ("hello_be", big(hello)),
        ("get_role_be", get_role_be.clone()),
        (
            "get_role_be_reply",
            answer(
                Message::method_return(&get_role_be).with_body(vec![43u32.into()]),
                None,
                None,
                10,
            ),
        ),
        ("cache_add_be", big(cache_add)),
    ];
    let mut map = BTreeMap::new();
    for (name, m) in entries {
        assert!(map.insert(name, m).is_none(), "case {name} twice");
    }
    map
}

fn recorded() -> Vec<(String, Vec<u8>)> {
    rows("zbus-5.16.0-messages.tsv")
        .into_iter()
        .map(|r| (r[0].clone(), unhex(&r[1])))
        .collect()
}

#[test]
fn every_case_is_recorded_once_and_nothing_else_is() {
    let cases = cases();
    let rows = recorded();
    let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.len(), cases.len());
    for name in cases.keys() {
        assert_eq!(names.iter().filter(|n| *n == name).count(), 1, "{name}");
    }
}

#[test]
fn every_message_encodes_to_zbus_bytes() {
    let cases = cases();
    for (name, bytes) in recorded() {
        let ours = cases[name.as_str()].encode().unwrap();
        assert_eq!(hex(&ours), hex(&bytes), "{name}");
    }
}

#[test]
fn zbus_bytes_decode_to_every_message() {
    let cases = cases();
    for (name, bytes) in recorded() {
        assert_eq!(
            message::frame_len(&bytes).unwrap(),
            Some(bytes.len()),
            "{name}"
        );
        assert_eq!(
            Message::decode(&bytes).unwrap(),
            cases[name.as_str()],
            "{name}"
        );
    }
}

#[test]
fn the_reply_takes_the_calls_byte_order_and_sender() {
    let cases = cases();
    let call = &cases["get_role_be"];
    let reply = Message::method_return(call);
    assert_eq!(
        (
            reply.endian,
            reply.reply_serial,
            reply.destination.as_deref()
        ),
        (Endian::Big, Some(0x0102_0304), Some(":1.9"))
    );
    assert_eq!(cases["cache_add"].signature(), "(so)(so)(so)iiassusau");
    assert_eq!(
        cases["get_items_reply"].body[0].type_of(),
        Type::array(cache_item_type())
    );
}

#[test]
fn a_message_one_field_away_misses_its_recording() {
    let cases = cases();
    for (name, bytes) in recorded() {
        let case = &cases[name.as_str()];
        let mut serial = case.clone();
        serial.serial += 1;
        let mut order = case.clone();
        order.endian = if case.endian == Endian::Big {
            Endian::Little
        } else {
            Endian::Big
        };
        let mut variants = vec![serial, order];
        if case.sender.is_some() {
            let mut anonymous = case.clone();
            anonymous.sender = None;
            variants.push(anonymous);
        }
        for near in variants {
            assert_ne!(near.encode().unwrap(), bytes, "{name}: {near:?}");
            assert_ne!(Message::decode(&bytes).unwrap(), near, "{name}");
        }
    }
}

#[test]
fn no_bit_flip_of_a_recording_decodes_to_its_message() {
    let cases = cases();
    let mut flips = 0;
    for (name, bytes) in recorded() {
        let case = &cases[name.as_str()];
        for i in 0..bytes.len() {
            for bit in 0..8 {
                let mut flipped = bytes.clone();
                flipped[i] ^= 1 << bit;
                flips += 1;
                if let Ok(m) = Message::decode(&flipped) {
                    assert_ne!(
                        &m, case,
                        "{name} byte {i} bit {bit} decoded to the recorded message"
                    );
                }
            }
        }
    }
    assert!(flips > 20_000, "only {flips} flips");
}

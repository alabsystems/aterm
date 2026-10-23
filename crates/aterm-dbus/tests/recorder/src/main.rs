// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus oracle recorder that emits serialized value, message, and session TSV
//! fixtures using upstream `zbus` and `zvariant`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::num::NonZeroU32;

use serde::Serialize;
use zbus::message::{Flags, Message};
use zvariant::serialized::Context;
use zvariant::{DynamicType, Endian, ObjectPath, Signature, StructureBuilder, Value};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn op(s: &str) -> ObjectPath<'_> {
    ObjectPath::try_from(s).unwrap()
}

fn rec<T: Serialize + DynamicType>(name: &str, offset: usize, v: &T) {
    for (e, tag) in [(Endian::Little, "l"), (Endian::Big, "B")] {
        let ctxt = Context::new_dbus(e, offset);
        let data = zvariant::to_bytes(ctxt, v).unwrap();
        println!(
            "{name}\t{}\t{tag}\t{offset}\t{}",
            v.signature(),
            hex(data.bytes())
        );
    }
}

type Ref<'a> = (&'a str, ObjectPath<'a>);

fn r(name: &'static str, path: &'static str) -> Ref<'static> {
    (name, op(path))
}

#[allow(clippy::type_complexity)]
fn cache_item(
    node: &'static str,
    parent: &'static str,
    index: i32,
    children: i32,
    ifaces: Vec<&'static str>,
    role: u32,
    name: &'static str,
    states: Vec<u32>,
) -> (
    Ref<'static>,
    Ref<'static>,
    Ref<'static>,
    i32,
    i32,
    Vec<&'static str>,
    &'static str,
    u32,
    &'static str,
    Vec<u32>,
) {
    (
        r(":1.42", node),
        r(":1.42", "/org/a11y/atspi/accessible/root"),
        r(":1.42", parent),
        index,
        children,
        ifaces,
        "",
        role,
        name,
        states,
    )
}

fn values() {
    println!("# name\tsignature\tendian\toffset\thex");
    rec("y_00", 0, &0u8);
    rec("y_ff", 0, &0xffu8);
    rec("b_true", 0, &true);
    rec("b_false", 0, &false);
    rec("n_neg2", 0, &-2i16);
    rec("n_min", 0, &i16::MIN);
    rec("q_beef", 0, &0xbeefu16);
    rec("q_at1", 1, &7u16);
    rec("i_neg1", 0, &-1i32);
    rec("i_1234567", 0, &1_234_567i32);
    rec("i_min", 0, &i32::MIN);
    rec("u_0", 0, &0u32);
    rec("u_max", 0, &u32::MAX);
    rec("u_at2", 2, &0xdead_beefu32);
    rec("x_min", 0, &i64::MIN);
    rec("x_at4", 4, &-5i64);
    rec("t_max", 0, &u64::MAX);
    rec("t_seq", 0, &0x0102_0304_0506_0708u64);
    rec("d_half", 0, &0.5f64);
    rec("d_big", 0, &-1.25e300f64);
    rec("d_negzero", 0, &-0.0f64);
    rec("s_empty", 0, &"");
    rec("s_iface", 0, &"org.a11y.atspi.Accessible");
    rec("s_utf8", 0, &"h\u{e9}llo \u{2713} \u{1d11e}");
    rec("s_at3", 3, &"abc");
    rec("o_slash", 0, &op("/"));
    rec("o_root", 0, &op("/org/a11y/atspi/accessible/root"));
    rec("g_empty", 0, &Signature::try_from("").unwrap());
    rec("g_asv", 0, &Signature::try_from("a{sv}").unwrap());
    rec(
        "g_cache",
        0,
        &Signature::try_from("((so)(so)(so)iiassusau)").unwrap(),
    );
    rec("v_u", 0, &Value::U32(7));
    rec("v_s", 0, &Value::from("hello"));
    let so = StructureBuilder::new()
        .add_field(":1.42")
        .add_field(op("/org/a11y/atspi/accessible/9"))
        .build()
        .unwrap();
    rec("v_so", 0, &Value::Structure(so));
    rec("v_v_i", 0, &Value::Value(Box::new(Value::I32(-3))));
    rec("v_as", 0, &Value::from(vec!["a", "bc"]));
    let iiii = StructureBuilder::new()
        .add_field(10i32)
        .add_field(-20i32)
        .add_field(300i32)
        .add_field(40i32)
        .build()
        .unwrap();
    rec("v_iiii", 0, &Value::Structure(iiii));
    rec("v_t_at1", 1, &Value::U64(1));
    rec(
        "v_g",
        0,
        &Value::Signature(Signature::try_from("a(so)").unwrap()),
    );
    rec("v_o", 0, &Value::ObjectPath(op("/org/a11y/bus")));
    rec("ay_empty", 0, &Vec::<u8>::new());
    rec("ay_123", 0, &vec![1u8, 2, 3]);
    rec("as_empty", 0, &Vec::<&str>::new());
    rec(
        "as_ifaces",
        0,
        &vec![
            "org.a11y.atspi.Accessible",
            "org.a11y.atspi.Component",
            "org.a11y.atspi.Text",
        ],
    );
    rec("au_states", 0, &vec![0x0040_0102u32, 1]);
    rec("ai_signs", 0, &vec![-1i32, 0, 1]);
    rec("ab", 0, &vec![true, false, true]);
    rec("ax_pair", 0, &vec![i64::MIN, 0]);
    rec("ax_at4", 4, &vec![7i64]);
    rec("a_ii_empty", 0, &Vec::<(i32, i32)>::new());
    rec("a_ii_empty_at4", 4, &Vec::<(i32, i32)>::new());
    rec("so_root", 0, &r(":1.42", "/org/a11y/atspi/accessible/root"));
    rec("a_so_empty", 0, &Vec::<Ref<'static>>::new());
    rec(
        "a_so_two",
        0,
        &vec![
            r(":1.42", "/org/a11y/atspi/accessible/1"),
            r(":1.42", "/org/a11y/atspi/accessible/22"),
        ],
    );
    rec("ii", 0, &(3i32, 4i32));
    rec("iiii", 0, &(10i32, -20i32, 300i32, 40i32));
    rec("sii", 0, &("word", 0i32, 4i32));
    rec("yt", 0, &(1u8, 2u64));
    rec("y_t_nested", 0, &(9u8, (0x1122_3344_5566_7788u64,)));
    rec("a_sss", 0, &vec![("click", "Click the button", "")]);
    let mut ss = BTreeMap::new();
    ss.insert("level", "1");
    ss.insert("toolkit", "aterm");
    rec("a_ss", 0, &ss);
    rec("a_ss_empty", 0, &BTreeMap::<&str, &str>::new());
    let mut sv = BTreeMap::new();
    sv.insert("IsEnabled", Value::Bool(true));
    sv.insert("ScreenReaderEnabled", Value::Bool(false));
    rec("a_sv", 0, &sv);
    rec("a_sv_empty", 0, &BTreeMap::<&str, Value>::new());
    let mut yd = BTreeMap::new();
    yd.insert(1u8, 0.25f64);
    yd.insert(2u8, -8.0f64);
    rec("a_yd", 0, &yd);
    rec(
        "a_u_a_so",
        0,
        &vec![
            (15u32, vec![r(":1.42", "/org/a11y/atspi/accessible/3")]),
            (16u32, Vec::<Ref<'static>>::new()),
        ],
    );
    rec(
        "event_body",
        0,
        &(
            "focused",
            1i32,
            0i32,
            Value::U32(0),
            BTreeMap::<&str, Value>::new(),
        ),
    );
    let item = cache_item(
        "/org/a11y/atspi/accessible/7",
        "/org/a11y/atspi/accessible/root",
        0,
        2,
        vec!["org.a11y.atspi.Accessible", "org.a11y.atspi.Text"],
        43,
        "Terminal",
        vec![0x0000_4002, 0],
    );
    rec("cache_item", 0, &item);
    let item2 = cache_item(
        "/org/a11y/atspi/accessible/8",
        "/org/a11y/atspi/accessible/7",
        1,
        0,
        vec![
            "org.a11y.atspi.Accessible",
            "org.a11y.atspi.Action",
            "org.a11y.atspi.Hyperlink",
        ],
        88,
        "https://example.org/",
        vec![0, 0],
    );
    rec("a_cache_items", 0, &vec![item.clone(), item2]);
    let mut changed = BTreeMap::new();
    changed.insert("IsEnabled", Value::Bool(true));
    rec(
        "props_changed",
        0,
        &("org.a11y.Status", changed, Vec::<&str>::new()),
    );
    rec("aay", 0, &vec![vec![1u8], vec![], vec![2u8, 3]]);
}

fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

const RULE: &str = "type='signal',sender='org.a11y.Bus',path='/org/a11y/bus',\
                    interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',\
                    arg0='org.a11y.Status'";
const AT_SPI_ADDRESS: &str =
    "unix:path=/run/user/1000/at-spi/bus_0,guid=0f3b26c3a5fe4e1f9b8a3e1c7d6e5f40";

fn messages() -> Vec<(&'static str, Message)> {
    let mut out = Vec::new();
    let bus = "org.freedesktop.DBus";
    let me = ":1.42";

    let hello = Message::method_call("/org/freedesktop/DBus", "Hello")
        .unwrap()
        .destination(bus)
        .unwrap()
        .interface(bus)
        .unwrap()
        .serial(nz(1))
        .build(&())
        .unwrap();
    let hello_reply = Message::method_return(&hello.header())
        .unwrap()
        .sender(bus)
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(1))
        .build(&(me,))
        .unwrap();
    let name_acquired = Message::signal("/org/freedesktop/DBus", bus, "NameAcquired")
        .unwrap()
        .sender(bus)
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(2))
        .build(&(me,))
        .unwrap();
    let add_match = Message::method_call("/org/freedesktop/DBus", "AddMatch")
        .unwrap()
        .destination(bus)
        .unwrap()
        .interface(bus)
        .unwrap()
        .serial(nz(2))
        .build(&(RULE,))
        .unwrap();
    let add_match_reply = Message::method_return(&add_match.header())
        .unwrap()
        .sender(bus)
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(3))
        .build(&())
        .unwrap();
    let get_address = Message::method_call("/org/a11y/bus", "GetAddress")
        .unwrap()
        .destination("org.a11y.Bus")
        .unwrap()
        .interface("org.a11y.Bus")
        .unwrap()
        .serial(nz(3))
        .build(&())
        .unwrap();
    let get_address_reply = Message::method_return(&get_address.header())
        .unwrap()
        .sender(":1.7")
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(9))
        .build(&(AT_SPI_ADDRESS,))
        .unwrap();
    let get_prop = Message::method_call("/org/a11y/bus", "Get")
        .unwrap()
        .destination("org.a11y.Bus")
        .unwrap()
        .interface("org.freedesktop.DBus.Properties")
        .unwrap()
        .serial(nz(4))
        .build(&("org.a11y.Status", "ScreenReaderEnabled"))
        .unwrap();
    let get_prop_error = Message::error(
        &get_prop.header(),
        "org.freedesktop.DBus.Error.UnknownProperty",
    )
    .unwrap()
    .sender(":1.7")
    .unwrap()
    .destination(me)
    .unwrap()
    .serial(nz(10))
    .build(&("Unknown property 'ScreenReaderEnabled'",))
    .unwrap();
    let state_changed = Message::signal(
        "/org/a11y/atspi/accessible/7",
        "org.a11y.atspi.Event.Object",
        "StateChanged",
    )
    .unwrap()
    .serial(nz(5))
    .build(&(
        "focused",
        1i32,
        0i32,
        Value::U32(0),
        BTreeMap::<&str, Value>::new(),
    ))
    .unwrap();
    out.push(("hello", hello.clone()));
    out.push(("hello_reply", hello_reply));
    out.push(("name_acquired", name_acquired));
    out.push(("add_match", add_match));
    out.push(("add_match_reply", add_match_reply));
    out.push(("get_address", get_address));
    out.push(("get_address_reply", get_address_reply));
    out.push(("get_prop", get_prop.clone()));
    out.push(("get_prop_error", get_prop_error));
    out.push(("state_changed", state_changed));

    let get_prop_reply = Message::method_return(&get_prop.header())
        .unwrap()
        .sender(":1.7")
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(11))
        .build(&(Value::Bool(true),))
        .unwrap();
    out.push(("get_prop_reply", get_prop_reply));
    let embed = Message::method_call("/org/a11y/atspi/accessible/root", "Embed")
        .unwrap()
        .destination("org.a11y.atspi.Registry")
        .unwrap()
        .interface("org.a11y.atspi.Socket")
        .unwrap()
        .serial(nz(6))
        .build(&(r(me, "/org/a11y/atspi/accessible/root"),))
        .unwrap();
    let embed_reply = Message::method_return(&embed.header())
        .unwrap()
        .sender(":1.3")
        .unwrap()
        .destination(me)
        .unwrap()
        .serial(nz(40))
        .build(&(r(":1.3", "/org/a11y/atspi/accessible/root"),))
        .unwrap();
    out.push(("embed", embed));
    out.push(("embed_reply", embed_reply));
    let children_changed = Message::signal(
        "/org/a11y/atspi/accessible/root",
        "org.a11y.atspi.Event.Object",
        "ChildrenChanged",
    )
    .unwrap()
    .serial(nz(7))
    .build(&(
        "add",
        3i32,
        0i32,
        Value::Structure(
            StructureBuilder::new()
                .add_field(me)
                .add_field(op("/org/a11y/atspi/accessible/9"))
                .build()
                .unwrap(),
        ),
        BTreeMap::<&str, Value>::new(),
    ))
    .unwrap();
    out.push(("children_changed", children_changed));
    let text_changed = Message::signal(
        "/org/a11y/atspi/accessible/7",
        "org.a11y.atspi.Event.Object",
        "TextChanged",
    )
    .unwrap()
    .serial(nz(8))
    .build(&(
        "insert",
        0i32,
        5i32,
        Value::from("hello"),
        BTreeMap::<&str, Value>::new(),
    ))
    .unwrap();
    out.push(("text_changed", text_changed));
    let item = cache_item(
        "/org/a11y/atspi/accessible/7",
        "/org/a11y/atspi/accessible/root",
        0,
        2,
        vec!["org.a11y.atspi.Accessible", "org.a11y.atspi.Text"],
        43,
        "Terminal",
        vec![0x0000_4002, 0],
    );
    let cache_add = Message::signal(
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "AddAccessible",
    )
    .unwrap()
    .serial(nz(9))
    .build(&item)
    .unwrap();
    out.push(("cache_add", cache_add));
    let props_changed = Message::signal(
        "/org/a11y/bus",
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
    )
    .unwrap()
    .sender(":1.7")
    .unwrap()
    .serial(nz(12))
    .build(&(
        "org.a11y.Status",
        {
            let mut m = BTreeMap::new();
            m.insert("IsEnabled", Value::Bool(true));
            m
        },
        Vec::<&str>::new(),
    ))
    .unwrap();
    out.push(("props_changed", props_changed));

    let get_role = Message::method_call("/org/a11y/atspi/accessible/7", "GetRole")
        .unwrap()
        .sender(":1.9")
        .unwrap()
        .destination(me)
        .unwrap()
        .interface("org.a11y.atspi.Accessible")
        .unwrap()
        .serial(nz(77))
        .build(&())
        .unwrap();
    let get_role_reply = Message::method_return(&get_role.header())
        .unwrap()
        .serial(nz(10))
        .build(&(43u32,))
        .unwrap();
    let get_role_error = Message::error(
        &get_role.header(),
        "org.freedesktop.DBus.Error.UnknownObject",
    )
    .unwrap()
    .serial(nz(11))
    .build(&("Unknown object '/org/a11y/atspi/accessible/7'",))
    .unwrap();
    out.push(("get_role", get_role));
    out.push(("get_role_reply", get_role_reply));
    out.push(("get_role_error", get_role_error));
    let get_items = Message::method_call("/org/a11y/atspi/cache", "GetItems")
        .unwrap()
        .sender(":1.9")
        .unwrap()
        .destination(me)
        .unwrap()
        .interface("org.a11y.atspi.Cache")
        .unwrap()
        .serial(nz(78))
        .build(&())
        .unwrap();
    let get_items_reply = Message::method_return(&get_items.header())
        .unwrap()
        .serial(nz(12))
        .build(&(vec![item.clone()],))
        .unwrap();
    out.push(("get_items_reply", get_items_reply));
    let get_string = Message::method_return(&get_items.header())
        .unwrap()
        .serial(nz(13))
        .build(&("word", 0i32, 4i32))
        .unwrap();
    out.push(("get_string_reply", get_string));
    let get_all = Message::method_return(&get_items.header())
        .unwrap()
        .serial(nz(14))
        .build(&({
            let mut m = BTreeMap::new();
            m.insert("ChildCount", Value::I32(2));
            m.insert("Name", Value::from("Terminal"));
            m.insert(
                "Parent",
                Value::Structure(
                    StructureBuilder::new()
                        .add_field(me)
                        .add_field(op("/org/a11y/atspi/accessible/root"))
                        .build()
                        .unwrap(),
                ),
            );
            m
        },))
        .unwrap();
    out.push(("get_all_reply", get_all));
    let no_reply = Message::method_call("/org/a11y/atspi/accessible/root", "Ping")
        .unwrap()
        .destination(":1.9")
        .unwrap()
        .interface("org.freedesktop.DBus.Peer")
        .unwrap()
        .with_flags(Flags::NoReplyExpected)
        .unwrap()
        .serial(nz(15))
        .build(&())
        .unwrap();
    out.push(("no_reply_ping", no_reply));

    let hello_be = Message::method_call("/org/freedesktop/DBus", "Hello")
        .unwrap()
        .destination(bus)
        .unwrap()
        .interface(bus)
        .unwrap()
        .endian(Endian::Big)
        .serial(nz(1))
        .build(&())
        .unwrap();
    out.push(("hello_be", hello_be));
    let get_role_be = Message::method_call("/org/a11y/atspi/accessible/7", "GetRole")
        .unwrap()
        .sender(":1.9")
        .unwrap()
        .destination(me)
        .unwrap()
        .interface("org.a11y.atspi.Accessible")
        .unwrap()
        .endian(Endian::Big)
        .serial(nz(0x0102_0304))
        .build(&())
        .unwrap();
    let get_role_be_reply = Message::method_return(&get_role_be.header())
        .unwrap()
        .serial(nz(10))
        .build(&(43u32,))
        .unwrap();
    out.push(("get_role_be", get_role_be));
    out.push(("get_role_be_reply", get_role_be_reply));
    let cache_add_be = Message::signal(
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "AddAccessible",
    )
    .unwrap()
    .endian(Endian::Big)
    .serial(nz(9))
    .build(&item)
    .unwrap();
    out.push(("cache_add_be", cache_add_be));
    out
}

fn print_messages() {
    println!("# name\thex");
    for (name, m) in messages() {
        println!("{name}\t{}", hex(m.data().bytes()));
    }
}

/// One whole message off the stream, by its own length fields.
fn read_message(s: &mut impl Read) -> Option<Vec<u8>> {
    let mut head = [0u8; 16];
    if s.read_exact(&mut head).is_err() {
        return None;
    }
    let le = head[0] == b'l';
    let u = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };
    let body = u(&head[4..8]) as usize;
    let fields = u(&head[12..16]) as usize;
    let header = (16 + fields).div_ceil(8) * 8;
    let mut rest = vec![0u8; header - 16 + body];
    s.read_exact(&mut rest).unwrap();
    let mut all = head.to_vec();
    all.extend(rest);
    Some(all)
}

fn read_line(s: &mut impl Read) -> Vec<u8> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        s.read_exact(&mut b).unwrap();
        line.push(b[0]);
    }
    line
}

const GUID: &str = "8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f";

/// The scripted bus: answers the SASL lines as dbus-daemon does, then plays the bus's recorded
/// messages. Prints every unit both ways, in order.
fn fake_bus(mut s: impl Read + Write, tag: &str, script: &BTreeMap<String, Vec<u8>>) {
    let emit = |dir: &str, kind: &str, b: &[u8]| println!("{tag}\t{dir}\t{kind}\t{}", hex(b));
    let auth = read_line(&mut s);
    emit("c2s", "line", &auth);
    let ok = format!("OK {GUID}\r\n");
    s.write_all(ok.as_bytes()).unwrap();
    emit("s2c", "line", ok.as_bytes());
    loop {
        let line = read_line(&mut s);
        emit("c2s", "line", &line);
        if line == b"NEGOTIATE_UNIX_FD\r\n" {
            s.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
            emit("s2c", "line", b"AGREE_UNIX_FD\r\n");
        } else if line == b"BEGIN\r\n" {
            break;
        } else {
            panic!("unexpected line {line:?}");
        }
    }
    let answers: [&[&str]; 5] = [
        &["hello_reply", "name_acquired"],
        &["add_match_reply"],
        &["get_address_reply"],
        &["get_prop_error"],
        &[],
    ];
    let expect = [
        "hello",
        "add_match",
        "get_address",
        "get_prop",
        "state_changed",
    ];
    for (i, replies) in answers.iter().enumerate() {
        let m = read_message(&mut s).expect("client message");
        emit("c2s", "msg", &m);
        if m != script[expect[i]] {
            eprintln!(
                "note: client message {i} differs from the standalone `{}` row",
                expect[i]
            );
        }
        for name in *replies {
            let bytes = &script[*name];
            s.write_all(bytes).unwrap();
            emit("s2c", "msg", bytes);
        }
    }
    let mut tail = Vec::new();
    s.read_to_end(&mut tail).unwrap();
    assert!(tail.is_empty(), "client sent more: {}", hex(&tail));
    println!("{tag}\tc2s\teof\t");
}

fn client_script(conn: zbus::blocking::Connection) {
    assert_eq!(conn.unique_name().unwrap().as_str(), ":1.42");
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "AddMatch",
        &(RULE,),
    )
    .unwrap();
    let reply = conn
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .unwrap();
    let address: String = reply.body().deserialize().unwrap();
    assert_eq!(address, AT_SPI_ADDRESS);
    let err = conn
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.a11y.Status", "ScreenReaderEnabled"),
        )
        .unwrap_err();
    eprintln!("get_prop error as zbus reports it: {err}");
    conn.emit_signal(
        Option::<&str>::None,
        "/org/a11y/atspi/accessible/7",
        "org.a11y.atspi.Event.Object",
        "StateChanged",
        &(
            "focused",
            1i32,
            0i32,
            Value::U32(0),
            BTreeMap::<&str, Value>::new(),
        ),
    )
    .unwrap();
    conn.close().unwrap();
}

fn session(msgs: &str, transport: &str) {
    let mut script = BTreeMap::new();
    for line in std::fs::read_to_string(msgs).unwrap().lines() {
        if line.starts_with('#') {
            continue;
        }
        let (name, h) = line.split_once('\t').unwrap();
        script.insert(name.to_string(), unhex(h));
    }
    match transport {
        "unix" => {
            let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || fake_bus(b, "unix", &script));
            let conn = zbus::blocking::connection::Builder::unix_stream(a)
                .user_id(1000)
                .build()
                .unwrap();
            client_script(conn);
            server.join().unwrap();
        }
        "tcp" => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (s, _) = listener.accept().unwrap();
                fake_bus(s, "nofd", &script)
            });
            let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let conn = zbus::blocking::connection::Builder::tcp_stream(stream)
                .auth_mechanism(zbus::AuthMechanism::External)
                .user_id(1000)
                .build()
                .unwrap();
            client_script(conn);
            server.join().unwrap();
        }
        other => panic!("transport {other}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("values") => values(),
        Some("messages") => print_messages(),
        Some("session") => session(&args[2], &args[3]),
        _ => panic!("usage: values | messages | session <messages.tsv> unix|tcp"),
    }
}

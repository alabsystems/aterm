// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual dequeue/enqueue and Unix wire delivery, with only the private endpoint injected.
//! This exercises no desktop service or D-Bus daemon.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use aterm_dbus::{Connection, Kind, Message, Value};
use aterm_uds::CtlStream;

use super::{DeliveryPolicy, NotifyMsg, admit_delivery, delivers, freedesktop, post_admitted};
use crate::consent_observer::{AttentionEvent, AttentionGate, ObserverMode};

const HANG: Duration = Duration::from_secs(60);
const BARRIER: &str = "delivery-policy barrier";

fn line(stream: &mut CtlStream) -> Vec<u8> {
    let mut line = Vec::new();
    while !line.ends_with(b"\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        line.push(byte[0]);
        assert!(line.len() <= aterm_dbus::auth::MAX_LINE);
    }
    line
}

fn message(stream: &mut CtlStream) -> Message {
    let mut frame = vec![0; 16];
    stream.read_exact(&mut frame).unwrap();
    let size = aterm_dbus::message::frame_len(&frame)
        .unwrap()
        .expect("the fixed header names its frame");
    assert!((16..=1 << 20).contains(&size));
    frame.resize(size, 0);
    stream.read_exact(&mut frame[16..]).unwrap();
    Message::decode(&frame).unwrap()
}

fn reply(stream: &mut CtlStream, serial: &mut u32, call: &Message, body: Vec<Value>) {
    let mut answer = Message::method_return(call).with_body(body);
    answer.serial = *serial;
    answer.sender = Some(":1.1".to_owned());
    *serial += 1;
    stream.write_all(&answer.encode().unwrap()).unwrap();
}

/// The existing connection_identity test's socket-pair handshake, followed by
/// the real Poster's AddMatch and Notify traffic. FIFO_B closes the observation.
fn serve(mut stream: CtlStream) -> Vec<Message> {
    stream.set_read_timeout(Some(HANG)).unwrap();
    stream.set_write_timeout(Some(HANG)).unwrap();
    assert!(line(&mut stream).starts_with(b"\0AUTH EXTERNAL "));
    stream
        .write_all(b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f\r\n")
        .unwrap();
    assert_eq!(line(&mut stream), b"BEGIN\r\n");
    let hello = message(&mut stream);
    assert_eq!(hello.kind, Kind::MethodCall);
    assert_eq!(hello.member.as_deref(), Some("Hello"));
    let mut serial = 1;
    reply(&mut stream, &mut serial, &hello, vec![Value::from(":1.42")]);
    let subscribe = message(&mut stream);
    assert_eq!(subscribe.member.as_deref(), Some("AddMatch"));
    reply(&mut stream, &mut serial, &subscribe, Vec::new());
    let mut seen = Vec::new();
    loop {
        let notify = message(&mut stream);
        assert_eq!(notify.kind, Kind::MethodCall);
        assert_eq!(notify.member.as_deref(), Some("Notify"));
        let barrier = notify.body[3].as_str() == Some(BARRIER);
        let id = serial;
        reply(&mut stream, &mut serial, &notify, vec![Value::U32(id)]);
        seen.push(notify);
        if barrier {
            return seen;
        }
    }
}

fn quiet(message: &Message) -> bool {
    message.body[6]
        .as_dict()
        .expect("Notify's typed hint dictionary")
        .entries()
        .iter()
        .any(|(key, value)| {
            key.as_str() == Some("suppress-sound") && *value == Value::variant(Value::Bool(true))
        })
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Unchanged,
    Focus,
    DesktopOff,
    Serious,
}

fn queued_case(change: Change) -> Vec<Message> {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).unwrap().focused = true;
    for _ in 0..2 {
        let session = crate::stub_session(app.next_session_id);
        app.push_stub_tab(wid, session);
    }
    app.switch_tab_in(wid, 0);
    app.switch_tab_in(wid, 2);
    app.config.desktop_alerts = Some(true);
    app.apply_desktop_alerts(true);
    app.apply_serious_mode(false);
    let policy = DeliveryPolicy {
        suppress: Arc::clone(&app.notify_suppress),
        silent: Arc::clone(&app.notify_silent),
        own_alerts: Arc::clone(&app.notify_own_alerts),
    };
    assert_eq!(*policy.suppress.lock().unwrap(), HashSet::from([2]));
    assert!(!policy.silent.load(std::sync::atomic::Ordering::Acquire));
    let mut gate = AttentionGate::new();
    let notice = matches!(change, Change::DesktopOff).then(|| {
        gate.note(AttentionEvent::ProtectedEperm { session: None })
            .for_mode(ObserverMode::Full)
            .notice
            .expect("the actual unattributed denial producer")
    });
    let a = notice.as_ref().map_or_else(
        || NotifyMsg::program(0, Some("policy A".into()), "program event".into()),
        NotifyMsg::consent,
    );
    let admitted = admit_delivery(a, &policy).expect("A is initially admitted");
    let (client, server) = CtlStream::pair().unwrap();
    let served = std::thread::spawn(move || serve(server));
    let client = Mutex::new(Some(client));
    let (opening, opened) = mpsc::sync_channel(1);
    let (released, release) = mpsc::sync_channel(1);
    let poster = freedesktop::Poster::start(
        move || {
            opening.send(()).unwrap();
            release
                .recv_timeout(HANG)
                .expect("the transport is released");
            Connection::handshake(
                client.lock().unwrap().take().expect("one connection"),
                aterm_dbus::conn::effective_uid(),
                None,
            )
        },
        |_| panic!("the private endpoint sends no click"),
    )
    .expect("the actual Poster starts");
    post_admitted(&poster, admitted, HANG);
    opened
        .recv_timeout(HANG)
        .expect("A reached transport setup");
    match change {
        Change::Unchanged => {}
        Change::Focus => {
            app.switch_tab_in(wid, 0);
            assert!(policy.suppress.lock().unwrap().contains(&0));
        }
        Change::DesktopOff => {
            app.config.desktop_alerts = Some(false);
            app.apply_desktop_alerts(false);
            assert!(!policy.own_alerts.load(std::sync::atomic::Ordering::Acquire));
            assert!(gate.has_denials(), "the FDA condition remains standing");
            assert!(delivers(
                &NotifyMsg::consent(notice.as_ref().unwrap()),
                &HashSet::new(),
                true,
            ));
        }
        Change::Serious => {
            app.apply_serious_mode(true);
            assert!(policy.silent.load(std::sync::atomic::Ordering::Acquire));
        }
    }
    // This real program event remains admissible after every change. It proves
    // the server's observation includes all earlier posts, without a short wait.
    let b = NotifyMsg::program(1, Some(BARRIER.into()), "independent B".into());
    post_admitted(
        &poster,
        admit_delivery(b, &policy).expect("B remains admitted"),
        HANG,
    );
    released.send(()).unwrap();
    let seen = served.join().expect("the typed private endpoint finished");
    drop(poster);
    assert_eq!(seen.last().unwrap().body[3].as_str(), Some(BARRIER));
    assert_eq!(
        seen.iter()
            .filter(|message| message.body[3].as_str() == Some(BARRIER))
            .count(),
        1,
    );
    assert_eq!(
        quiet(seen.last().unwrap()),
        matches!(change, Change::Serious),
        "B's fresh admission reads the changed setting"
    );
    seen
}

#[test]
fn delayed_unix_delivery_obeys_the_current_focus_alert_and_sound_policy() {
    let unchanged = queued_case(Change::Unchanged);
    assert_eq!(unchanged.len(), 2, "unchanged A and B both cross the wire");
    assert!(!quiet(&unchanged[0]));
    let mut stale = Vec::new();
    for change in [Change::Focus, Change::DesktopOff, Change::Serious] {
        let seen = queued_case(change);
        let valid = match change {
            Change::Focus | Change::DesktopOff => seen.len() == 1,
            Change::Serious => seen.len() == 2 && quiet(&seen[0]),
            Change::Unchanged => unreachable!(),
        };
        if !valid {
            stale.push((change, seen.len(), quiet(&seen[0])));
        }
    }
    assert!(
        stale.is_empty(),
        "stale delivery policy crossed the wire: {stale:?}"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A round trip through a real bus: a private `dbus-daemon` on a scratch
//! socket, two [`Connection`]s, a call from one to the other and back, and a
//! signal one emits that the other's `AddMatch` routes to it.
//!
//! It runs only where `dbus-daemon` is on PATH (Linux desktops; `brew install
//! dbus` on macOS) and otherwise skips, saying so. The daemon is this test's
//! own, never the session's bus.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use aterm_dbus::{Connection, Kind, Message, Value, address};

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

fn dbus_daemon() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("dbus-daemon"))
        .find(|p| p.is_file())
}

fn config(socket: &Path) -> String {
    format!(
        "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
         \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
         <busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
         <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
         <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>\n",
        socket.display()
    )
}

#[test]
fn a_call_and_a_signal_cross_a_private_dbus_daemon() {
    let Some(daemon) = dbus_daemon() else {
        eprintln!(
            "skip: no dbus-daemon on PATH, so no live bus round trip here; \
             install dbus (`brew install dbus`, or a Linux desktop's dbus-daemon) to run it"
        );
        return;
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let socket = scratch.path().join("bus");
    let conf = scratch.path().join("bus.conf");
    std::fs::write(&conf, config(&socket)).unwrap();
    let mut daemon = Daemon(
        aterm_uds::spawnfd::command(&daemon)
            .arg(format!("--config-file={}", conf.display()))
            .arg("--nofork")
            .arg("--print-address")
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(daemon.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addresses = address::parse(line.trim()).unwrap();

    {
        let mut caller = Connection::open(&addresses).unwrap();
        let mut callee = Connection::open(&addresses).unwrap();
        assert_ne!(caller.unique_name(), callee.unique_name());
        assert_eq!(caller.server_guid(), callee.server_guid());
        let callee_name = callee.unique_name().to_owned();

        caller
            .add_match(
                "type='signal',interface='org.aterm.Test',member='Ping'",
                HANG,
            )
            .unwrap();
        let served = thread::spawn(move || {
            loop {
                let msg = callee.recv(Some(HANG)).unwrap().expect("the call");
                if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Echo") {
                    callee.reply(&msg, msg.body.clone()).unwrap();
                    callee
                        .emit(
                            "/org/aterm/test",
                            "org.aterm.Test",
                            "Ping",
                            vec![Value::U32(7)],
                        )
                        .unwrap();
                    return;
                }
            }
        });
        let echo = Message::method_call(
            Some(&callee_name),
            "/org/aterm/test",
            Some("org.aterm.Test"),
            "Echo",
        )
        .unwrap()
        .with_body(vec![
            "hello".into(),
            Value::variant(Value::strings(["a", "b"])),
        ]);
        let reply = caller.call(echo, HANG).unwrap();
        assert_eq!(
            reply.body,
            vec![
                Value::from("hello"),
                Value::variant(Value::strings(["a", "b"]))
            ]
        );
        served.join().unwrap();
        let ping = loop {
            let msg = caller.recv(Some(HANG)).unwrap().expect("the signal");
            if msg.member.as_deref() == Some("Ping") {
                break msg;
            }
        };
        assert_eq!(
            (ping.sender.as_deref(), &ping.body[..]),
            (Some(callee_name.as_str()), &[Value::U32(7)][..])
        );
    }
}

/// The private daemon, killed and reaped however the test ends.
struct Daemon(aterm_uds::spawnfd::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
mod tests {
    use std::io::BufRead as _;

    use aterm_dbus::{Connection, Kind, Message, Value};

    use super::*;

    const HANG: Duration = Duration::from_secs(60);

    #[test]
    fn only_a_false_enable_animations_means_reduced() {
        assert!(animations_disabled(Some(&Value::Bool(false))));
        for not_reduced in [
            None,
            Some(Value::Bool(true)),
            Some(Value::from("false")),
            Some(Value::U32(0)),
        ] {
            assert!(
                !animations_disabled(not_reduced.as_ref()),
                "{not_reduced:?} must not read as reduced motion"
            );
        }
    }

    struct Daemon(aterm_uds::spawnfd::Child);

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus(scratch: &std::path::Path) -> (Daemon, Vec<aterm_dbus::address::Address>) {
        let Some(daemon) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("dbus-daemon"))
                .find(|p| p.is_file())
        }) else {
            aterm_spec::host::could_not_run(
                "no dbus-daemon on PATH, so no Settings portal round trip here; install the \
                 distribution's dbus (dbus-daemon) to run it",
            );
        };
        let socket = scratch.join("bus");
        let conf = scratch.join("bus.conf");
        let services = scratch.join("services");
        std::fs::create_dir_all(&services).unwrap();
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <servicedir>{}</servicedir>\
                 <policy context=\"default\"><allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>\n",
                socket.display(),
                services.display()
            ),
        )
        .unwrap();
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
        std::io::BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        (daemon, aterm_dbus::address::parse(line.trim()).unwrap())
    }

    /// How the stand-in portal answers a read.
    #[derive(Clone, Copy)]
    enum Portal {
        /// Version 2: `ReadOne`, the value in one variant.
        ReadOne(bool),
        /// Before version 2: `ReadOne` unknown, `Read`'s value in two variants.
        ReadOnly(bool),
        /// A portal with no such setting.
        NotFound,
    }

    fn portal(
        addresses: &[aterm_dbus::address::Address],
        how: Portal,
    ) -> std::thread::JoinHandle<Vec<String>> {
        let mut server = Connection::open(addresses).unwrap();
        let asked = Instant::now();
        loop {
            let owned = server
                .call(
                    Message::method_call(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        Some("org.freedesktop.DBus"),
                        "RequestName",
                    )
                    .unwrap()
                    .with_body(vec![Value::from(DESKTOP), Value::U32(4)]),
                    HANG,
                )
                .unwrap();
            if owned.body == vec![Value::U32(1)] {
                break;
            }
            assert!(
                owned.body == vec![Value::U32(3)] && asked.elapsed() < HANG,
                "the stand-in never owned the name: {:?}",
                owned.body
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::spawn(move || {
            let mut members = Vec::new();
            loop {
                let msg = server.recv(Some(HANG)).unwrap().expect("a Settings call");
                if msg.kind != Kind::MethodCall {
                    continue;
                }
                let member = msg.member.clone().unwrap_or_default();
                assert_eq!(msg.interface.as_deref(), Some(SETTINGS));
                assert_eq!(
                    msg.body,
                    vec![Value::from(GNOME_INTERFACE), Value::from(ENABLE_ANIMATIONS)]
                );
                members.push(member.clone());
                match (how, member.as_str()) {
                    (Portal::ReadOne(on), "ReadOne") => {
                        server
                            .reply(&msg, vec![Value::variant(Value::Bool(on))])
                            .unwrap();
                    }
                    (Portal::ReadOnly(_), "ReadOne") => {
                        server
                            .reply_error(&msg, UNKNOWN_METHOD, "No such method 'ReadOne'")
                            .unwrap();
                        continue;
                    }
                    (Portal::ReadOnly(on), "Read") => {
                        server
                            .reply(&msg, vec![Value::variant(Value::variant(Value::Bool(on)))])
                            .unwrap();
                    }
                    _ => {
                        server
                            .reply_error(
                                &msg,
                                "org.freedesktop.portal.Error.NotFound",
                                "Requested setting not found",
                            )
                            .unwrap();
                    }
                }
                return members;
            }
        })
    }

    #[test]
    fn the_portal_s_answer_is_the_motion_preference() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (_daemon, addresses) = private_bus(scratch.path());
        let reduced = |how: Portal| {
            let served = portal(&addresses, how);
            let answer = read_at(&addresses, GNOME_INTERFACE, ENABLE_ANIMATIONS, HANG);
            (animations_disabled(answer.as_ref()), served.join().unwrap())
        };
        assert_eq!(
            reduced(Portal::ReadOne(false)),
            (true, vec!["ReadOne".into()])
        );
        assert_eq!(
            reduced(Portal::ReadOne(true)),
            (false, vec!["ReadOne".into()])
        );
        assert_eq!(
            reduced(Portal::ReadOnly(false)),
            (true, vec!["ReadOne".into(), "Read".into()])
        );
        assert_eq!(
            reduced(Portal::ReadOnly(true)),
            (false, vec!["ReadOne".into(), "Read".into()])
        );
        assert_eq!(reduced(Portal::NotFound), (false, vec!["ReadOne".into()]));
        assert_eq!(
            read_at(&addresses, GNOME_INTERFACE, ENABLE_ANIMATIONS, HANG),
            None
        );
    }

    #[test]
    fn a_portal_that_is_not_running_is_never_started_by_the_read() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let started = scratch.path().join("started");
        let services = scratch.path().join("services");
        std::fs::create_dir_all(&services).unwrap();
        std::fs::write(
            services.join(format!("{DESKTOP}.service")),
            format!(
                "[D-BUS Service]\nName={DESKTOP}\nExec=/bin/sh -c 'echo > {}'\n",
                started.display()
            ),
        )
        .unwrap();
        let (_daemon, addresses) = private_bus(scratch.path());
        assert_eq!(
            read_at(&addresses, GNOME_INTERFACE, ENABLE_ANIMATIONS, HANG),
            None
        );
        assert!(
            !started.exists(),
            "the read started the portal's service on the bus"
        );
    }

    #[test]
    fn a_bus_that_never_answers_holds_the_read_no_longer_than_its_bound() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("silent");
        let _silent = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let addresses =
            aterm_dbus::address::parse(&format!("unix:path={}", path.display())).unwrap();
        let asked = Instant::now();
        let answer = read_at(
            &addresses,
            GNOME_INTERFACE,
            ENABLE_ANIMATIONS,
            Duration::from_millis(100),
        );
        let waited = asked.elapsed();
        assert_eq!(answer, None);
        assert!(
            waited < aterm_dbus::conn::HANDSHAKE_TIMEOUT,
            "the read gave up after {waited:?}, the handshake default's, not the 100 ms it was \
             given"
        );
    }

    #[test]
    #[ignore = "needs a session bus with xdg-desktop-portal and dconf: the Linux lane runs it"]
    fn the_desktop_s_portal_follows_enable_animations() {
        let gsettings = |args: &[&str]| {
            let out = aterm_uds::spawnfd::command("gsettings")
                .args(args)
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .output()
                .expect("gsettings");
            assert!(out.status.success(), "gsettings {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let was = gsettings(&["get", GNOME_INTERFACE, ENABLE_ANIMATIONS]);
        for on in ["false", "true", "false"] {
            gsettings(&["set", GNOME_INTERFACE, ENABLE_ANIMATIONS, on]);
            let started = Instant::now();
            let answer = loop {
                let answer = read(GNOME_INTERFACE, ENABLE_ANIMATIONS, HANG);
                if answer == Some(Value::Bool(on == "true")) || started.elapsed() > HANG {
                    break answer;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            eprintln!("enable-animations {on}: the portal answered {answer:?}");
            assert_eq!(
                animations_disabled(answer.as_ref()),
                on == "false",
                "{answer:?}"
            );
        }
        let took: Vec<Duration> = (0..5)
            .map(|_| {
                let asked = Instant::now();
                let _ = read(GNOME_INTERFACE, ENABLE_ANIMATIONS, HANG);
                asked.elapsed()
            })
            .collect();
        eprintln!("warm Settings portal reads took {took:?}");
        gsettings(&["set", GNOME_INTERFACE, ENABLE_ANIMATIONS, &was]);
    }

    #[test]
    #[ignore = "needs a session bus with xdg-desktop-portal and dconf: the Linux lane runs it"]
    fn the_desktop_s_portal_answers_the_button_layout() {
        use crate::client_frame::{ButtonLayout, LayoutButton, Side};
        let gsettings = |args: &[&str]| {
            let out = aterm_uds::spawnfd::command("gsettings")
                .args(args)
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .output()
                .expect("gsettings");
            assert!(out.status.success(), "gsettings {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let was = gsettings(&["get", GNOME_WM_PREFERENCES, BUTTON_LAYOUT]);
        for (value, left, right) in [
            (
                "close,minimize:maximize",
                &[LayoutButton::Close, LayoutButton::Minimize][..],
                &[LayoutButton::Maximize][..],
            ),
            ("appmenu:close", &[][..], &[LayoutButton::Close][..]),
        ] {
            gsettings(&["set", GNOME_WM_PREFERENCES, BUTTON_LAYOUT, value]);
            let started = Instant::now();
            let answer = loop {
                let answer = read(GNOME_WM_PREFERENCES, BUTTON_LAYOUT, HANG);
                if answer == Some(Value::Str(value.into())) || started.elapsed() > HANG {
                    break answer;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let Some(Value::Str(answer)) = answer else {
                panic!("button-layout {value}: the portal answered {answer:?}");
            };
            let layout = ButtonLayout::parse(&answer).expect("a layout");
            assert_eq!(layout.on(Side::Left).collect::<Vec<_>>(), left, "{value}");
            assert_eq!(layout.on(Side::Right).collect::<Vec<_>>(), right, "{value}");
        }
        gsettings(&["set", GNOME_WM_PREFERENCES, BUTTON_LAYOUT, &was]);
    }

    #[test]
    #[ignore = "needs a session bus with xdg-desktop-portal and dconf: the Linux lane runs it"]
    fn the_desktop_s_portal_answers_the_color_scheme() {
        let gsettings = |args: &[&str]| {
            let out = aterm_uds::spawnfd::command("gsettings")
                .args(args)
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .output()
                .expect("gsettings");
            assert!(out.status.success(), "gsettings {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let was = gsettings(&["get", GNOME_INTERFACE, "color-scheme"]);
        for (scheme, value) in [("prefer-dark", 1), ("default", 0), ("prefer-dark", 1)] {
            gsettings(&["set", GNOME_INTERFACE, "color-scheme", scheme]);
            let started = Instant::now();
            let answer = loop {
                let answer = read("org.freedesktop.appearance", "color-scheme", HANG);
                if answer == Some(Value::U32(value)) || started.elapsed() > HANG {
                    break answer;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            assert_eq!(answer, Some(Value::U32(value)), "color-scheme {scheme}");
        }
        gsettings(&["set", GNOME_INTERFACE, "color-scheme", &was]);
    }
}

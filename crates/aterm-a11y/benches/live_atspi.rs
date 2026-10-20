// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit publisher acceptance against Linux at-spi2-core. This changes the
//! session's accessibility enablement temporarily and kills/rejoins its accessibility
//! bus; run in the Linux lane's scratch VM, never an owner's desktop session.
//! STOCK EXCEPTION: the scratch guest uses the pinned stock compiler in
//! `aterm-lab/src/linux_guest.rs`; the lane documents the unavailable Linux Trust build.
//! The host still invokes Trust tooling:
//! `targo --unverified lab linux exec -- cargo bench -p aterm-a11y --bench live_atspi --profile test`
//! A missing session bus/at-spi2-core is COULD NOT RUN, never a successful row.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
#[cfg_attr(
    target_os = "macos",
    expect(
        dead_code,
        reason = "this host can only report Linux acceptance unavailable"
    )
)]
mod support;

#[cfg(target_os = "linux")]
mod live {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};

    use aterm_a11y::atspi::{
        ActionHandler, ActivationHandler, DeactivationHandler, Publisher, SessionBuses,
    };
    use aterm_a11y::model::{
        Action, ActionRequest, Node, NodeId, Rect, Role, TextPosition, TextSelection, Tree, TreeId,
        TreeUpdate,
    };
    use aterm_dbus::{Connection, Kind, Message, Value, address};

    /// A wait for something that must happen: a hang detector, not a budget.
    const HANG: Duration = Duration::from_secs(60);

    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
    const REGISTRY: &str = "org.a11y.atspi.Registry";
    const ROOT: &str = "/org/a11y/atspi/accessible/root";

    #[derive(Debug, PartialEq)]
    enum Heard {
        Activated,
        Action(Action),
        Deactivated,
    }

    struct Window(Sender<Heard>);

    impl ActivationHandler for Window {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            let _ = self.0.send(Heard::Activated);
            None
        }
    }

    impl ActionHandler for Window {
        fn do_action(&mut self, request: ActionRequest) {
            let _ = self.0.send(Heard::Action(request.action));
        }
    }

    impl DeactivationHandler for Window {
        fn deactivate_accessibility(&mut self) {
            let _ = self.0.send(Heard::Deactivated);
        }
    }

    fn tree(second: &str) -> TreeUpdate {
        let (window, terminal, run_a, run_b, button) =
            (NodeId(0), NodeId(1), NodeId(10), NodeId(11), NodeId(2));
        let run = |text: &str| {
            let mut node = Node::new(Role::TextRun);
            node.set_value(text.to_owned());
            node.set_character_lengths(
                text.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>(),
            );
            node.set_word_starts(vec![0]);
            node
        };
        let mut root = Node::new(Role::Window);
        root.set_label("~");
        root.set_children(vec![terminal, button]);
        let mut term = Node::new(Role::Terminal);
        term.set_label("aterm terminal");
        term.set_read_only();
        term.set_children(vec![run_a, run_b]);
        term.set_bounds(Rect::new(12.0, 36.0, 732.0, 516.0));
        let caret = TextPosition {
            node: run_b,
            character_index: 0,
        };
        term.set_text_selection(TextSelection {
            anchor: caret,
            focus: caret,
        });
        let mut new_tab = Node::new(Role::Button);
        new_tab.set_label("New tab");
        new_tab.set_bounds(Rect::new(12.0, 4.0, 92.0, 28.0));
        new_tab.add_action(Action::Click);
        new_tab.add_action(Action::Focus);
        TreeUpdate {
            nodes: vec![
                (window, root),
                (terminal, term),
                (run_a, run("live$ echo hi\n")),
                (run_b, run(second)),
                (button, new_tab),
            ],
            tree: Some(Tree::new(window)),
            tree_id: TreeId::ROOT,
            focus: terminal,
        }
    }

    fn call(
        conn: &mut Connection,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> aterm_dbus::Result<Vec<Value>> {
        let msg = Message::method_call(Some(destination), path, Some(interface), member)?;
        conn.call(msg.with_body(body), HANG).map(|reply| reply.body)
    }

    fn property(conn: &mut Connection, bus: &str, path: &str, iface: &str, name: &str) -> Value {
        let body = call(
            conn,
            bus,
            path,
            PROPERTIES,
            "Get",
            vec![iface.into(), name.into()],
        )
        .unwrap_or_else(|e| panic!("Get {iface}.{name} at {bus} {path}: {e}"));
        body.first()
            .and_then(Value::as_variant)
            .cloned()
            .unwrap_or_else(|| panic!("Get {iface}.{name}: {body:?}"))
    }

    fn set_enabled(session: &mut Connection, on: bool) {
        call(
            session,
            "org.a11y.Bus",
            "/org/a11y/bus",
            PROPERTIES,
            "Set",
            vec![
                "org.a11y.Status".into(),
                "IsEnabled".into(),
                Value::variant(Value::Bool(on)),
            ],
        )
        .expect("org.a11y.Status.IsEnabled can be set");
    }

    /// `(so)` references.
    fn refs(body: &[Value]) -> Vec<(String, String)> {
        body.first()
            .and_then(Value::as_array)
            .map(|a| {
                a.items()
                    .iter()
                    .filter_map(Value::as_struct)
                    .filter_map(|f| {
                        Some((
                            f.first()?.as_str()?.to_owned(),
                            f.get(1)?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An assistive technology on the accessibility bus at-spi2-core names now.
    fn at(session: &mut Connection) -> Connection {
        let body = call(
            session,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
            "GetAddress",
            vec![],
        )
        .expect("org.a11y.Bus.GetAddress");
        let list = body
            .first()
            .and_then(Value::as_str)
            .expect("an address")
            .to_owned();
        let mut conn =
            Connection::open(&address::parse(&list).unwrap()).expect("the accessibility bus");
        conn.add_match("type='signal'", HANG).unwrap();
        conn
    }

    /// The application the registry lists under `name`: its bus name. The
    /// registry lists an application when it answers its `Embed`, which the
    /// publisher awaits before it asks a window for its tree.
    fn find(conn: &mut Connection, name: &str) -> String {
        let apps = refs(&call(conn, REGISTRY, ROOT, ACCESSIBLE, "GetChildren", vec![]).unwrap());
        apps.into_iter()
            .find_map(|(bus, path)| {
                let named = call(
                    conn,
                    &bus,
                    &path,
                    PROPERTIES,
                    "Get",
                    vec![ACCESSIBLE.into(), "Name".into()],
                )
                .ok()?;
                let named = named.first()?.as_variant()?.as_str()? == name;
                named.then_some(bus)
            })
            .unwrap_or_else(|| panic!("the registry does not list {name}"))
    }

    /// The next signal `member` the publisher at `bus` sends.
    fn signal(conn: &mut Connection, bus: &str, member: &str) -> Message {
        let deadline = Instant::now() + HANG;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no {member} from {bus}");
            if let Ok(Some(msg)) = conn.recv(Some(left))
                && msg.kind == Kind::Signal
                && msg.sender.as_deref() == Some(bus)
                && msg.member.as_deref() == Some(member)
            {
                return msg;
            }
        }
    }

    /// The accessibility bus's daemon: the `dbus-daemon` this user runs on
    /// at-spi2-core's `accessibility.conf`.
    fn accessibility_daemon() -> Option<u32> {
        let uid = aterm_dbus::conn::effective_uid();
        std::fs::read_dir("/proc")
            .ok()?
            .flatten()
            .find_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
                let cmdline = String::from_utf8_lossy(&cmdline);
                let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
                let owner = status
                    .lines()
                    .find_map(|l| l.strip_prefix("Uid:"))?
                    .split_whitespace()
                    .next()?
                    .parse::<u32>()
                    .ok()?;
                (owner == uid
                    && cmdline.contains("dbus-daemon")
                    && cmdline.contains("accessibility.conf"))
                .then_some(pid)
            })
    }

    fn heard(rx: &Receiver<Heard>) -> Heard {
        rx.recv_timeout(HANG).expect("the window heard nothing")
    }

    /// The publisher's log, on stderr.
    struct Stderr;

    impl aterm_log::Log for Stderr {
        fn enabled(&self, _: &aterm_log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &aterm_log::Record<'_>) {
            eprintln!("[{}] {}", record.level(), record.args());
        }

        fn flush(&self) {}
    }

    /// Waits until `org.a11y.Bus` leaves its owner `was`.
    fn launcher_gone(session: &mut Connection, was: &str) {
        let deadline = Instant::now() + HANG;
        while owner(session, "org.a11y.Bus") == was {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the launcher {was} outlived its bus");
            // Its exit is a NameOwnerChanged, which this connection's rule hears.
            let _ = session.recv(Some(left));
        }
    }

    /// The unique name that owns `name`, or `""`.
    fn owner(session: &mut Connection, name: &str) -> String {
        call(
            session,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            vec![name.into()],
        )
        .ok()
        .and_then(|b| b.first().and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
    }

    pub(super) fn run() -> Result<(), String> {
        let _ = aterm_log::set_logger(&Stderr);
        aterm_log::set_max_level(aterm_log::LevelFilter::Debug);
        let mut session =
            Connection::session().map_err(|why| format!("a session bus is required: {why}"))?;
        let was = call(
            &mut session,
            "org.a11y.Bus",
            "/org/a11y/bus",
            PROPERTIES,
            "Get",
            vec!["org.a11y.Status".into(), "IsEnabled".into()],
        )
        .map_err(|why| format!("at-spi2-core is required on the session bus: {why}"))?
        .first()
        .and_then(Value::as_variant)
        .and_then(Value::as_bool)
        .ok_or_else(|| "at-spi2-core returned no IsEnabled boolean".to_string())?;
        if !was {
            set_enabled(&mut session, true);
        }

        let name = format!("aterm-a11y-live-{}", std::process::id());
        let publisher = Publisher::start(SessionBuses, Some(name.clone()));
        let (tx, window_heard) = mpsc::channel();
        let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
        assert_eq!(heard(&window_heard), Heard::Activated);
        let mut reader = at(&mut session);
        adapter.update_window_focus_state(true);
        adapter.update_if_active(|| tree("hi\n"));

        // The registry lists the application; its window and terminal read.
        let app = find(&mut reader, &name);
        let windows =
            refs(&call(&mut reader, &app, ROOT, ACCESSIBLE, "GetChildren", vec![]).unwrap());
        let [(_, window)] = windows.as_slice() else {
            panic!("one window: {windows:?}")
        };
        assert_eq!(
            property(&mut reader, &app, window, ACCESSIBLE, "Name"),
            Value::from("~")
        );
        let first = call(
            &mut reader,
            &app,
            window,
            ACCESSIBLE,
            "GetChildAtIndex",
            vec![0.into()],
        )
        .unwrap();
        let terminal = first[0].as_struct().unwrap()[1]
            .as_str()
            .unwrap()
            .to_owned();
        let text = call(
            &mut reader,
            &app,
            &terminal,
            "org.a11y.atspi.Text",
            "GetText",
            vec![0.into(), (-1).into()],
        )
        .unwrap();
        assert_eq!(text, vec![Value::from("live$ echo hi\nhi\n")]);
        let attributes = call(&mut reader, &app, ROOT, ACCESSIBLE, "GetAttributes", vec![]);
        assert!(
            matches!(&attributes, Err(aterm_dbus::Error::Remote { name, message })
                if name == "org.freedesktop.DBus.Error.UnknownMethod"
                    && message == "Unknown method 'GetAttributes'"),
            "{attributes:?}"
        );

        // An event crosses the bus, and an action comes back.
        adapter.update_if_active(|| tree("hi there\n"));
        let changed = signal(&mut reader, &app, "TextChanged");
        assert_eq!(changed.path.as_ref().unwrap().as_str(), terminal);
        let grabbed = call(
            &mut reader,
            &app,
            &terminal,
            "org.a11y.atspi.Component",
            "GrabFocus",
            vec![],
        );
        assert_eq!(grabbed.unwrap(), vec![Value::Bool(true)]);
        assert_eq!(heard(&window_heard), Heard::Action(Action::Focus));

        // The accessibility bus dies. The window is told, and nothing panics.
        let launcher = owner(&mut session, "org.a11y.Bus");
        session
            .add_match(
                "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',\
                 arg0='org.a11y.Bus'",
                HANG,
            )
            .unwrap();
        let daemon = accessibility_daemon().expect("the accessibility bus's dbus-daemon");
        let killed = aterm_uds::spawnfd::command("kill")
            .arg("-KILL")
            .arg(daemon.to_string())
            .status()
            .unwrap();
        assert!(killed.success());
        drop(reader);
        assert_eq!(heard(&window_heard), Heard::Deactivated);

        // The launcher goes with its bus; the publisher, told it changed hands,
        // asks for it again, rejoins the new bus, and the window publishes there.
        launcher_gone(&mut session, &launcher);
        assert_eq!(heard(&window_heard), Heard::Activated);
        let mut reader = at(&mut session);
        adapter.update_if_active(|| tree("hi again\n"));
        let app = find(&mut reader, &name);
        let windows =
            refs(&call(&mut reader, &app, ROOT, ACCESSIBLE, "GetChildren", vec![]).unwrap());
        assert_eq!(windows.len(), 1, "{windows:?}");

        if !was {
            set_enabled(&mut session, false);
        }
        drop(adapter);
        drop(publisher);
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    support::run(
        "a11y-atspi",
        &[("read-action-event-bus-rejoin", ())],
        |_| {
            #[cfg(target_os = "linux")]
            match live::run() {
                Ok(()) => support::Outcome::Pass(
                    "registry/tree/text, event, action and killed-bus rejoin controls ran".into(),
                ),
                Err(why) => support::Outcome::CouldNotRun(why),
            }
            #[cfg(not(target_os = "linux"))]
            support::Outcome::CouldNotRun(
                "requires a Linux scratch session with at-spi2-core".into(),
            )
        },
    )
}

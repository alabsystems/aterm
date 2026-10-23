// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit publisher acceptance against Linux `at-spi2-core` in the scratch VM.
//! STOCK EXCEPTION: the scratch guest uses the pinned stock compiler in
//! `aterm-lab/src/linux_guest.rs`; the lane documents the unavailable Linux Trust build.

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
        ActionHandler, ActivationHandler, Buses, DeactivationHandler, Publisher, SessionBuses,
        WRITE,
    };
    use aterm_a11y::model::{
        Action, ActionRequest, Node, NodeId, Rect, Role, TextPosition, TextSelection, Tree, TreeId,
        TreeUpdate,
    };
    use aterm_dbus::{Connection, Kind, Message, Value, address};

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

    fn find(conn: &mut Connection, name: &str) -> String {
        listed(conn, name).unwrap_or_else(|e| panic!("{e}"))
    }

    fn listed(conn: &mut Connection, name: &str) -> Result<String, String> {
        let apps = call(conn, REGISTRY, ROOT, ACCESSIBLE, "GetChildren", vec![])
            .map_err(|e| format!("the registry's GetChildren: {e}"))?;
        let mut answered = Vec::new();
        for (bus, path) in refs(&apps) {
            let named = call(
                conn,
                &bus,
                &path,
                PROPERTIES,
                "Get",
                vec![ACCESSIBLE.into(), "Name".into()],
            );
            if let Ok(body) = &named
                && body
                    .first()
                    .and_then(Value::as_variant)
                    .and_then(Value::as_str)
                    == Some(name)
            {
                return Ok(bus);
            }
            answered.push(format!("{bus} {path}: {named:?}"));
        }
        Err(format!(
            "the registry does not list {name}; it lists {answered:?}"
        ))
    }

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

    fn signal_pid(signal: &str, pid: u32) {
        let sent = aterm_uds::spawnfd::command("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status()
            .unwrap();
        assert!(sent.success(), "kill -{signal} {pid}");
    }

    struct Frozen(u32);

    impl Frozen {
        fn now(pid: u32) -> Frozen {
            signal_pid("STOP", pid);
            Frozen(pid)
        }
    }

    impl Drop for Frozen {
        fn drop(&mut self) {
            let _ = aterm_uds::spawnfd::command("kill")
                .arg("-CONT")
                .arg(self.0.to_string())
                .status();
        }
    }

    struct Deadline(Option<Duration>);

    impl Buses for Deadline {
        fn session(&mut self) -> aterm_dbus::Result<Connection> {
            SessionBuses::default().session()
        }

        fn open(&mut self, address: &str) -> aterm_dbus::Result<Connection> {
            SessionBuses::default().open(address)
        }

        fn address_override(&self) -> Option<String> {
            SessionBuses::default().address_override()
        }

        fn write_timeout(&self) -> Option<Duration> {
            self.0
        }
    }

    struct Enabled {
        session: Connection,
        was: bool,
    }

    impl Enabled {
        fn on() -> Result<Enabled, String> {
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
            Ok(Enabled { session, was })
        }
    }

    impl Drop for Enabled {
        fn drop(&mut self) {
            if !self.was {
                let _ = call(
                    &mut self.session,
                    "org.a11y.Bus",
                    "/org/a11y/bus",
                    PROPERTIES,
                    "Set",
                    vec![
                        "org.a11y.Status".into(),
                        "IsEnabled".into(),
                        Value::variant(Value::Bool(false)),
                    ],
                );
            }
        }
    }

    fn terminal_text(session: &mut Connection, name: &str) -> Vec<Value> {
        read_text(session, name).unwrap_or_else(|e| panic!("{e}"))
    }

    fn read_text(session: &mut Connection, name: &str) -> Result<Vec<Value>, String> {
        let mut reader = at(session);
        let app = listed(&mut reader, name)?;
        let windows = refs(
            &call(&mut reader, &app, ROOT, ACCESSIBLE, "GetChildren", vec![])
                .map_err(|e| format!("{app}'s windows: {e}"))?,
        );
        let [(_, window)] = windows.as_slice() else {
            return Err(format!("one window: {windows:?}"));
        };
        let first = call(
            &mut reader,
            &app,
            window,
            ACCESSIBLE,
            "GetChildAtIndex",
            vec![0.into()],
        )
        .map_err(|e| format!("{app}'s terminal: {e}"))?;
        let terminal = first
            .first()
            .and_then(Value::as_struct)
            .and_then(|r| r.get(1))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("a terminal reference: {first:?}"))?
            .to_owned();
        call(
            &mut reader,
            &app,
            &terminal,
            "org.a11y.atspi.Text",
            "GetText",
            vec![0.into(), (-1).into()],
        )
        .map_err(|e| format!("{app}'s text: {e}"))
    }

    struct Thawed {
        /// The window heard it was let go within the row's `told_within`.
        told: bool,
        /// The terminal's text, as an AT read it once the daemon was thawed.
        text: Vec<Value>,
        /// What else the window heard after the thaw.
        rest: Vec<Heard>,
        /// How many changes the publisher was given with the daemon frozen.
        changes: usize,
    }

    fn frozen_under_changes(
        write_timeout: Option<Duration>,
        changes: Option<usize>,
        told_within: Duration,
    ) -> Result<Thawed, String> {
        let _ = aterm_log::set_logger(&Stderr);
        aterm_log::set_max_level(aterm_log::LevelFilter::Debug);
        let mut enabled = Enabled::on()?;
        let name = format!("aterm-a11y-frozen-{}", std::process::id());
        let publisher = Publisher::start(Deadline(write_timeout), Some(name.clone()));
        let (tx, window_heard) = mpsc::channel();
        let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
        assert_eq!(heard(&window_heard), Heard::Activated);
        adapter.update_window_focus_state(true);
        adapter.update_if_active(|| tree("hi\n"));
        assert_eq!(
            terminal_text(&mut enabled.session, &name),
            vec![Value::from("live$ echo hi\nhi\n")]
        );

        let daemon = accessibility_daemon().expect("the accessibility bus's dbus-daemon");
        let frozen = Frozen::now(daemon);
        let started = Instant::now();
        let mut told = false;
        let mut i = 0usize;
        while changes.is_none_or(|n| i < n) && started.elapsed() < HANG {
            adapter.update_if_active(|| tree(&format!("line {i}\n")));
            i += 1;
            if changes.is_none()
                && window_heard.recv_timeout(Duration::from_millis(1)) == Ok(Heard::Deactivated)
            {
                told = true;
                break;
            }
        }
        if !told {
            told = window_heard.recv_timeout(told_within) == Ok(Heard::Deactivated);
        }
        eprintln!(
            "{i} changes sent with the daemon frozen; the window {} told within {told_within:?}",
            if told { "was" } else { "was not" }
        );
        drop(frozen);
        if told {
            assert_eq!(heard(&window_heard), Heard::Activated);
        }
        adapter.update_if_active(|| tree("thawed\n"));
        let thawed = vec![Value::from("live$ echo hi\nthawed\n")];
        let started = Instant::now();
        let mut first = None;
        let text = loop {
            let text = read_text(&mut enabled.session, &name);
            if text.as_ref() == Ok(&thawed) || started.elapsed() > HANG {
                eprintln!(
                    "thawed, the text read {:?} after {:?}; the first read {:?}",
                    text,
                    started.elapsed(),
                    first.as_ref().unwrap_or(&text)
                );
                break text.unwrap_or_else(|e| panic!("thawed: {e}"));
            }
            first.get_or_insert(text);
            std::thread::sleep(Duration::from_millis(50));
        };
        let rest: Vec<Heard> = window_heard.try_iter().collect();
        drop(adapter);
        drop(publisher);
        Ok(Thawed {
            told,
            text,
            rest,
            changes: i,
        })
    }

    /// Leaves a frozen bus on its write deadline, tells the window, and rejoins when thawed.
    pub(super) fn frozen() -> Result<String, String> {
        let row = frozen_under_changes(Some(WRITE), None, HANG)?;
        assert!(
            row.told,
            "the window was never told its bus stopped reading"
        );
        assert_eq!(row.text, vec![Value::from("live$ echo hi\nthawed\n")]);
        Ok(format!(
            "left the frozen bus after {} changes on the {WRITE:?} write deadline, told the \
             window, and rejoined it thawed",
            row.changes
        ))
    }

    /// Negative control of [`frozen`]: writes with no deadline block until the daemon thaws.
    pub(super) fn frozen_control() -> Result<String, String> {
        let row = frozen_under_changes(None, Some(20_000), WRITE * 3)?;
        assert!(!row.told, "a publisher with no deadline let its window go");
        assert!(!row.rest.contains(&Heard::Deactivated), "{:?}", row.rest);
        assert_eq!(row.text, vec![Value::from("live$ echo hi\nthawed\n")]);
        Ok(format!(
            "with no write deadline, {} changes and {:?} with the bus frozen, the window was \
             never let go",
            row.changes,
            WRITE * 3
        ))
    }

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

    fn launcher_gone(session: &mut Connection, was: &str) {
        let deadline = Instant::now() + HANG;
        while owner(session, "org.a11y.Bus") == was {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the launcher {was} outlived its bus");
            let _ = session.recv(Some(left));
        }
    }

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
        let publisher = Publisher::start(SessionBuses::default(), Some(name.clone()));
        let (tx, window_heard) = mpsc::channel();
        let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
        assert_eq!(heard(&window_heard), Heard::Activated);
        let mut reader = at(&mut session);
        adapter.update_window_focus_state(true);
        adapter.update_if_active(|| tree("hi\n"));

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

/// The rows, run in this order, one at a time on the session's one
/// accessibility bus: one kills its daemon, the others freeze it.
#[derive(Clone, Copy)]
enum Row {
    Rejoin,
    Frozen,
    FrozenControl,
}

fn main() -> std::process::ExitCode {
    support::run(
        "a11y-atspi",
        &[
            ("read-action-event-bus-rejoin", Row::Rejoin),
            ("frozen-bus-write-deadline", Row::Frozen),
            ("frozen-bus-no-deadline-control", Row::FrozenControl),
        ],
        |row| {
            #[cfg(target_os = "linux")]
            let ran = match row {
                Row::Rejoin => live::run().map(|()| {
                    "registry/tree/text, event, action and killed-bus rejoin controls ran".into()
                }),
                Row::Frozen => live::frozen(),
                Row::FrozenControl => live::frozen_control(),
            };
            #[cfg(target_os = "linux")]
            match ran {
                Ok(detail) => support::Outcome::Pass(detail),
                Err(why) => support::Outcome::CouldNotRun(why),
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = row;
                support::Outcome::CouldNotRun(
                    "requires a Linux scratch session with at-spi2-core".into(),
                )
            }
        },
    )
}

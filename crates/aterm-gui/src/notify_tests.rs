// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    NotifyMsg, applescript_escape, balloon_clicked, balloon_text, delivers, fold_controls,
    fold_to_utf16, markup_escape, windows_info_flags,
};
use std::collections::HashSet;

#[test]
fn a_bundled_aterm_posts_only_as_itself() {
    use super::{Route, route};
    use crate::notice_center::{Center, status_answer};
    let answers = [
        Center::Granted,
        Center::Refused,
        Center::Unasked,
        Center::Absent,
        status_answer(1),
        status_answer(3),
        status_answer(-1),
    ];
    assert_eq!(route(Center::Granted, true), Route::Center);
    for refused in [Center::Refused, status_answer(1), status_answer(-1)] {
        assert_eq!(route(refused, true), Route::Withheld);
    }
    for unasked in [Center::Unasked, status_answer(3)] {
        assert_eq!(route(unasked, true), Route::Unasked);
    }
    assert_eq!(
        route(Center::Absent, true),
        Route::Centerless,
        "Absent while bundled"
    );
    for answer in answers {
        assert!(
            !matches!(route(answer, true), Route::Osascript),
            "a bundle reached a subprocess: {answer:?}"
        );
    }
    assert_eq!(
        route(Center::Absent, false),
        Route::Osascript,
        "a bare binary"
    );
}

/// What a route posts: the center's notice for a granted center and osascript's for a bare
/// binary, each only while the notice is still wanted, and on every other way nothing, so a
/// bundle never spawns osascript. The script says the body, then the title, each escaped.
#[cfg(target_os = "macos")]
#[test]
fn a_notice_posts_only_through_the_center_or_a_bare_binarys_osascript_while_wanted() {
    use super::{Post, Route, osascript_script, post_for};
    for way in [
        Route::Center,
        Route::Withheld,
        Route::Unasked,
        Route::Centerless,
        Route::Osascript,
    ] {
        let posts = match way {
            Route::Center => Some(Post::Center),
            Route::Osascript => Some(Post::Osascript),
            _ => None,
        };
        assert_eq!(post_for(way, || true), posts, "{way:?}");
        assert_eq!(post_for(way, || false), None, "no longer wanted: {way:?}");
    }
    assert_eq!(
        osascript_script("Title", "Body"),
        r#"display notification "Body" with title "Title""#
    );
    assert_eq!(
        osascript_script("a\"b", "c\\"),
        r#"display notification "c\\" with title "a\"b""#
    );
}

/// An ask hands the App one [`crate::Wake::NoticeReady`] once macOS answers and none before,
/// so the center's delegate is adopted when there is a center to adopt. The stand-in answers on
/// this thread because `WAKES` is thread-local: an answer from another thread would hide the
/// wake from `take_wakes`.
#[cfg(target_os = "macos")]
#[test]
fn asking_macos_wakes_the_app_once_it_answers() {
    use std::cell::RefCell;

    use super::{ask_macos, take_wakes};
    use crate::Wake;
    use crate::notice_center::{Center, NoticeCenter};

    type Then = Box<dyn FnOnce() + Send>;
    thread_local! {
        static ASKED: RefCell<Vec<Then>> = const { RefCell::new(Vec::new()) };
    }
    fn hold(then: Then) {
        ASKED.with(|asked| asked.borrow_mut().push(then));
    }
    let center = NoticeCenter::stand_in(|| Center::Unasked, hold);

    let _ = take_wakes();
    ask_macos(center);
    let mut asked = ASKED.with(RefCell::take);
    assert_eq!(asked.len(), 1, "one press is one ask");
    assert!(
        take_wakes().is_empty(),
        "macOS has not answered, so nothing wakes"
    );
    let answered = asked.pop().expect("the held ask");
    answered();
    assert!(
        matches!(take_wakes()[..], [Wake::NoticeReady]),
        "the answer wakes the App once"
    );
}

/// A notice on a center macOS has not answered is held and asks nothing, so no program's bytes
/// can raise the dialog. The granted center is the control: `post_of` reads the stand-in.
#[cfg(target_os = "macos")]
#[test]
fn a_notice_macos_has_not_answered_is_held_and_asks_nothing() {
    use std::cell::Cell;

    use super::{Post, post_of};
    use crate::notice_center::{Center, NoticeCenter};

    thread_local! {
        static ASKS: Cell<u32> = const { Cell::new(0) };
    }
    fn counted(_then: Box<dyn FnOnce() + Send>) {
        ASKS.with(|asks| asks.set(asks.get() + 1));
    }
    for bundled in [true, false] {
        for center in [
            NoticeCenter::stand_in(|| Center::Unasked, counted),
            NoticeCenter::stand_in(|| Center::Provisional, counted),
        ] {
            assert_eq!(post_of(center, bundled, || true), None, "held");
        }
        assert_eq!(
            post_of(
                NoticeCenter::stand_in(|| Center::Granted, counted),
                bundled,
                || true
            ),
            Some(Post::Center),
            "control: an answered center posts"
        );
    }
    assert_eq!(ASKS.with(Cell::get), 0, "no notice asked macOS");
}

/// A program's notice is wanted unless the person is looking at its own tab: focus
/// suppression is the one thing that drops it on the way to the desktop.
#[test]
fn a_program_notice_is_dropped_only_while_its_tab_is_looked_at() {
    let program = NotifyMsg::program(7, None, "build done".into());
    assert!(delivers(&program, &HashSet::new()), "nobody looking");
    assert!(
        delivers(&program, &HashSet::from([3])),
        "another tab looked at"
    );
    assert!(
        !delivers(&program, &HashSet::from([7])),
        "its own tab looked at: focus suppression holds"
    );
}

#[test]
fn serious_notifications_set_windows_no_sound_flag() {
    const NIIF_NOSOUND: u32 = 0x10;
    for own_icon in [false, true] {
        assert_eq!(windows_info_flags(false, own_icon) & NIIF_NOSOUND, 0);
        assert_eq!(
            windows_info_flags(true, own_icon) & NIIF_NOSOUND,
            NIIF_NOSOUND
        );
    }
}

#[test]
fn a_windows_notification_shows_aterms_icon() {
    const NIIF_ICON_MASK: u32 = 0xF;
    const NIIF_INFO: u32 = 0x1;
    const NIIF_USER: u32 = 0x4;
    const NIIF_LARGE_ICON: u32 = 0x20;
    let own = windows_info_flags(false, true);
    assert_eq!(own & NIIF_ICON_MASK, NIIF_USER);
    assert_eq!(own & NIIF_LARGE_ICON, NIIF_LARGE_ICON);
    let stock = windows_info_flags(false, false);
    assert_eq!(stock & NIIF_ICON_MASK, NIIF_INFO);
    assert_eq!(stock & NIIF_LARGE_ICON, 0);
}

#[test]
fn only_a_click_on_the_notification_raises_its_tab() {
    const WM_USER: isize = 0x0400;
    const WM_MOUSEMOVE: isize = 0x0200;
    const WM_LBUTTONUP: isize = 0x0202;
    assert!(balloon_clicked(WM_USER + 5));
    for other in [
        WM_USER + 2,
        WM_USER + 3,
        WM_USER + 4,
        WM_MOUSEMOVE,
        WM_LBUTTONUP,
    ] {
        assert!(!balloon_clicked(other), "{other:#x}");
    }
}

/// A notice with a title and no body shows its title as the balloon's text, under `aterm`:
/// a balloon with empty text never shows.
#[test]
fn a_title_only_notice_shows_its_title_as_the_balloons_text() {
    assert_eq!(balloon_text(Some("Build"), "done"), ("Build", "done"));
    assert_eq!(balloon_text(None, "done"), ("aterm", "done"));
    assert_eq!(
        balloon_text(Some("Build done"), ""),
        ("aterm", "Build done")
    );
    assert_eq!(balloon_text(None, ""), ("aterm", "aterm"));
}

#[test]
fn applescript_escape_neutralizes_quotes_backslashes_and_controls() {
    for (case, input, want) in [
        ("plain", "build finished", "build finished"),
        ("unicode", "café — 100% ✅", "café — 100% ✅"),
        ("quotes", r#"say "hi""#, r#"say \"hi\""#),
        ("a backslash", r"a\b", r"a\\b"),
        ("a backslash before a quote", "\\\"", "\\\\\\\""),
        ("a newline", "line1\nline2", "line1 line2"),
        ("a CRLF", "a\r\nb", "a  b"),
        ("a tab", "tab\there", "tab here"),
        ("a bell", "bell\x07x", "bell x"),
        ("an escape", "esc\x1b[0m", "esc [0m"),
        ("a NUL", "a\0b", "a b"),
        (
            "the break-out",
            r#"" & (do shell script "rm -rf ~") & ""#,
            r#"\" & (do shell script \"rm -rf ~\") & \""#,
        ),
    ] {
        assert_eq!(applescript_escape(input), want, "{case}");
    }
}

#[test]
fn fold_to_utf16_folds_truncates_per_char_and_terminates() {
    for (case, input, capacity, want) in [
        ("copies", "build done ✅", 16, "build done ✅"),
        ("folds controls", "a\r\nb\x07c", 16, "a  b c"),
        ("truncates, keeping the NUL", "abcdef", 4, "abc"),
        ("drops a pair that does not fit whole", "a🎉", 3, "a"),
        ("keeps a pair with room", "a🎉", 8, "a🎉"),
        ("only the NUL fits", "abc", 1, ""),
    ] {
        let mut buf = vec![0xFFFFu16; capacity];
        fold_to_utf16(input, &mut buf);
        let mut expected: Vec<u16> = want.encode_utf16().collect();
        expected.push(0);
        expected.resize(capacity, 0xFFFF);
        assert_eq!(buf, expected, "{case}");
    }
    fold_to_utf16("abc", &mut []);
}

#[test]
fn a_freedesktop_body_is_escaped_markup_and_folded_controls() {
    assert_eq!(markup_escape("build done ✅"), "build done ✅");
    assert_eq!(
        markup_escape("<a href=\"x\">click</a> & <b>"),
        "&lt;a href=\"x\"&gt;click&lt;/a&gt; &amp; &lt;b&gt;"
    );
    assert_eq!(
        markup_escape("&amp;"),
        "&amp;amp;",
        "an entity is escaped too"
    );
    assert_eq!(markup_escape("a\nb\r\tc\x1b[0m\x7f"), "a\nb  c [0m ");
    assert_eq!(fold_controls("t\x07i\ntle"), "t i\ntle");
}

#[cfg(target_os = "linux")]
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_os = "linux")]
const SERVER: &str = "org.freedesktop.Notifications";
#[cfg(target_os = "linux")]
const OBJECT: &str = "/org/freedesktop/Notifications";

#[cfg(target_os = "linux")]
struct PrivateBus {
    addresses: Vec<aterm_dbus::address::Address>,
    _daemon: Daemon,
    _scratch: aterm_tempfile::TempDir,
}

#[cfg(target_os = "linux")]
struct Daemon(aterm_uds::spawnfd::Child);

#[cfg(target_os = "linux")]
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
impl PrivateBus {
    fn start() -> Self {
        use std::io::BufRead as _;
        let Some(daemon) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("dbus-daemon"))
                .find(|p| p.is_file())
        }) else {
            aterm_spec::host::could_not_run(
                "no dbus-daemon on PATH, so no live notification round trip here; install \
                 the distribution's dbus (dbus-daemon) to run it",
            );
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let socket = scratch.path().join("bus");
        let conf = scratch.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>\n",
                socket.display()
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
        Self {
            addresses: aterm_dbus::address::parse(line.trim()).unwrap(),
            _daemon: daemon,
            _scratch: scratch,
        }
    }

    fn server(&self) -> aterm_dbus::Connection {
        use aterm_dbus::{Connection, Message, Value};
        let mut server = Connection::open_within(&self.addresses, HANG).unwrap();
        let owned = server
            .call(
                Message::method_call(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "RequestName",
                )
                .unwrap()
                .with_body(vec![Value::from(SERVER), Value::U32(0)]),
                HANG,
            )
            .unwrap();
        assert_eq!(
            owned.body,
            vec![Value::U32(1)],
            "the stand-in owns the name"
        );
        server
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_notification_is_a_notify_call_on_the_session_bus() {
    use aterm_dbus::{Connection, Kind, Message, Value};

    let private = PrivateBus::start();
    let mut server = private.server();
    let server_name = server.unique_name().to_owned();
    let served = std::thread::spawn(move || {
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
            if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                server.reply(&msg, vec![Value::U32(7)]).unwrap();
                seen.push(msg);
            }
        }
        seen
    });

    let mut bus = super::freedesktop::Bus { conn: None };
    let open = || Connection::open_within(&private.addresses, HANG);
    let loud = super::freedesktop::notify_call("build", "done <b>&</b>", false).unwrap();
    let posted = bus.notify(&loud, HANG, &open, &mut |_| {});
    assert_eq!(
        posted,
        Some(super::freedesktop::Posted {
            server: Some(server_name),
            id: 7
        }),
        "the server took the first one, and said where"
    );
    let quiet = super::freedesktop::notify_call("aterm", "serious", true).unwrap();
    assert!(
        bus.notify(&quiet, HANG, &open, &mut |_| {}).is_some(),
        "and the second, on the cached connection"
    );
    let seen = served.join().unwrap();

    let hints = |msg: &Message| {
        msg.body[6]
            .as_dict()
            .expect("a{sv} hints")
            .entries()
            .iter()
            .map(|(k, v)| (k.as_str().unwrap().to_owned(), v.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        seen[0].body[..6],
        [
            Value::from("aterm"),
            Value::U32(0),
            Value::from("aterm"),
            Value::from("build"),
            Value::from("done &lt;b&gt;&amp;&lt;/b&gt;"),
            Value::strings(["default", "Go to tab"]),
        ]
    );
    assert_eq!(seen[0].body[7], Value::I32(-1));
    assert_eq!(
        hints(&seen[0]),
        vec![
            (
                "desktop-entry".to_owned(),
                Value::variant(Value::from("aterm"))
            ),
            ("urgency".to_owned(), Value::variant(Value::Byte(1))),
        ]
    );
    assert_eq!(
        hints(&seen[1]),
        vec![
            (
                "desktop-entry".to_owned(),
                Value::variant(Value::from("aterm"))
            ),
            ("urgency".to_owned(), Value::variant(Value::Byte(1))),
            (
                "suppress-sound".to_owned(),
                Value::variant(Value::Bool(true))
            ),
        ],
        "serious mode is suppress-sound at normal urgency, never urgency low"
    );
    assert_eq!(seen[1].body[4], Value::from("serious"));
    assert_eq!(
        seen[1].body[5],
        Value::strings(["default", "Go to tab"]),
        "every notice is a tab's, and offers the default action"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_click_is_the_default_action_on_a_notice_aterm_posted() {
    use super::freedesktop::{Clicks, Observation, POSTED_CAP, Posted};
    use aterm_dbus::{Message, Value};

    const TOKEN: &str = "gnome-shell/aterm/4242-1-host_TIME5678";
    let server = Some(":1.7".to_owned());
    let sent = |sender: &Option<String>, mut msg: Message, id: u32, arg: Value| {
        msg.sender.clone_from(sender);
        msg.with_body(vec![Value::U32(id), arg])
    };
    let signal = |member: &str| Message::signal(OBJECT, SERVER, member).unwrap();
    let token = |id| sent(&server, signal("ActivationToken"), id, Value::from(TOKEN));
    let click = |id| sent(&server, signal("ActionInvoked"), id, Value::from("default"));
    let closed = |id| sent(&server, signal("NotificationClosed"), id, Value::U32(2));
    let at = |id| Posted {
        server: server.clone(),
        id,
    };

    let mut clicks = Clicks::default();
    clicks.posted(at(41), 3);
    clicks.posted(at(42), 5);
    assert_eq!(clicks.heard(&token(42)), None, "a token is no click");
    assert_eq!(
        clicks.heard(&click(42)),
        Some((5, Some(TOKEN.to_owned()))),
        "a click goes to its notice's session, with the token sent before it"
    );
    assert_eq!(
        clicks.heard(&click(42)),
        Some((5, None)),
        "the token went with that click"
    );
    assert_eq!(clicks.heard(&click(41)), Some((3, None)), "no token sent");

    let other_sender = Some(":1.9".to_owned());
    for (case, msg) in [
        ("another program's notification", click(99)),
        (
            "another sender",
            sent(
                &other_sender,
                signal("ActionInvoked"),
                42,
                Value::from("default"),
            ),
        ),
        (
            "an action aterm never offered",
            sent(&server, signal("ActionInvoked"), 42, Value::from("reply")),
        ),
        (
            "a method call",
            sent(
                &server,
                Message::method_call(Some(SERVER), OBJECT, Some(SERVER), "ActionInvoked").unwrap(),
                42,
                Value::from("default"),
            ),
        ),
        (
            "another interface",
            sent(
                &server,
                Message::signal(OBJECT, "org.example.Notifications", "ActionInvoked").unwrap(),
                42,
                Value::from("default"),
            ),
        ),
    ] {
        assert_eq!(clicks.heard(&msg), None, "{case}");
    }

    assert_eq!(clicks.heard(&closed(41)), None);
    assert_eq!(
        clicks.heard(&click(41)),
        None,
        "a closed notice is forgotten"
    );
    assert_eq!(
        clicks.heard(&click(42)),
        Some((5, None)),
        "control: the open one still clicks"
    );

    for id in 100..100 + u32::try_from(POSTED_CAP).unwrap() {
        clicks.posted(at(id), u64::from(id));
    }
    assert_eq!(
        clicks.heard(&click(42)),
        None,
        "the oldest of more than the bus thread remembers is forgotten"
    );
    assert_eq!(
        clicks.heard(&click(100)),
        Some((100, None)),
        "control: the newer ones still click"
    );

    let mut epochs = Clicks::default();
    epochs.posted(at(41), 3);
    assert_eq!(epochs.observe(Observation::Message(&token(41))), None);
    assert_eq!(epochs.observe(Observation::ConnectionReset), None);
    assert_eq!(epochs.observe(Observation::Message(&click(41))), None);
    epochs.posted(at(41), 5);
    assert_eq!(
        epochs.observe(Observation::Message(&click(41))),
        Some((5, None)),
        "the replacement's notice routes to its own session, with no old token"
    );
    assert_eq!(epochs.observe(Observation::Message(&token(41))), None);
    epochs.posted(at(41), 7);
    assert_eq!(
        epochs.observe(Observation::Message(&click(41))),
        Some((7, None)),
        "a repeated key replaces its target and token within an epoch too"
    );
    epochs.posted(at(42), 9);
    assert_eq!(
        epochs.observe(Observation::Message(&click(42))),
        Some((9, None)),
        "an unrelated open notice still routes"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_click_on_a_linux_notification_goes_to_the_tab_that_posted_it() {
    use aterm_dbus::{Connection, Kind, Message, Value};

    use crate::app_tabs::{take_raised_activations, take_raised_windows};

    const TOKEN: &str = "gnome-shell/aterm/4242-1-host_TIME5678";

    fn emit(server: &mut Connection, member: &str, id: u32, arg: Value) {
        server
            .emit(OBJECT, SERVER, member, vec![Value::U32(id), arg])
            .unwrap();
    }
    fn click(server: &mut Connection, (id, notify): &(u32, Message)) {
        let offers_default = notify.body[5].as_array().is_some_and(|actions| {
            actions
                .items()
                .iter()
                .any(|a| a.as_str() == Some("default"))
        });
        if offers_default {
            emit(server, "ActivationToken", *id, Value::from(TOKEN));
            emit(server, "ActionInvoked", *id, Value::from("default"));
        }
    }

    let private = PrivateBus::start();
    let mut server = private.server();
    let served = std::thread::spawn(move || {
        let mut notices = Vec::new();
        let mut next = 41;
        while notices.len() < 2 {
            let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
            if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                server.reply(&msg, vec![Value::U32(next)]).unwrap();
                notices.push((next, msg));
                next += 1;
            }
        }
        emit(&mut server, "ActionInvoked", 99, Value::from("default"));
        emit(
            &mut server,
            "NotificationClosed",
            notices[0].0,
            Value::U32(2),
        );
        click(&mut server, &notices[0]);
        click(&mut server, &notices[1]);
        (server, notices)
    });

    let wid = crate::WindowId(0);
    let mut app = crate::App::headless_for_test();
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let (tab_a, tab_b) = (0, 1);
    app.switch_tab_in(wid, 0);
    assert_eq!(
        app.windows[&wid].tabs.active, 0,
        "fixture: the owner is on tab A"
    );
    let _ = (take_raised_windows(), take_raised_activations());

    let (woke, wakes) = std::sync::mpsc::channel();
    let addresses = private.addresses.clone();
    let poster = super::freedesktop::Poster::start(
        move || Connection::open_within(&addresses, HANG),
        move |wake| {
            let _ = woke.send(wake);
        },
    )
    .expect("the bus thread starts");
    super::post(&poster, Some("tab A"), "build done", false, tab_a, HANG);
    super::post(&poster, Some("tab B"), "tests done", false, tab_b, HANG);

    let wake = wakes
        .recv_timeout(HANG)
        .expect("the click on tab B's notice reaches aterm");
    let crate::Wake::NoticeClicked {
        session,
        activation,
    } = wake
    else {
        panic!("not a click: {wake:?}");
    };
    assert_eq!(
        (session, activation.as_deref()),
        (tab_b, Some(TOKEN)),
        "tab B's notice, with the server's token: the other program's click and the closed \
         notice's went nowhere"
    );
    app.notice_clicked(session, activation);
    assert_eq!(app.windows[&wid].tabs.active, 1, "tab B is selected");
    assert_eq!(take_raised_windows(), vec![wid], "its window raised");
    assert_eq!(
        take_raised_activations(),
        vec![Some(TOKEN.to_owned())],
        "with the click's activation token"
    );

    let (_server, notices) = served.join().unwrap();
    for (_, notify) in &notices {
        assert_eq!(
            notify.body[5],
            Value::strings(["default", "Go to tab"]),
            "a notice about a session offers the default action"
        );
    }

    app.notice_clicked(999, None);
    assert_eq!(
        app.windows[&wid].tabs.active, 1,
        "a session gone moves no tab"
    );
    assert_eq!(
        take_raised_windows(),
        vec![wid],
        "and raises the window aterm was last used in"
    );
    drop(poster);
}

#[cfg(target_os = "linux")]
#[test]
fn a_slow_linux_server_shows_a_notice_once_and_keeps_its_clicks() {
    use aterm_dbus::{Connection, Kind, Value};

    let private = PrivateBus::start();
    let mut server = private.server();
    let served = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut held = Vec::new();
        let mut next = 41;
        loop {
            let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
            if msg.kind != Kind::MethodCall || msg.member.as_deref() != Some("Notify") {
                continue;
            }
            let summary = msg.body[3].as_str().unwrap().to_owned();
            held.push((next, msg));
            next += 1;
            let last = summary == "tab C";
            seen.push(summary);
            if seen.len() == 1 || last {
                for (id, notify) in held.drain(..) {
                    server.reply(&notify, vec![Value::U32(id)]).unwrap();
                }
            }
            if last {
                server
                    .emit(
                        OBJECT,
                        SERVER,
                        "ActionInvoked",
                        vec![Value::U32(41), Value::from("default")],
                    )
                    .unwrap();
                return (server, seen);
            }
        }
    });

    let (woke, wakes) = std::sync::mpsc::channel();
    let addresses = private.addresses.clone();
    let poster = super::freedesktop::Poster::start(
        move || Connection::open_within(&addresses, HANG),
        move |wake| {
            let _ = woke.send(wake);
        },
    )
    .expect("the bus thread starts");
    let (tab_a, tab_b, tab_c) = (3, 5, 7);
    for (tab, summary, bound) in [
        (tab_a, "tab A", HANG),
        (tab_b, "tab B", super::freedesktop::CALL_TIMEOUT),
        (tab_c, "tab C", HANG),
    ] {
        let call = super::freedesktop::notify_call(summary, "done", false).unwrap();
        poster.post(call, tab, bound);
    }

    let (_server, seen) = served.join().unwrap();
    assert_eq!(
        seen,
        ["tab A", "tab B", "tab C"],
        "the server saw each notice once: an answer past the deadline sent none again"
    );
    let wake = wakes
        .recv_timeout(HANG)
        .expect("the click on tab A's notice reaches aterm");
    let crate::Wake::NoticeClicked {
        session,
        activation,
    } = wake
    else {
        panic!("not a click: {wake:?}");
    };
    assert_eq!(
        (session, activation),
        (tab_a, None),
        "tab A's notice still goes to tab A: the connection and its clicks were kept"
    );
    drop(poster);
}

#[cfg(target_os = "linux")]
#[test]
fn a_notice_posted_at_once_gives_up_on_a_silent_bus_within_its_bound() {
    use aterm_dbus::{Kind, Value};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("silent");
    let _silent = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let silent = aterm_dbus::address::parse(&format!("unix:path={}", path.display())).unwrap();
    let call = super::freedesktop::notify_call("aterm", "cannot open", false).unwrap();
    let asked = std::time::Instant::now();
    let posted = super::freedesktop::notify_now(&call, &silent, super::freedesktop::CALL_TIMEOUT);
    let waited = asked.elapsed();
    assert_eq!(posted, None, "a bus that never answered took nothing");
    assert!(
        waited < aterm_dbus::conn::HANDSHAKE_TIMEOUT,
        "the notice gave up after {waited:?}, the handshake default's, not its own bound"
    );

    let private = PrivateBus::start();
    let mut server = private.server();
    let served = std::thread::spawn(move || {
        loop {
            let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
            if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                server.reply(&msg, vec![Value::U32(7)]).unwrap();
                return server;
            }
        }
    });
    let posted = super::freedesktop::notify_now(&call, &private.addresses, HANG);
    let _server = served.join().unwrap();
    assert_eq!(
        posted.map(|at| at.id),
        Some(7),
        "a bus that answers takes the notice"
    );
}

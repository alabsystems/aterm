// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_dbus::Type;

use super::*;

#[test]
fn roles_and_states_follow_at_spi_numbering() {
    assert_eq!(role_name(23), "frame");
    assert_eq!(role_name(60), "terminal");
    assert_eq!(role_name(75), "application");
    assert_eq!(role_name(129), "push_button_menu");
    assert_eq!(role_name(130), "role130");
    let words = Value::array(
        Type::U32,
        vec![Value::U32((1 << 12) | (1 << 25)), Value::U32(1 << 11)],
    )
    .expect("au");
    assert_eq!(state_names(&[words]), "focused,showing,read_only");
    assert_eq!(state_names(&[]), "-");
}

#[test]
fn values_render_on_one_line() {
    assert_eq!(quote("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u{1}\"");
    let r = Value::Struct(vec![":1.4".into(), Value::path("/o/a").expect("path")]);
    assert_eq!(render(&r), "ref(/o/a)");
    assert_eq!(render(&Value::variant(Value::I32(-3))), "-3");
    let ext = Value::Struct(vec![
        Value::I32(0),
        Value::I32(5),
        Value::I32(10),
        Value::I32(20),
    ]);
    assert_eq!(extents(&[ext]), "0,5,10,20");
    let attrs = Value::dict(
        Type::Str,
        Type::Str,
        vec![("z".into(), "1".into()), ("a".into(), "2".into())],
    )
    .expect("a{ss}");
    assert_eq!(attributes(&[attrs]), "\"a\":\"2\",\"z\":\"1\"");
    assert_eq!(
        field(Err(&"no such method".to_string())),
        "ERR(no_such_method)"
    );
}

#[test]
fn an_event_line_names_kind_path_and_body() {
    let mut msg = Message::signal(
        "/org/a11y/atspi/accessible/0/1",
        "org.a11y.atspi.Event.Object",
        "TextChanged",
    )
    .expect("signal");
    msg.body = vec![
        "insert".into(),
        Value::I32(76),
        Value::I32(17),
        Value::variant(" echo x".into()),
        Value::dict(Type::Str, Type::Variant, vec![]).expect("a{sv}"),
    ];
    assert_eq!(
        event_line(12, 77, &msg),
        "12 77 Object.TextChanged /org/a11y/atspi/accessible/0/1 detail=\"insert\" d1=76 d2=17 any=\" echo x\""
    );
}

#[test]
fn seat_steps_parse_and_refuse() {
    let w = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        parse_steps(&w(&["chord:ctrl+shift+c", "sleep:5"])).expect("parses"),
        [
            Step::Key {
                keysym: 0xffe3,
                pressed: true
            },
            Step::Key {
                keysym: 0xffe1,
                pressed: true
            },
            Step::Key {
                keysym: 0x63,
                pressed: true
            },
            Step::Key {
                keysym: 0x63,
                pressed: false
            },
            Step::Key {
                keysym: 0xffe1,
                pressed: false
            },
            Step::Key {
                keysym: 0xffe3,
                pressed: false
            },
            Step::Sleep(5),
        ]
    );
    assert_eq!(
        parse_steps(&w(&["tap:0x61", "button:middle"])).expect("parses")[2],
        Step::Button {
            code: 0x112,
            pressed: true
        }
    );
    assert_eq!(
        parse_steps(&w(&["touch:/tmp/a/ready", "wait:/tmp/a/go"])).expect("parses"),
        [
            Step::Touch("/tmp/a/ready".into()),
            Step::Wait("/tmp/a/go".into())
        ]
    );
    assert!(
        parse_steps(&w(&["wait:relative"])).is_err(),
        "a marker is an absolute path"
    );
    assert_eq!(parse_window("12582913"), Some(12_582_913));
    assert_eq!(parse_window("0xc00001"), Some(0x00c0_0001));
    assert_eq!(parse_window("window"), None);
    assert!(parse_steps(&w(&["tap:nosuchkey"])).is_err());
    assert!(parse_steps(&w(&["hold"])).is_err());
    assert!(parse_steps(&[]).is_err());
    assert_eq!(
        parse_steps(&w(&[
            "press:left",
            "move:40,-30",
            "move:@/tmp/a/aim",
            "release:left"
        ]))
        .expect("parses"),
        [
            Step::Button {
                code: 0x110,
                pressed: true
            },
            Step::Motion { dx: 40, dy: -30 },
            Step::MotionFrom("/tmp/a/aim".into()),
            Step::Button {
                code: 0x110,
                pressed: false
            },
        ]
    );
    assert_eq!(motion("12 -7\n"), Some((12, -7)), "the file's form");
    assert!(parse_steps(&w(&["move:@relative"])).is_err());
    assert!(parse_steps(&w(&["move:1"])).is_err());
    assert!(parse_steps(&w(&["move:1,2,3"])).is_err());
    assert!(parse_steps(&w(&["move:1,x"])).is_err());
    assert!(parse_steps(&w(&["move:200000,0"])).is_err());
    assert!(parse_steps(&w(&["press:nosuchbutton"])).is_err());
    assert_eq!(
        parse_steps(&w(&[
            "finger-down:640,400",
            "finger-move:@/tmp/a/to",
            "finger-up:",
            "tap:dead_acute",
            "chord:Super_L+Left"
        ]))
        .expect("parses")[..4],
        [
            Step::FingerDown(Point::At(640, 400)),
            Step::FingerMove(Point::From("/tmp/a/to".into())),
            Step::FingerUp,
            Step::Key {
                keysym: 0xfe51,
                pressed: true
            },
        ]
    );
    assert!(parse_steps(&w(&["finger-up:0"])).is_err());
    assert!(parse_steps(&w(&["finger-down:@relative"])).is_err());
    assert!(parse_steps(&w(&["finger-move:1"])).is_err());
    assert!(parse_steps(&w(&["sleep:+5"])).is_err());
    assert!(parse_steps(&w(&["tap:0x+41"])).is_err());
    assert_eq!(parse_window("+123"), None);
    assert_eq!(parse_window("0x+123"), None);
}

#[test]
fn which_steps_drive_the_pointer() {
    let w = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let drives = |v: &[&str]| {
        parse_steps(&w(v))
            .expect("parses")
            .iter()
            .any(Step::drives_pointer)
    };
    assert!(drives(&["touch:/tmp/a/ready", "move:640,400"]));
    assert!(drives(&["move:@/tmp/a/aim"]));
    assert!(drives(&["button:middle"]));
    assert!(drives(&["release:left"]));
    assert!(!drives(&[
        "chord:ctrl+shift+v",
        "sleep:5",
        "wait:/tmp/a/go"
    ]));
    assert!(PARK.drives_pointer());
    let touches = |v: &[&str]| {
        parse_steps(&w(v))
            .expect("parses")
            .iter()
            .any(Step::drives_touch)
    };
    assert!(touches(&["move:1,1", "finger-down:3,4"]));
    assert!(touches(&["finger-up:"]));
    assert!(!touches(&["move:1,1", "touch:/tmp/a/ready"]));
    assert!(!drives(&["finger-down:3,4"]));
}

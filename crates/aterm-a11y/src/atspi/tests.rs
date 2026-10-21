// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publisher against its two oracles that run on any unix:
//!
//! * zbus's bytes: the messages registry zbus 5.16.0 built for aterm-dbus's
//!   oracle (`crates/aterm-dbus/tests/recorded/zbus-5.16.0-messages.tsv`,
//!   recorded outside the workspace), which this module's events, error
//!   answers and `Embed` call must equal byte for byte;
//! * a whole session over scripted buses: a session bus and an
//!   accessibility bus played by the test over socket pairs, the test also
//!   playing the assistive technology. It walks the tree, drives the actions
//!   an AT can send, kills the accessibility bus mid-session (the publisher
//!   logs it, tells the window, and rejoins a second bus) and turns the
//!   status off.
//!
//! The Linux lane (`targo --unverified lab linux baseline`) holds the same
//! publisher to the recorded tree walks and event streams of a real aterm
//! under at-spi2-core.

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_dbus::{Connection, Kind, Message, Type, Value, message};
use aterm_uds::CtlStream;

use crate::model::{
    Action, ActionData, ActionRequest, Node, NodeId as LocalId, Rect, Role, TextPosition,
    TextSelection, Tree, TreeId, TreeUpdate,
};

use super::semantics::{
    ActionHandlerWrapper, Adapter as Semantics, AdapterCallback, AppContext, CacheNode, Event,
    Interface, InterfaceSet, NodeIdentity, ObjectEvent, PlatformNode, PlatformRoot, State,
    WindowBounds, next_adapter_id,
};
use super::serve::{self, Fault, Objects};
use super::wire::{self, ObjectId, ROOT_PATH};
use super::{
    ActionHandler, ActivationHandler, Buses, DeactivationHandler, Publisher, events, proxies,
};

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const AT: &str = ":1.9";
const ME: &str = ":1.42";

// --- the fixture ---------------------------------------------------------

const WINDOW: LocalId = LocalId(0);
const TERMINAL: LocalId = LocalId(1);
const RUN_A: LocalId = LocalId(10);
const RUN_B: LocalId = LocalId(11);
const BUTTON: LocalId = LocalId(2);
const LINK: LocalId = LocalId(3);
const SLIDER: LocalId = LocalId(4);

fn run(text: &str) -> Node {
    let mut node = Node::new(Role::TextRun);
    node.set_value(text.to_owned());
    node.set_character_lengths(text.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>());
    node.set_word_starts(vec![0]);
    node
}

/// A window like aterm's: a terminal document of two rows, a button, a link
/// and a slider. `second` is the terminal's second row.
fn fixture(second: &str) -> TreeUpdate {
    let mut window = Node::new(Role::Window);
    window.set_label("~");
    window.set_children(vec![TERMINAL, BUTTON, LINK, SLIDER]);
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_label("aterm terminal");
    terminal.set_read_only();
    terminal.set_children(vec![RUN_A, RUN_B]);
    terminal.set_bounds(Rect::new(12.0, 36.0, 732.0, 516.0));
    let caret = TextPosition {
        node: RUN_B,
        character_index: 0,
    };
    terminal.set_text_selection(TextSelection {
        anchor: caret,
        focus: caret,
    });
    let mut button = Node::new(Role::Button);
    button.set_label("New tab");
    button.set_bounds(Rect::new(12.0, 4.0, 92.0, 28.0));
    button.add_action(Action::Click);
    button.add_action(Action::Focus);
    let mut link = Node::new(Role::Link);
    link.set_label("docs");
    link.set_url("https://example.org/");
    link.add_action(Action::Click);
    let mut slider = Node::new(Role::Slider);
    slider.set_label("opacity");
    slider.set_numeric_value(3.0);
    slider.set_min_numeric_value(0.0);
    slider.set_max_numeric_value(10.0);
    slider.set_numeric_value_step(1.0);
    slider.add_action(Action::SetValue);
    TreeUpdate {
        nodes: vec![
            (WINDOW, window),
            (TERMINAL, terminal),
            (RUN_A, run("lane$ echo hi\n")),
            (RUN_B, run(second)),
            (BUTTON, button),
            (LINK, link),
            (SLIDER, slider),
        ],
        tree: Some(Tree::new(WINDOW)),
        tree_id: TreeId::ROOT,
        focus: TERMINAL,
    }
}

// --- the dispatcher alone -------------------------------------------------

/// The semantics layer's registrations, as the publisher thread gets them.
#[derive(Clone, Default)]
struct Recorder {
    registered: Arc<Mutex<Vec<(PlatformNode, InterfaceSet)>>>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl AdapterCallback for Recorder {
    fn register_interfaces(&self, adapter: &Semantics, id: NodeIdentity, interfaces: InterfaceSet) {
        self.registered
            .lock()
            .unwrap()
            .push((adapter.platform_node(id), interfaces));
    }

    fn unregister_interfaces(&self, _: &Semantics, _: NodeIdentity, _: InterfaceSet) {}

    fn emit_event(&self, _: &Semantics, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Clone, Default)]
struct Actions {
    requests: Arc<Mutex<Vec<ActionRequest>>>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        self.requests.lock().unwrap().push(request);
    }
}

struct Served {
    _semantics: Semantics,
    _app: Arc<super::semantics::SharedLock<AppContext>>,
    objects: Objects,
    actions: Actions,
    recorder: Recorder,
}

impl Served {
    fn new() -> Self {
        Self::over(fixture("hi\n"))
    }

    fn over(update: TreeUpdate) -> Self {
        let app = AppContext::new(Some("aterm-test".into()));
        let recorder = Recorder::default();
        let actions = Actions::default();
        let semantics = Semantics::with_wrapped_action_handler(
            next_adapter_id(),
            &app,
            recorder.clone(),
            update,
            true,
            WindowBounds::default(),
            Arc::new(ActionHandlerWrapper::new(actions.clone())),
        );
        let mut objects = Objects::new(ME, PlatformRoot::new(&app));
        objects.publish_root();
        for (node, interfaces) in recorder.registered.lock().unwrap().drain(..) {
            objects.register(node, interfaces);
        }
        Self {
            _semantics: semantics,
            _app: app,
            objects,
            actions,
            recorder,
        }
    }

    fn path(&self, local: LocalId) -> String {
        let registered = self.recorder.registered.lock().unwrap();
        assert!(registered.is_empty());
        // Node ids are the local id in the high word over tree index 0.
        let id = (u128::from(local.0) << 64).to_string();
        format!("/org/a11y/atspi/accessible/{}/{id}", self._semantics.id())
    }

    fn call(&self, path: &str, interface: &str, member: &str, body: Vec<Value>) -> serve::Served {
        let mut msg = Message::method_call(Some(ME), path, Some(interface), member)
            .unwrap()
            .with_body(body);
        msg.serial = 7;
        msg.sender = Some(AT.into());
        serve::serve(&self.objects, &msg)
    }

    fn ok(&self, path: &str, interface: &str, member: &str, body: Vec<Value>) -> Vec<Value> {
        let served = self.call(path, interface, member, body);
        assert!(served.signals.is_empty());
        served
            .answer
            .unwrap_or_else(|f| panic!("{interface}.{member} at {path}: {f:?}"))
    }

    fn fault(&self, path: &str, interface: &str, member: &str, body: Vec<Value>) -> Fault {
        self.call(path, interface, member, body)
            .answer
            .expect_err("an error answer")
    }

    fn get(&self, path: &str, interface: &str, name: &str) -> Value {
        let body = self.ok(
            path,
            serve::PROPERTIES,
            "Get",
            vec![interface.into(), name.into()],
        );
        match body.as_slice() {
            [Value::Variant(v)] => (**v).clone(),
            other => panic!("Get {interface}.{name}: {other:?}"),
        }
    }
}

fn signature(body: &[Value]) -> String {
    aterm_dbus::sig::signature_of(&body.iter().map(Value::type_of).collect::<Vec<_>>())
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.items()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_root_and_its_window_answer_a_walk() {
    let s = Served::new();
    let window = s.path(WINDOW);
    let children = s.ok(ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    assert_eq!(signature(&children), "a(so)");
    assert_eq!(
        children,
        vec![wire::refs(
            ME,
            [ObjectId::Node {
                adapter: s._semantics.id(),
                node: s._semantics.root_id(),
            }]
        )]
    );
    assert_eq!(
        s.get(ROOT_PATH, ACCESSIBLE, "Name"),
        Value::from("aterm-test")
    );
    assert_eq!(s.get(ROOT_PATH, ACCESSIBLE, "Parent"), wire::null_ref());
    assert_eq!(
        s.ok(ROOT_PATH, ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(75)]
    );
    assert_eq!(
        names(&s.ok(ROOT_PATH, ACCESSIBLE, "GetInterfaces", vec![])[0]),
        ["org.a11y.atspi.Accessible", "org.a11y.atspi.Application"]
    );
    // The root never had GetAttributes: the recorded walks read this error.
    assert_eq!(
        s.fault(ROOT_PATH, ACCESSIBLE, "GetAttributes", vec![]),
        Fault {
            name: "org.freedesktop.DBus.Error.UnknownMethod",
            text: "Unknown method 'GetAttributes'".into(),
        }
    );
    assert_eq!(
        s.get(ROOT_PATH, "org.a11y.atspi.Application", "AtspiVersion"),
        Value::from("2.1")
    );

    assert_eq!(s.get(&window, ACCESSIBLE, "Name"), Value::from("~"));
    assert_eq!(
        s.get(&window, ACCESSIBLE, "Parent"),
        wire::object_ref(ME, ObjectId::Root)
    );
    assert_eq!(
        s.ok(&window, ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(23)]
    );
    assert_eq!(s.get(&window, ACCESSIBLE, "ChildCount"), Value::I32(4));
    assert_eq!(
        names(&s.ok(&window, ACCESSIBLE, "GetInterfaces", vec![])[0]),
        ["org.a11y.atspi.Accessible", "org.a11y.atspi.Component"]
    );
    let attributes = s.ok(&window, ACCESSIBLE, "GetAttributes", vec![]);
    assert_eq!(signature(&attributes), "a{ss}");
    let all = s.ok(
        &window,
        serve::PROPERTIES,
        "GetAll",
        vec![ACCESSIBLE.into()],
    );
    let keys: Vec<_> = all[0]
        .as_dict()
        .unwrap()
        .entries()
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "Name",
            "Description",
            "Parent",
            "ChildCount",
            "Locale",
            "AccessibleId"
        ]
    );
    let cache = s.ok(wire::CACHE_PATH, "org.a11y.atspi.Cache", "GetItems", vec![]);
    assert_eq!(signature(&cache), "a((so)(so)(so)iiassusau)");
    assert_eq!(cache[0].as_array().unwrap().items().len(), 6);
}

#[test]
fn the_terminal_is_text() {
    let s = Served::new();
    let terminal = s.path(TERMINAL);
    let text = "org.a11y.atspi.Text";
    assert_eq!(
        names(&s.ok(&terminal, ACCESSIBLE, "GetInterfaces", vec![])[0]),
        [
            "org.a11y.atspi.Accessible",
            "org.a11y.atspi.Component",
            "org.a11y.atspi.Text"
        ]
    );
    assert_eq!(
        s.ok(&terminal, text, "GetText", vec![0.into(), (-1).into()]),
        vec![Value::from("lane$ echo hi\nhi\n")]
    );
    assert_eq!(s.get(&terminal, text, "CharacterCount"), Value::I32(17));
    assert_eq!(s.get(&terminal, text, "CaretOffset"), Value::I32(14));
    let line = s.ok(
        &terminal,
        text,
        "GetStringAtOffset",
        vec![0.into(), 3u32.into()],
    );
    assert_eq!(signature(&line), "sii");
    assert_eq!(line[0], Value::from("lane$ echo hi\n"));
    let extents = s.ok(
        &terminal,
        "org.a11y.atspi.Component",
        "GetExtents",
        vec![1u32.into()],
    );
    assert_eq!(
        extents,
        vec![wire::rect(super::semantics::Rect {
            x: 12,
            y: 36,
            width: 720,
            height: 480,
        })]
    );
    let character = s.ok(
        &terminal,
        text,
        "GetCharacterExtents",
        vec![0.into(), 1u32.into()],
    );
    assert_eq!(signature(&character), "iiii");
    assert_eq!(
        s.fault(&terminal, text, "GetText", vec![0.into()]).name,
        "org.freedesktop.DBus.Error.InvalidArgs"
    );
    let states = s.ok(&terminal, ACCESSIBLE, "GetState", vec![]);
    let focused = 1u32 << 12;
    assert!(matches!(
        states[0].as_array().unwrap().items()[0],
        Value::U32(low) if low & focused != 0
    ));
}

#[test]
fn a_link_is_a_hyperlink_and_a_slider_a_value() {
    let s = Served::new();
    let link = s.path(LINK);
    assert!(
        names(&s.ok(&link, ACCESSIBLE, "GetInterfaces", vec![])[0])
            .contains(&"org.a11y.atspi.Hyperlink".to_owned())
    );
    let hyperlink = "org.a11y.atspi.Hyperlink";
    assert_eq!(
        s.ok(&link, hyperlink, "GetURI", vec![0.into()]),
        vec![Value::from("https://example.org/")]
    );
    assert_eq!(s.get(&link, hyperlink, "NAnchors"), Value::I32(1));
    assert_eq!(
        s.ok(&link, hyperlink, "IsValid", vec![]),
        vec![Value::Bool(true)]
    );
    let slider = s.path(SLIDER);
    let value = "org.a11y.atspi.Value";
    assert_eq!(s.get(&slider, value, "CurrentValue"), Value::F64(3.0));
    assert_eq!(s.get(&slider, value, "MaximumValue"), Value::F64(10.0));
    // Set: the AT's SetValue, announced before the reply as zbus did.
    let set = s.call(
        &slider,
        serve::PROPERTIES,
        "Set",
        vec![
            value.into(),
            "CurrentValue".into(),
            Value::variant(Value::F64(7.0)),
        ],
    );
    assert_eq!(set.answer, Ok(vec![]));
    assert_eq!(set.signals.len(), 1);
    assert_eq!(set.signals[0].member.as_deref(), Some("PropertiesChanged"));
    let requested = s.actions.requests.lock().unwrap().pop().unwrap();
    assert_eq!(requested.action, Action::SetValue);
    assert_eq!(requested.data, Some(ActionData::NumericValue(7.0)));
    assert_eq!(
        s.fault(
            &slider,
            serve::PROPERTIES,
            "Set",
            vec![
                value.into(),
                "MaximumValue".into(),
                Value::variant(Value::F64(1.0))
            ],
        ),
        Fault {
            name: "org.freedesktop.DBus.Error.UnknownProperty",
            text: "Unknown property 'MaximumValue'".into(),
        }
    );
}

/// A load bar reads as a level bar holding its value, and a heatmap of
/// cores as a table: the stats bars' meters (`Role::Meter`) and core grid
/// (`Role::Grid`). Negative control: the slider beside them is still a
/// slider.
#[test]
fn a_meter_is_a_level_bar_and_a_grid_a_table() {
    const METER: LocalId = LocalId(30);
    const GRID: LocalId = LocalId(31);
    let mut update = fixture("hi\n");
    let mut meter = Node::new(Role::Meter);
    meter.set_label("CPU");
    meter.set_description("42 %, rising");
    meter.set_numeric_value(42.0);
    meter.set_min_numeric_value(0.0);
    meter.set_max_numeric_value(100.0);
    let mut grid = Node::new(Role::Grid);
    grid.set_label("Cores");
    let window = &mut update.nodes[0].1;
    window.set_children(vec![TERMINAL, BUTTON, LINK, SLIDER, METER, GRID]);
    update.nodes.push((METER, meter));
    update.nodes.push((GRID, grid));
    let s = Served::over(update);
    let meter = s.path(METER);
    assert_eq!(
        s.ok(&meter, ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(103)]
    );
    assert_eq!(
        s.get(&meter, ACCESSIBLE, "Description"),
        Value::from("42 %, rising")
    );
    let value = "org.a11y.atspi.Value";
    assert_eq!(s.get(&meter, value, "CurrentValue"), Value::F64(42.0));
    assert_eq!(s.get(&meter, value, "MaximumValue"), Value::F64(100.0));
    assert_eq!(
        s.ok(&s.path(GRID), ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(55)]
    );
    assert_eq!(
        s.ok(&s.path(SLIDER), ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(51)]
    );
}

/// A window holding a button and, after it, a list box of two options: ON
/// selected, OFF not, over the button where the list would scroll it to,
/// marked off screen when `offscreen`.
fn list_fixture(offscreen: bool) -> TreeUpdate {
    const LIST: LocalId = LocalId(20);
    let mut window = Node::new(Role::Window);
    window.set_label("Settings");
    window.set_children(vec![BUTTON, LIST]);
    window.set_bounds(Rect::new(0.0, 0.0, 200.0, 100.0));
    let mut button = Node::new(Role::Button);
    button.set_label("Import");
    button.set_bounds(Rect::new(0.0, 40.0, 100.0, 60.0));
    button.add_action(Action::Click);
    let mut list = Node::new(Role::ListBox);
    list.set_label("Themes");
    list.set_bounds(Rect::new(0.0, 0.0, 100.0, 20.0));
    list.set_children(vec![LINK, SLIDER]);
    let option = |label: &str, selected: bool, y: f64| {
        let mut option = Node::new(Role::ListBoxOption);
        option.set_label(label);
        option.set_selected(selected);
        option.set_bounds(Rect::new(0.0, y, 100.0, y + 20.0));
        option.add_action(Action::Click);
        option
    };
    let mut off = option("OFF", false, 40.0);
    if offscreen {
        off.set_offscreen();
    }
    TreeUpdate {
        nodes: vec![
            (WINDOW, window),
            (BUTTON, button),
            (LIST, list),
            (LINK, option("ON", true, 0.0)),
            (SLIDER, off),
        ],
        tree: Some(Tree::new(WINDOW)),
        tree_id: TreeId::ROOT,
        focus: WINDOW,
    }
}

/// An option its list holds off screen is in the tree, visible and
/// selectable but not showing, and owns no point: the button drawn where
/// its bounds lie answers there. The list's Selection counts its options:
/// child 0 is selected, child 1 is not and can be. Negative control: the
/// same option on screen answers its point and shows.
#[test]
fn an_offscreen_option_is_listed_not_showing_and_owns_no_point() {
    const LIST: LocalId = LocalId(20);
    let bit = |states: &[Value], state: State| {
        let bit = state as u32;
        let word = states[0].as_array().unwrap().items()[usize::from(bit >= 32)].clone();
        matches!(word, Value::U32(w) if w & (1 << (bit % 32)) != 0)
    };
    let at = |s: &Served| {
        let hit = s.ok(
            &s.path(WINDOW),
            "org.a11y.atspi.Component",
            "GetAccessibleAtPoint",
            vec![50.into(), 50.into(), 1u32.into()],
        );
        hit[0].as_struct().unwrap()[1].as_str().unwrap().to_owned()
    };
    for offscreen in [true, false] {
        let s = Served::over(list_fixture(offscreen));
        let off = s.ok(&s.path(SLIDER), ACCESSIBLE, "GetState", vec![]);
        assert!(bit(&off, State::Visible) && bit(&off, State::Selectable));
        assert_eq!(bit(&off, State::Showing), !offscreen, "{offscreen}");
        let on = s.ok(&s.path(LINK), ACCESSIBLE, "GetState", vec![]);
        assert!(bit(&on, State::Showing) && bit(&on, State::Selected));
        let expected = if offscreen { BUTTON } else { SLIDER };
        assert_eq!(at(&s), s.path(expected), "{offscreen}");
        let selection = "org.a11y.atspi.Selection";
        let list = s.path(LIST);
        assert_eq!(
            s.ok(&list, selection, "IsChildSelected", vec![0.into()]),
            vec![Value::Bool(true)]
        );
        assert_eq!(
            s.ok(&list, selection, "IsChildSelected", vec![1.into()]),
            vec![Value::Bool(false)]
        );
        assert_eq!(
            s.ok(&list, selection, "SelectChild", vec![1.into()]),
            vec![Value::Bool(true)]
        );
    }
}

#[test]
fn unknown_objects_interfaces_and_members_answer_as_zbus_did() {
    let s = Served::new();
    let gone = "/org/a11y/atspi/accessible/9/9";
    assert_eq!(
        s.fault(gone, ACCESSIBLE, "GetRole", vec![]),
        Fault {
            name: "org.freedesktop.DBus.Error.UnknownObject",
            text: format!("Unknown object '{gone}'"),
        }
    );
    let button = s.path(BUTTON);
    assert_eq!(
        s.fault(
            &button,
            "org.a11y.atspi.Text",
            "GetText",
            vec![0.into(), 1.into()]
        ),
        Fault {
            name: "org.freedesktop.DBus.Error.UnknownInterface",
            text: "Unknown interface 'org.a11y.atspi.Text'".into(),
        }
    );
    // An ancestor of a served path exists, with only the standard interfaces.
    assert_eq!(
        s.fault("/org/a11y/atspi", ACCESSIBLE, "GetRole", vec![])
            .name,
        "org.freedesktop.DBus.Error.UnknownInterface"
    );
    assert_eq!(
        s.ok(gone, "org.freedesktop.DBus.Peer", "Ping", vec![]),
        Vec::<Value>::new()
    );
}

#[test]
fn a_closed_window_s_paths_are_forgotten() {
    let mut s = Served::new();
    let window = s.path(WINDOW);
    s.objects.forget_adapter(s._semantics.id());
    assert_eq!(
        s.fault(&window, ACCESSIBLE, "GetRole", vec![]),
        Fault {
            name: "org.freedesktop.DBus.Error.UnknownObject",
            text: format!("Unknown object '{window}'"),
        }
    );
    // The application root stays.
    assert_eq!(
        s.ok(ROOT_PATH, ACCESSIBLE, "GetRole", vec![]),
        vec![Value::U32(75)]
    );
}

#[test]
fn the_actions_an_at_can_send_reach_the_window() {
    let scoped = |generation| {
        let mut update = fixture("hi\n");
        for (id, node) in &mut update.nodes {
            if *id == BUTTON || *id == TERMINAL {
                node.set_action_context(crate::model::ActionContext::new(41, generation));
            }
        }
        update
    };
    let mut s = Served::over(scoped(7));
    let (button, terminal) = (s.path(BUTTON), s.path(TERMINAL));
    let action = "org.a11y.atspi.Action";
    assert_eq!(s.get(&button, action, "NActions"), Value::I32(1));
    assert_eq!(
        s.ok(&button, action, "GetName", vec![0.into()]),
        vec![Value::from("click")]
    );
    assert_eq!(
        signature(&s.ok(&button, action, "GetActions", vec![])),
        "a(sss)"
    );
    assert_eq!(
        s.ok(&button, action, "DoAction", vec![0.into()]),
        vec![Value::Bool(true)]
    );
    assert_eq!(
        s.ok(&button, "org.a11y.atspi.Component", "GrabFocus", vec![]),
        vec![Value::Bool(true)]
    );
    assert_eq!(
        s.ok(
            &terminal,
            "org.a11y.atspi.Text",
            "SetCaretOffset",
            vec![2.into()]
        ),
        vec![Value::Bool(true)]
    );
    let heard: Vec<Action> = s
        .actions
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.action)
        .collect();
    assert_eq!(
        heard,
        [Action::Click, Action::Focus, Action::SetTextSelection]
    );
    // Routine content updates retain the exported object and its held handle.
    let held = s._semantics.platform_node(node(BUTTON.0));
    let mut unchanged_owner = scoped(7);
    unchanged_owner
        .nodes
        .iter_mut()
        .find(|(id, _)| *id == BUTTON)
        .unwrap()
        .1
        .set_label("Changed label");
    s._semantics.update(unchanged_owner);
    assert_eq!(held.name().unwrap(), "Changed label");
    assert_eq!(
        s.ok(&button, action, "DoAction", vec![0.into()]),
        vec![Value::Bool(true)]
    );
    assert!(
        s.recorder
            .registered
            .lock()
            .unwrap()
            .drain(..)
            .all(|(_, interfaces)| interfaces.bits() == 0)
    );

    // A replaced owner retires both a retained object path and a held node.
    // Old queued requests retain gen7; an old path cannot acquire gen8.
    s.recorder.events.lock().unwrap().clear();
    s._semantics.update(scoped(8));
    for (node, interfaces) in s.recorder.registered.lock().unwrap().drain(..) {
        s.objects.register(node, interfaces);
    }
    assert_eq!(
        s.fault(&button, action, "DoAction", vec![0.into()]).name,
        "org.freedesktop.DBus.Error.UnknownObject"
    );
    assert!(held.do_action(0).is_err());
    let children = s.ok(&s.path(WINDOW), ACCESSIBLE, "GetChildren", vec![]);
    let current = children[0].as_array().unwrap().items()[1]
        .as_struct()
        .unwrap()[1]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(current, button);
    let child_events: Vec<_> = s
        .recorder
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            Event::Object {
                target: super::semantics::NodeIdOrRoot::Node(parent),
                event,
            } if *parent == node(WINDOW.0) => match event {
                ObjectEvent::ChildRemoved(child) if child.id.0 == BUTTON => Some((
                    false,
                    ObjectId::Node {
                        adapter: s._semantics.id(),
                        node: *child,
                    }
                    .path(),
                )),
                ObjectEvent::ChildAdded(1, child) if child.id.0 == BUTTON => Some((
                    true,
                    ObjectId::Node {
                        adapter: s._semantics.id(),
                        node: *child,
                    }
                    .path(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        child_events,
        [(false, button.clone()), (true, current.clone())]
    );
    assert_eq!(
        s.ok(&current, action, "DoAction", vec![0.into()]),
        vec![Value::Bool(true)]
    );
    {
        let queued = s.actions.requests.lock().unwrap();
        assert_eq!(
            queued.len(),
            5,
            "stale path and held object must emit no action"
        );
        assert!(
            queued[..4]
                .iter()
                .all(|request| request.context == Some(crate::model::ActionContext::new(41, 7)))
        );
        assert_eq!(
            queued.last().unwrap().context,
            Some(crate::model::ActionContext::new(41, 8))
        );
    }

    // Removal/reinsertion also retires an object, even with unchanged owner context.
    let mut absent = scoped(8);
    absent.nodes.retain(|(id, _)| *id != BUTTON);
    absent
        .nodes
        .iter_mut()
        .find(|(id, _)| *id == WINDOW)
        .unwrap()
        .1
        .set_children(vec![TERMINAL, LINK, SLIDER]);
    s._semantics.update(absent);
    s._semantics.update(scoped(8));
    for (node, interfaces) in s.recorder.registered.lock().unwrap().drain(..) {
        s.objects.register(node, interfaces);
    }
    assert_eq!(
        s.fault(&current, action, "DoAction", vec![0.into()]).name,
        "org.freedesktop.DBus.Error.UnknownObject"
    );
    let children = s.ok(&s.path(WINDOW), ACCESSIBLE, "GetChildren", vec![]);
    let reinserted = children[0].as_array().unwrap().items()[1]
        .as_struct()
        .unwrap()[1]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(reinserted, current);
    assert_eq!(
        s.ok(&reinserted, action, "DoAction", vec![0.into()]),
        vec![Value::Bool(true)]
    );
    assert_eq!(s.actions.requests.lock().unwrap().len(), 6);
}

// --- zbus's bytes ---------------------------------------------------------

fn recorded(name: &str) -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-dbus/tests/recorded/zbus-5.16.0-messages.tsv"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let hex = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(&format!("{name}\t")))
        .unwrap_or_else(|| panic!("no recorded {name}"));
    let hex = hex.split('\t').next().unwrap();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// `msg` as the recorder built its twin: at `path`, with `serial`.
fn bytes(mut msg: Message, path: &str, serial: u32) -> Vec<u8> {
    msg.path = Some(aterm_dbus::ObjectPath::new(path).unwrap());
    msg.serial = serial;
    msg.encode().unwrap()
}

fn node(local: u64) -> NodeIdentity {
    NodeIdentity {
        id: crate::tree::NodeId(LocalId(local)),
        incarnation: 0,
    }
}

#[test]
fn events_are_zbus_s_bytes() {
    let state = events::object_event(
        ME,
        0,
        super::semantics::NodeIdOrRoot::Node(node(7)),
        ObjectEvent::StateChanged(State::Focused, true),
    );
    let path = "/org/a11y/atspi/accessible/7";
    assert_eq!(bytes(state, path, 5), recorded("state_changed"));
    let text = events::object_event(
        ME,
        0,
        super::semantics::NodeIdOrRoot::Node(node(7)),
        ObjectEvent::TextInserted {
            start_index: 0,
            length: 5,
            content: "hello".into(),
        },
    );
    assert_eq!(bytes(text, path, 8), recorded("text_changed"));
    // The negative control: one field off and the bytes differ.
    let off = events::object_event(
        ME,
        0,
        super::semantics::NodeIdOrRoot::Node(node(7)),
        ObjectEvent::StateChanged(State::Focused, false),
    );
    assert_ne!(bytes(off, path, 5), recorded("state_changed"));
}

#[test]
fn a_cache_addition_is_zbus_s_flattened_body() {
    // The recorder's terminal item: a Button-role (43) node with Accessible
    // and Text, two children, described "Terminal", Active | Horizontal.
    let item = CacheNode {
        adapter_id: 0,
        id: node(7),
        parent: super::semantics::NodeIdOrRoot::Root,
        index_in_parent: 0,
        child_count: 2,
        interfaces: InterfaceSet::new(Interface::Accessible | Interface::Text),
        name: String::new(),
        description: "Terminal".into(),
        role: super::semantics::Role::Button,
        states: State::Active | State::Horizontal,
    };
    let mut add = events::cache_add(ME, item);
    // The recorder named the node by a short path; the rest is the item's.
    add.body[0] = Value::Struct(vec![
        ME.into(),
        Value::path("/org/a11y/atspi/accessible/7").unwrap(),
    ]);
    assert_eq!(signature(&add.body), "(so)(so)(so)iiassusau");
    assert_eq!(
        bytes(add.clone(), wire::CACHE_PATH, 9),
        recorded("cache_add")
    );
    // The negative control: the item as the one struct argument the AT-SPI
    // specification names is not what zbus sent.
    let mut one = add.clone();
    one.body = vec![Value::Struct(add.body)];
    assert_ne!(bytes(one, wire::CACHE_PATH, 9), recorded("cache_add"));
}

#[test]
fn the_embed_call_and_an_unknown_object_are_zbus_s_bytes() {
    let mut embed = Message::method_call(
        Some("org.a11y.atspi.Registry"),
        ROOT_PATH,
        Some("org.a11y.atspi.Socket"),
        "Embed",
    )
    .unwrap()
    .with_body(vec![wire::object_ref(ME, ObjectId::Root)]);
    embed.serial = 6;
    assert_eq!(embed.encode().unwrap(), recorded("embed"));

    // The recorder's AT asked an application with no such node.
    let call = Message::decode(&recorded("get_role")).unwrap();
    let app = AppContext::new(None);
    let mut objects = Objects::new(ME, PlatformRoot::new(&app));
    objects.publish_root();
    let fault = serve::serve(&objects, &call).answer.unwrap_err();
    let mut error = Message::error(&call, fault.name, &fault.text).unwrap();
    error.serial = 11;
    assert_eq!(error.encode().unwrap(), recorded("get_role_error"));
}

// --- a whole session over scripted buses -----------------------------------

fn line(s: &mut CtlStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while !out.ends_with(b"\r\n") {
        s.read_exact(&mut b).unwrap();
        out.push(b[0]);
    }
    out
}

/// The bus's side of one connection, which the test plays: the bus itself
/// (`Hello`, `AddMatch`, at-spi2-core's and the registry's answers) and the
/// assistive technology calling the publisher.
struct Peer {
    stream: CtlStream,
    buf: Vec<u8>,
    serial: u32,
    held: VecDeque<Message>,
}

impl Peer {
    /// Runs the server half of the handshake on another thread, naming the
    /// client `unique`.
    fn accept(mut stream: CtlStream, unique: &'static str) -> std::thread::JoinHandle<Peer> {
        std::thread::spawn(move || {
            stream.set_read_timeout(Some(HANG)).unwrap();
            line(&mut stream);
            stream
                .write_all(b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f\r\n")
                .unwrap();
            assert_eq!(line(&mut stream), b"BEGIN\r\n");
            let mut peer = Peer {
                stream,
                buf: Vec::new(),
                serial: 0,
                held: VecDeque::new(),
            };
            let hello = peer.read();
            assert_eq!(hello.member.as_deref(), Some("Hello"));
            peer.answer(&hello, vec![unique.into()]);
            peer
        })
    }

    /// The next message the publisher wrote; `None` once it hung up.
    fn try_read(&mut self) -> Option<Message> {
        loop {
            if let Some(n) = message::frame_len(&self.buf).unwrap()
                && n <= self.buf.len()
            {
                let bytes: Vec<u8> = self.buf.drain(..n).collect();
                return Some(Message::decode(&bytes).unwrap());
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk).unwrap() {
                0 => return None,
                n => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn read(&mut self) -> Message {
        self.try_read().expect("the publisher hung up")
    }

    fn next(&mut self) -> Message {
        self.held.pop_front().unwrap_or_else(|| self.read())
    }

    fn write(&mut self, mut msg: Message) -> u32 {
        self.serial += 1;
        msg.serial = self.serial;
        self.stream.write_all(&msg.encode().unwrap()).unwrap();
        self.serial
    }

    fn answer(&mut self, call: &Message, body: Vec<Value>) {
        let mut reply = Message::method_return(call).with_body(body);
        reply.sender = Some("org.freedesktop.DBus".into());
        self.write(reply);
    }

    fn expect_call(&mut self, member: &str) -> Message {
        let msg = self.next();
        assert_eq!(msg.kind, Kind::MethodCall, "{msg:?}");
        assert_eq!(msg.member.as_deref(), Some(member), "{msg:?}");
        msg
    }

    /// The next signal named `member`, past any other.
    fn expect_signal(&mut self, member: &str) -> Message {
        loop {
            let msg = self.next();
            if msg.kind == Kind::Signal && msg.member.as_deref() == Some(member) {
                return msg;
            }
        }
    }

    /// The AT calls the publisher and waits for the answer; signals that
    /// arrive first are held for [`Peer::expect_signal`].
    fn ask(
        &mut self,
        to: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Message {
        let mut call = Message::method_call(Some(to), path, Some(interface), member)
            .unwrap()
            .with_body(body);
        call.sender = Some(AT.into());
        let serial = self.write(call);
        loop {
            let msg = self.read();
            if matches!(msg.kind, Kind::MethodReturn | Kind::Error)
                && msg.reply_serial == Some(serial)
            {
                return msg;
            }
            self.held.push_back(msg);
        }
    }
}

struct TestBuses {
    session: Option<CtlStream>,
    accessibility: VecDeque<CtlStream>,
    opened: Sender<String>,
    /// How many `open`s fail, as a bus that is not there yet fails, before
    /// the queue's buses are handed out.
    refuse: usize,
    /// [`Buses::write_timeout`]'s answer.
    write_timeout: Option<Duration>,
}

impl TestBuses {
    fn new(
        session: CtlStream,
        accessibility: impl IntoIterator<Item = CtlStream>,
        opened: Sender<String>,
    ) -> TestBuses {
        TestBuses {
            session: Some(session),
            accessibility: accessibility.into_iter().collect(),
            opened,
            refuse: 0,
            write_timeout: Some(super::WRITE),
        }
    }
}

impl Buses for TestBuses {
    fn session(&mut self) -> aterm_dbus::Result<Connection> {
        let stream = self.session.take().ok_or(aterm_dbus::Error::Closed)?;
        Connection::handshake(stream, 1000, None)
    }

    fn write_timeout(&self) -> Option<Duration> {
        self.write_timeout
    }

    fn open(&mut self, address: &str) -> aterm_dbus::Result<Connection> {
        let _ = self.opened.send(address.to_owned());
        if self.refuse > 0 {
            self.refuse -= 1;
            return Err(aterm_dbus::Error::Io(std::io::ErrorKind::NotFound.into()));
        }
        let stream = self
            .accessibility
            .pop_front()
            .ok_or(aterm_dbus::Error::Closed)?;
        Connection::handshake(stream, 1000, None)
    }
}

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

/// Every log line, for the lines the publisher must write.
struct Captured {
    lines: Mutex<Vec<String>>,
}

impl aterm_log::Log for Captured {
    fn enabled(&self, _: &aterm_log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &aterm_log::Record<'_>) {
        self.lines.lock().unwrap().push(record.args().to_string());
    }

    fn flush(&self) {}
}

fn captured() -> &'static Captured {
    static LOG: OnceLock<&'static Captured> = OnceLock::new();
    LOG.get_or_init(|| {
        let log: &'static Captured = Box::leak(Box::new(Captured {
            lines: Mutex::new(Vec::new()),
        }));
        let _ = aterm_log::set_logger(log);
        aterm_log::set_max_level(aterm_log::LevelFilter::Info);
        log
    })
}

fn heard(rx: &Receiver<Heard>) -> Heard {
    rx.recv_timeout(HANG).expect("the window heard nothing")
}

/// The session bus's opening, through to the accessibility bus's address.
fn open_session(session: &mut Peer, enabled: bool) {
    let rule = session.expect_call("AddMatch");
    assert_eq!(rule.body, vec![Value::from(proxies::STATUS_RULE)]);
    session.answer(&rule, vec![]);
    let rule = session.expect_call("AddMatch");
    assert_eq!(rule.body, vec![Value::from(proxies::OWNER_RULE)]);
    session.answer(&rule, vec![]);
    answer_status(session, enabled);
}

fn answer_status(session: &mut Peer, enabled: bool) {
    let get = session.expect_call("Get");
    assert_eq!(get.destination.as_deref(), Some("org.a11y.Bus"));
    assert_eq!(
        get.body,
        vec![Value::from("org.a11y.Status"), Value::from("IsEnabled")]
    );
    session.answer(&get, vec![Value::variant(Value::Bool(enabled))]);
    if enabled {
        let address = session.expect_call("GetAddress");
        session.answer(&address, vec![Value::from("unix:path=/run/at-spi/bus_0")]);
    }
}

/// The registry's side of `Embed`.
fn embedded(bus: &mut Peer, me: &str) {
    let embed = bus.expect_call("Embed");
    assert_eq!(
        embed.destination.as_deref(),
        Some("org.a11y.atspi.Registry")
    );
    assert_eq!(embed.body, vec![wire::object_ref(me, ObjectId::Root)]);
    bus.answer(&embed, vec![wire::object_ref(":1.3", ObjectId::Root)]);
}

/// at-spi2-core's announcement that `IsEnabled` is now `enabled`.
fn status_changed(enabled: bool) -> Message {
    let mut changed = Message::signal("/org/a11y/bus", serve::PROPERTIES, "PropertiesChanged")
        .unwrap()
        .with_body(vec![
            "org.a11y.Status".into(),
            Value::dict(
                Type::Str,
                Type::Variant,
                vec![("IsEnabled".into(), Value::variant(enabled.into()))],
            )
            .unwrap(),
            Value::strings(Vec::<String>::new()),
        ]);
    changed.sender = Some(":1.5".into());
    changed
}

fn child_path(reply: &Message) -> String {
    let Some(Value::Array(children)) = reply.body.first() else {
        panic!("{reply:?}")
    };
    match children.items() {
        [Value::Struct(fields)] => fields[1].as_str().unwrap().to_owned(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_session_survives_losing_the_bus_and_rejoins() {
    let log = captured();
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (first_client, first_bus) = CtlStream::pair().unwrap();
    let (second_client, second_bus) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let first_peer = Peer::accept(first_bus, ":1.20");
    let second_peer = Peer::accept(second_bus, ":1.21");
    let (opened_tx, opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses::new(session_client, [first_client, second_client], opened_tx),
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));

    let mut session = session_peer.join().unwrap();
    open_session(&mut session, true);
    assert_eq!(
        opened.recv_timeout(HANG).unwrap(),
        "unix:path=/run/at-spi/bus_0"
    );
    let mut bus = first_peer.join().unwrap();
    embedded(&mut bus, ":1.20");
    assert_eq!(heard(&window_heard), Heard::Activated);
    adapter.update_window_focus_state(true);
    adapter.update_if_active(|| fixture("hi\n"));

    // The window arrives under the root, and the AT walks to it.
    let added = bus.expect_signal("ChildrenChanged");
    assert_eq!(added.path.as_ref().unwrap().as_str(), ROOT_PATH);
    assert_eq!(added.body[0], Value::from("add"));
    let children = bus.ask(":1.20", ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    let window = child_path(&children);
    let name = bus.ask(
        ":1.20",
        &window,
        serve::PROPERTIES,
        "Get",
        vec![ACCESSIBLE.into(), "Name".into()],
    );
    assert_eq!(name.body, vec![Value::variant("~".into())]);
    let first = bus.ask(
        ":1.20",
        &window,
        ACCESSIBLE,
        "GetChildAtIndex",
        vec![0.into()],
    );
    let Value::Struct(fields) = &first.body[0] else {
        panic!("{first:?}")
    };
    let terminal = fields[1].as_str().unwrap().to_owned();
    let text = bus.ask(
        ":1.20",
        &terminal,
        "org.a11y.atspi.Text",
        "GetText",
        vec![0.into(), (-1).into()],
    );
    assert_eq!(text.body, vec![Value::from("lane$ echo hi\nhi\n")]);

    // A change is an event; an action is the window's.
    adapter.update_if_active(|| fixture("hi there\n"));
    let changed = bus.expect_signal("TextChanged");
    assert_eq!(changed.path.as_ref().unwrap().as_str(), terminal);
    let grab = bus.ask(
        ":1.20",
        &terminal,
        "org.a11y.atspi.Component",
        "GrabFocus",
        vec![],
    );
    assert_eq!(grab.body, vec![Value::Bool(true)]);
    assert_eq!(heard(&window_heard), Heard::Action(Action::Focus));

    // The accessibility bus dies: logged, the window told, no panic; the
    // publisher asks at-spi2-core again and rejoins the bus it now names.
    drop(bus);
    assert_eq!(heard(&window_heard), Heard::Deactivated);
    answer_status(&mut session, true);
    let mut bus = second_peer.join().unwrap();
    embedded(&mut bus, ":1.21");
    assert!(
        log.lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("the AT-SPI bus failed")),
        "the loss is logged"
    );
    assert_eq!(heard(&window_heard), Heard::Activated);
    adapter.update_if_active(|| fixture("hi\n"));
    bus.expect_signal("ChildrenChanged");
    let children = bus.ask(":1.21", ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    assert_eq!(child_path(&children), window);

    // Assistive technologies turned off: the publisher leaves the bus.
    session.write(status_changed(false));
    answer_status(&mut session, false);
    assert_eq!(heard(&window_heard), Heard::Deactivated);
    while bus.try_read().is_some() {}

    // The session bus dies: the publisher ends, quietly, and a window made
    // afterwards is simply never activated.
    drop(session);
    let late = publisher.adapter(
        Window(mpsc::channel().0),
        Window(mpsc::channel().0),
        Window(mpsc::channel().0),
    );
    drop(late);
    drop(adapter);
    drop(publisher);
}

/// The events that show an AT a window and where its focus is.
const ANNOUNCING: [&str; 4] = [
    "ChildrenChanged",
    "Activate",
    "StateChanged",
    "ActiveDescendantChanged",
];

/// What an AT on the bus hears of a window's first tree and its focus, up to
/// the focus itself, when the window had the keyboard focus before its tree
/// (`focus_first`) or after it: each event's member and kind.
fn first_announcement(focus_first: bool) -> Vec<(String, String)> {
    let _log = captured();
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (bus_client, bus_end) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let bus_peer = Peer::accept(bus_end, ":1.20");
    let (opened_tx, _opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses::new(session_client, [bus_client], opened_tx),
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
    let mut session = session_peer.join().unwrap();
    open_session(&mut session, true);
    let mut bus = bus_peer.join().unwrap();
    embedded(&mut bus, ":1.20");
    assert_eq!(heard(&window_heard), Heard::Activated);
    if focus_first {
        adapter.update_window_focus_state(true);
        adapter.update_if_active(|| fixture("hi\n"));
    } else {
        adapter.update_if_active(|| fixture("hi\n"));
        adapter.update_window_focus_state(true);
    }
    let mut heard_events = Vec::new();
    loop {
        let msg = bus.next();
        let member = msg.member.clone().unwrap_or_default();
        if msg.kind != Kind::Signal || !ANNOUNCING.contains(&member.as_str()) {
            continue;
        }
        let kind = msg
            .body
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let focused = member == "StateChanged" && kind == "focused";
        heard_events.push((member, kind));
        if focused {
            break;
        }
    }
    drop(adapter);
    drop(publisher);
    heard_events
}

/// A window focused before its first tree reached the bus is announced as one
/// focused after it: activated, then its focus. Orca names a window only
/// when it hears it activated (the Linux lane's Orca row, x86_64, 2026-10-19,
/// heard the terminal's text and no frame).
#[test]
fn a_window_focused_before_its_tree_is_announced_activated() {
    let event = |member: &str, kind: &str| (member.to_owned(), kind.to_owned());
    let announced = vec![
        event("ChildrenChanged", "add"),
        event("Activate", ""),
        event("StateChanged", "active"),
        event("ActiveDescendantChanged", ""),
        event("StateChanged", "focused"),
    ];
    assert_eq!(first_announcement(false), announced, "focus after the tree");
    assert_eq!(first_announcement(true), announced, "focus before the tree");
}

// --- a tree on its way when the bus dies ------------------------------------

/// What the window had sent the publisher thread, still unread, when the
/// first accessibility bus died.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Queued {
    /// Nothing: the negative control, whose loss the poll finds.
    Nothing,
    /// Its whole first tree.
    FirstTree,
    /// An update adding a node to a tree already published.
    NewNode,
}

const EXTRA: LocalId = LocalId(5);

/// [`fixture`] with one more button, `EXTRA`.
fn with_extra() -> TreeUpdate {
    let mut update = fixture("hi\n");
    let mut button = Node::new(Role::Button);
    button.set_label("Close tab");
    button.add_action(Action::Click);
    update.nodes[0].1.push_child(EXTRA);
    update.nodes.push((EXTRA, button));
    update
}

/// What the AT met on the second bus, once the window had published its
/// tree there: every signal before its last answer, the terminal's text,
/// and `EXTRA`'s role.
struct Rejoined {
    signals: Vec<Message>,
    text: Message,
    extra: Message,
    extra_path: String,
}

/// A session whose first accessibility bus dies with `queued` behind a
/// status re-check the publisher thread is parked in, so that the thread
/// finds the loss, and rejoins the second bus, with the old tree's
/// registrations and events still to read.
fn rejoin_after_a_loss(queued: Queued) -> Rejoined {
    let _log = captured();
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (first_client, first_bus) = CtlStream::pair().unwrap();
    let (second_client, second_bus) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let first_peer = Peer::accept(first_bus, ":1.20");
    let second_peer = Peer::accept(second_bus, ":1.21");
    let (opened_tx, _opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses::new(session_client, [first_client, second_client], opened_tx),
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
    let mut session = session_peer.join().unwrap();
    open_session(&mut session, true);
    let mut bus = first_peer.join().unwrap();
    embedded(&mut bus, ":1.20");
    assert_eq!(heard(&window_heard), Heard::Activated);
    if queued == Queued::NewNode {
        adapter.update_if_active(|| fixture("hi\n"));
        bus.expect_signal("ChildrenChanged");
    }

    // Park the thread in a status re-check, queue the update behind it,
    // kill the bus, and let the thread go: it reads the queue, finds the
    // bus gone, and asks at-spi2-core again.
    session.write(status_changed(true));
    let get = session.expect_call("Get");
    match queued {
        Queued::Nothing => {}
        Queued::FirstTree => adapter.update_if_active(|| fixture("hi\n")),
        Queued::NewNode => adapter.update_if_active(with_extra),
    }
    drop(bus);
    session.answer(&get, vec![Value::variant(Value::Bool(true))]);
    assert_eq!(heard(&window_heard), Heard::Deactivated);
    answer_status(&mut session, true);
    let mut bus = second_peer.join().unwrap();
    embedded(&mut bus, ":1.21");
    assert_eq!(heard(&window_heard), Heard::Activated);

    // The window's tree as it now stands, then the AT's walk.
    let current = if queued == Queued::NewNode {
        with_extra()
    } else {
        fixture("hi\n")
    };
    adapter.update_if_active(|| current);
    let mut signals = Vec::new();
    loop {
        let msg = bus.next();
        let arrived = msg.member.as_deref() == Some("ChildrenChanged")
            && msg.path.as_ref().is_some_and(|p| p.as_str() == ROOT_PATH)
            && msg.body.first() == Some(&Value::from("add"));
        signals.push(msg);
        if arrived {
            break;
        }
    }
    let children = bus.ask(":1.21", ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    let window = child_path(&children);
    let first = bus.ask(
        ":1.21",
        &window,
        ACCESSIBLE,
        "GetChildAtIndex",
        vec![0.into()],
    );
    let Value::Struct(fields) = &first.body[0] else {
        panic!("{first:?}")
    };
    let terminal = fields[1].as_str().unwrap().to_owned();
    let text = bus.ask(
        ":1.21",
        &terminal,
        "org.a11y.atspi.Text",
        "GetText",
        vec![0.into(), (-1).into()],
    );
    let (tree, _) = window.rsplit_once('/').unwrap();
    let extra_path = format!("{tree}/{}", u128::from(EXTRA.0) << 64);
    let extra = bus.ask(":1.21", &extra_path, ACCESSIBLE, "GetRole", vec![]);
    signals.extend(bus.held.drain(..));
    drop(adapter);
    drop(publisher);
    Rejoined {
        signals,
        text,
        extra,
        extra_path,
    }
}

/// The second bus carried only the live tree's arrival under the root: no
/// event of the tree the first bus took with it.
fn only_the_live_tree_arrived(r: &Rejoined) {
    let at_root: Vec<_> = r
        .signals
        .iter()
        .filter(|m| m.kind == Kind::Signal)
        .filter(|m| m.member.as_deref() == Some("ChildrenChanged"))
        .filter(|m| m.path.as_ref().is_some_and(|p| p.as_str() == ROOT_PATH))
        .map(|m| m.body.first().cloned())
        .collect();
    assert_eq!(at_root, [Some(Value::from("add"))], "{:?}", r.signals);
}

fn the_terminal_reads(r: &Rejoined) {
    assert_eq!(r.text.kind, Kind::MethodReturn, "{:?}", r.text);
    assert_eq!(r.text.body, vec![Value::from("lane$ echo hi\nhi\n")]);
}

fn extra_is_unknown(r: &Rejoined) {
    assert_eq!(r.extra.kind, Kind::Error, "{:?}", r.extra);
    assert_eq!(
        r.extra.error_name.as_deref(),
        Some("org.freedesktop.DBus.Error.UnknownObject")
    );
    assert_eq!(
        r.extra.body,
        vec![Value::from(format!("Unknown object '{}'", r.extra_path))]
    );
}

#[test]
fn a_rejoin_with_nothing_queued_serves_the_live_tree() {
    // The negative control: the same walk, with no tree of the first bus
    // left to read.
    let r = rejoin_after_a_loss(Queued::Nothing);
    the_terminal_reads(&r);
    extra_is_unknown(&r);
    only_the_live_tree_arrived(&r);
}

#[test]
fn a_first_tree_queued_when_the_bus_dies_is_not_served_on_the_next() {
    let r = rejoin_after_a_loss(Queued::FirstTree);
    the_terminal_reads(&r);
    extra_is_unknown(&r);
    only_the_live_tree_arrived(&r);
}

#[test]
fn a_node_queued_when_the_bus_dies_is_served_live_on_the_next() {
    let r = rejoin_after_a_loss(Queued::NewNode);
    the_terminal_reads(&r);
    assert_eq!(r.extra.kind, Kind::MethodReturn, "{:?}", r.extra);
    only_the_live_tree_arrived(&r);
}

// --- a bus that stops reading -------------------------------------------------

/// How long a write may wait here for a bus that stopped reading. The bus
/// reads nothing at all, so the bound under test is the publisher's, never
/// the machine's speed.
const STALL: Duration = Duration::from_millis(100);

/// Updates the window's text one line at a time until `until` says the
/// window heard what it waits for, or `count` updates are sent when it
/// waits for nothing (`until` is `None`).
fn keep_changing(
    adapter: &mut super::Adapter,
    window_heard: &Receiver<Heard>,
    until: Option<Heard>,
    count: usize,
) {
    let started = std::time::Instant::now();
    for i in 0.. {
        adapter.update_if_active(|| fixture(&format!("line {i}\n")));
        match &until {
            None if i + 1 >= count => return,
            None => {}
            Some(wanted) => {
                if let Ok(got) = window_heard.recv_timeout(Duration::from_millis(1))
                    && &got == wanted
                {
                    return;
                }
                assert!(
                    started.elapsed() < HANG,
                    "the window never heard {wanted:?}"
                );
            }
        }
    }
}

/// A session over accessibility buses whose writes wait `write_timeout`;
/// the first bus is joined, and the window's tree published there. With a
/// deadline there is a second bus to rejoin (`second`); without one the
/// publisher never leaves the first, and no second bus waits on a join that
/// never comes.
struct Stalling {
    publisher: Publisher,
    adapter: super::Adapter,
    window_heard: Receiver<Heard>,
    session: Peer,
    first: Peer,
    second: Option<std::thread::JoinHandle<Peer>>,
}

fn stalling(write_timeout: Option<Duration>) -> Stalling {
    let _log = captured();
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (first_client, first_bus) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let first_peer = Peer::accept(first_bus, ":1.20");
    let mut buses = vec![first_client];
    let second = write_timeout.map(|_| {
        let (second_client, second_bus) = CtlStream::pair().unwrap();
        buses.push(second_client);
        Peer::accept(second_bus, ":1.21")
    });
    let (opened_tx, _opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses {
            write_timeout,
            ..TestBuses::new(session_client, buses, opened_tx)
        },
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let mut adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
    let mut session = session_peer.join().unwrap();
    open_session(&mut session, true);
    let mut first = first_peer.join().unwrap();
    embedded(&mut first, ":1.20");
    assert_eq!(heard(&window_heard), Heard::Activated);
    adapter.update_window_focus_state(true);
    adapter.update_if_active(|| fixture("hi\n"));
    first.expect_signal("ChildrenChanged");
    Stalling {
        publisher,
        adapter,
        window_heard,
        session,
        first,
        second,
    }
}

#[test]
fn a_bus_that_stops_reading_is_left_and_the_next_one_joined() {
    let log = captured();
    let mut s = stalling(Some(STALL));
    // The first bus reads nothing more while the text keeps changing: its
    // socket fills, a write waits out its bound, and the window is told.
    keep_changing(&mut s.adapter, &s.window_heard, Some(Heard::Deactivated), 0);
    assert!(
        log.lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("the AT-SPI bus stopped reading")),
        "the stall is logged"
    );
    // The publisher asks at-spi2-core again and joins the bus it names.
    answer_status(&mut s.session, true);
    let mut bus = s.second.take().expect("a second bus").join().unwrap();
    embedded(&mut bus, ":1.21");
    assert_eq!(heard(&s.window_heard), Heard::Activated);
    s.adapter.update_if_active(|| fixture("after\n"));
    bus.expect_signal("ChildrenChanged");
    let children = bus.ask(":1.21", ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    let window = child_path(&children);
    let first = bus.ask(
        ":1.21",
        &window,
        ACCESSIBLE,
        "GetChildAtIndex",
        vec![0.into()],
    );
    let Value::Struct(fields) = &first.body[0] else {
        panic!("{first:?}")
    };
    let text = bus.ask(
        ":1.21",
        fields[1].as_str().unwrap(),
        "org.a11y.atspi.Text",
        "GetText",
        vec![0.into(), (-1).into()],
    );
    assert_eq!(text.body, vec![Value::from("lane$ echo hi\nafter\n")]);
    drop(s.first);
    drop(s.adapter);
    drop(s.publisher);
}

#[test]
fn without_a_write_deadline_a_bus_that_stops_reading_holds_the_publisher() {
    // The negative control: the same bus, with writes that wait for as long
    // as the bus does. Ten times the bound above passes and the window is
    // still not told; once the bus reads again, every change it held
    // arrives, and the window was never let go.
    const CHANGES: usize = 2000;
    let mut s = stalling(None);
    keep_changing(&mut s.adapter, &s.window_heard, None, CHANGES);
    let early = s.window_heard.recv_timeout(STALL * 10);
    assert!(
        early.is_err(),
        "the window heard {early:?} with the bus stalled"
    );
    let answer = s
        .first
        .ask(":1.20", ROOT_PATH, ACCESSIBLE, "GetChildren", vec![]);
    assert_eq!(answer.kind, Kind::MethodReturn, "{answer:?}");
    let changed = s
        .first
        .held
        .iter()
        .filter(|m| m.member.as_deref() == Some("TextChanged"))
        .count();
    assert!(
        changed >= CHANGES,
        "{changed} text changes arrived of the {CHANGES} sent"
    );
    assert!(s.window_heard.try_recv().is_err());
    drop(s.adapter);
    drop(s.publisher);
}

#[test]
fn a_join_that_fails_is_tried_again() {
    let log = captured();
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (bus_client, bus_end) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let bus_peer = Peer::accept(bus_end, ":1.30");
    let (opened_tx, opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses {
            refuse: 1,
            ..TestBuses::new(session_client, [bus_client], opened_tx)
        },
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
    let mut session = session_peer.join().unwrap();
    // The first join fails, and nothing on the session bus changes after it.
    open_session(&mut session, true);
    opened.recv_timeout(HANG).unwrap();
    // A second later the publisher asks again, unprompted, and joins.
    answer_status(&mut session, true);
    opened.recv_timeout(HANG).unwrap();
    let mut bus = bus_peer.join().unwrap();
    embedded(&mut bus, ":1.30");
    assert_eq!(heard(&window_heard), Heard::Activated);
    assert!(
        log.lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("cannot join the AT-SPI bus") && l.contains("in 1 s")),
        "the failure and the next attempt are logged"
    );
    drop(adapter);
    drop(publisher);
}

/// A status read that fails while the publisher waits to rejoin (at-spi2-core
/// busy, or answering an error) keeps the wait going, longer, instead of
/// ending the attempts until the next status signal.
#[test]
fn a_status_unanswered_during_the_backoff_keeps_it() {
    let (session_client, session_bus) = CtlStream::pair().unwrap();
    let (bus_client, bus_end) = CtlStream::pair().unwrap();
    let session_peer = Peer::accept(session_bus, ":1.10");
    let bus_peer = Peer::accept(bus_end, ":1.30");
    let (opened_tx, opened) = mpsc::channel();
    let publisher = Publisher::start(
        TestBuses {
            refuse: 1,
            ..TestBuses::new(session_client, [bus_client], opened_tx)
        },
        Some("aterm-test".into()),
    );
    let (tx, window_heard) = mpsc::channel();
    let adapter = publisher.adapter(Window(tx.clone()), Window(tx.clone()), Window(tx));
    let mut session = session_peer.join().unwrap();
    // The first join fails; a second later the status read that would retry
    // it is answered with an error.
    open_session(&mut session, true);
    opened.recv_timeout(HANG).unwrap();
    let get = session.expect_call("Get");
    let mut failed = Message::error(
        &get,
        "org.freedesktop.DBus.Error.NoReply",
        "at-spi-bus-launcher did not answer",
    )
    .unwrap();
    failed.sender = Some("org.freedesktop.DBus".into());
    session.write(failed);
    // Two seconds after that, unprompted, the publisher asks again and joins.
    answer_status(&mut session, true);
    opened.recv_timeout(HANG).unwrap();
    let mut bus = bus_peer.join().unwrap();
    embedded(&mut bus, ":1.30");
    assert_eq!(heard(&window_heard), Heard::Activated);
    drop(adapter);
    drop(publisher);
}

/// The desktop's join of the accessibility bus gives up on a daemon that
/// never answers its handshake (a frozen one's listener still takes the
/// connection into its backlog) within [`super::WRITE`], not aterm-dbus's
/// 25 s handshake default.
#[test]
fn the_desktop_s_join_gives_up_on_a_silent_bus_within_the_write_deadline() {
    let path = std::env::temp_dir().join(format!("aterm-a11y-frozen-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _frozen = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let asked = std::time::Instant::now();
    let joined = super::SessionBuses.open(&format!("unix:path={}", path.display()));
    let waited = asked.elapsed();
    assert!(
        matches!(joined, Err(aterm_dbus::Error::Timeout)),
        "{:?}",
        joined.err()
    );
    // The two bounds compared, not the machine's speed.
    assert!(
        waited < aterm_dbus::conn::HANDSHAKE_TIMEOUT,
        "the join gave up after {waited:?}, the handshake default's, not WRITE's"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_failed_join_waits_twice_as_long_each_time_up_to_a_minute() {
    let now = std::time::Instant::now();
    let mut waits = Vec::new();
    let mut last = None;
    for _ in 0..9 {
        last = super::publisher::Rejoin::next(last, now);
        waits.push(last.unwrap().after.as_secs());
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The semantics oracle's output: every scenario of the corpus played on
//! the bus-free AT-SPI semantics, and everything the publisher can ask of
//! them written down, the same source compiled twice.
//!
//! `tests/recorder` compiles this file against registry
//! `accesskit_atspi_common` 0.19.0 (with `accesskit_consumer` 0.37.0 under
//! it), which wrote `tests/recorded/`; the replay in `src/atspi/oracle.rs`
//! against aterm-a11y's own semantics, which must write the same. Both see
//! it as a sibling of `model`, `sem` and `corpus` modules; `sem` names the
//! application context's handle `SharedApp`, the one type the two sides
//! spell differently.
//!
//! After each step it writes:
//!
//! * the step's events, from the adapter callback: registrations, object,
//!   window and cache events, sorted (the registry crates emit them in hash
//!   order, which differs from run to run, so a step's events are a set);
//! * the tree: the application root, every cache item and, for every node,
//!   each `Accessible`, `Action`, `Component`, `Hyperlink`, `Selection` and
//!   `Value` answer the publisher serves;
//! * the text: each `Text` answer at every offset (and one past either end)
//!   and every granularity, and at points across the node;
//! * the actions: what every action-producing call returns and the action
//!   requests it hands the window.
//!
//! The recording keeps the events as text and each other block as a 64-bit
//! FNV-1a digest of its text, so it stays small; `--full` (recorder) and a
//! failing replay print the text itself.
//!
//! The blank-line table ([`blank_line_answer`]) is apart: one row per text
//! offset and granularity of each of the corpus's blank-line documents.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use super::corpus::{Document, Scenario, Step};
use super::model::{ActionData, ActionHandler, ActionRequest, Point};
use super::sem::{
    ActionHandlerWrapper, Adapter, AdapterCallback, AppContext, CacheEvent, CacheNode, CoordType,
    Event, Granularity, InterfaceSet, NodeId, NodeIdOrRoot, ObjectEvent, PlatformNode,
    PlatformRoot, Property, Rect as AtspiRect, ScrollType, SharedApp, WindowBounds, WindowEvent,
};

/// What the callback and the action handler write to.
type Sink = Arc<Mutex<Vec<String>>>;

fn push(sink: &Sink, line: String) {
    sink.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

fn drain(sink: &Sink) -> Vec<String> {
    std::mem::take(&mut *sink.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Compare semantic node names in this single-tree corpus (`TreeId::ROOT`).
/// Registry graft indices are always 0 here; aterm's exported incarnation uses
/// that low word for object lifetime instead. Wire-path/held-object tests cover
/// those identities separately; this oracle compares the same model-node names.
fn nid(id: NodeId) -> String {
    format!("n{}", u128::from(id) >> 64)
}

fn target(t: NodeIdOrRoot) -> String {
    match t {
        NodeIdOrRoot::Node(id) => nid(id),
        NodeIdOrRoot::Root => "root".to_owned(),
    }
}

fn arect(r: AtspiRect) -> String {
    format!("({},{} {}x{})", r.x, r.y, r.width, r.height)
}

struct Callback {
    sink: Sink,
}

impl AdapterCallback for Callback {
    fn register_interfaces(&self, adapter: &Adapter, id: NodeId, interfaces: InterfaceSet) {
        push(
            &self.sink,
            format!(
                "w{} register {} {:#x}",
                adapter.id(),
                nid(id),
                interfaces.bits()
            ),
        );
    }

    fn unregister_interfaces(&self, adapter: &Adapter, id: NodeId, interfaces: InterfaceSet) {
        push(
            &self.sink,
            format!(
                "w{} unregister {} {:#x}",
                adapter.id(),
                nid(id),
                interfaces.bits()
            ),
        );
    }

    fn emit_event(&self, adapter: &Adapter, event: Event) {
        push(
            &self.sink,
            format!("w{} {}", adapter.id(), event_text(&event)),
        );
    }
}

fn event_text(event: &Event) -> String {
    match event {
        Event::Object { target: t, event } => {
            format!("object {} {}", target(*t), object_event(event))
        }
        Event::Window {
            target: t,
            name,
            event,
        } => {
            let what = match event {
                WindowEvent::Activated => "activated",
                WindowEvent::Deactivated => "deactivated",
            };
            format!("window {} {name:?} {what}", nid(*t))
        }
        Event::Cache(CacheEvent::Added(id)) => format!("cache-add {}", nid(*id)),
        Event::Cache(CacheEvent::Removed(id)) => format!("cache-remove {}", nid(*id)),
    }
}

fn object_event(event: &ObjectEvent) -> String {
    match event {
        ObjectEvent::ActiveDescendantChanged(id) => format!("active-descendant {}", nid(*id)),
        ObjectEvent::Announcement(text, politeness) => {
            format!("announce {text:?} {}", *politeness as i32)
        }
        ObjectEvent::BoundsChanged(r) => format!("bounds {}", arect(*r)),
        ObjectEvent::CaretMoved(offset) => format!("caret {offset}"),
        ObjectEvent::ChildAdded(index, id) => format!("child-added {index} {}", nid(*id)),
        ObjectEvent::ChildRemoved(id) => format!("child-removed {}", nid(*id)),
        ObjectEvent::PropertyChanged(property) => match property {
            Property::Name(name) => format!("name {name:?}"),
            Property::Description(text) => format!("description {text:?}"),
            Property::Parent(parent) => format!("parent {}", target(*parent)),
            Property::Role(role) => format!("role {}", *role as u32),
            Property::Value(value) => format!("value {value}"),
        },
        ObjectEvent::SelectionChanged => "selection-changed".to_owned(),
        ObjectEvent::StateChanged(state, on) => {
            format!("state {} {on}", state.to_static_str())
        }
        ObjectEvent::TextInserted {
            start_index,
            length,
            content,
        } => format!("text-inserted {start_index} {length} {content:?}"),
        ObjectEvent::TextRemoved {
            start_index,
            length,
            content,
        } => format!("text-removed {start_index} {length} {content:?}"),
        ObjectEvent::TextSelectionChanged => "text-selection-changed".to_owned(),
    }
}

/// The window's action handler: each request, written down.
struct Requests {
    sink: Sink,
}

impl ActionHandler for Requests {
    fn do_action(&mut self, request: ActionRequest) {
        push(&self.sink, request_text(&request));
    }
}

fn point(p: Point) -> String {
    format!("({},{})", p.x, p.y)
}

fn request_text(request: &ActionRequest) -> String {
    let data = match &request.data {
        None => "-".to_owned(),
        Some(ActionData::CustomAction(id)) => format!("custom {id}"),
        Some(ActionData::Value(value)) => format!("value {value:?}"),
        Some(ActionData::NumericValue(value)) => format!("numeric {value}"),
        Some(ActionData::ScrollUnit(unit)) => format!("unit {unit:?}"),
        Some(ActionData::ScrollHint(hint)) => format!("hint {hint:?}"),
        Some(ActionData::ScrollToPoint(p)) => format!("to {}", point(*p)),
        Some(ActionData::SetScrollOffset(p)) => format!("offset {}", point(*p)),
        Some(ActionData::SetTextSelection(s)) => format!(
            "select n{}:{} n{}:{}",
            s.anchor.node.0, s.anchor.character_index, s.focus.node.0, s.focus.character_index
        ),
    };
    format!(
        "request {:?} n{} {data}",
        request.action, request.target_node.0
    )
}

/// One step's written blocks.
struct StepText {
    events: Vec<String>,
    tree: String,
    text: String,
    actions: String,
}

/// `Ok` as its value, `Err` as the error's text.
fn res<T: std::fmt::Debug, E: std::fmt::Display>(r: Result<T, E>) -> String {
    match r {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("err {e}"),
    }
}

fn sorted_map<K: AsRef<str>>(map: std::collections::HashMap<K, String>) -> String {
    let sorted: BTreeMap<String, String> = map
        .into_iter()
        .map(|(k, v)| (k.as_ref().to_owned(), v))
        .collect();
    format!("{sorted:?}")
}

const COORDS: [CoordType; 3] = [CoordType::Screen, CoordType::Window, CoordType::Parent];
const GRANULARITIES: [Granularity; 5] = [
    Granularity::Char,
    Granularity::Word,
    Granularity::Sentence,
    Granularity::Line,
    Granularity::Paragraph,
];
const SCROLLS: [ScrollType; 7] = [
    ScrollType::TopLeft,
    ScrollType::BottomRight,
    ScrollType::TopEdge,
    ScrollType::BottomEdge,
    ScrollType::LeftEdge,
    ScrollType::RightEdge,
    ScrollType::Anywhere,
];

/// Points around and inside `r` (window coordinates) for hit tests.
fn points(r: Option<AtspiRect>) -> Vec<(i32, i32)> {
    let mut out = vec![(0, 0), (-5, -5)];
    if let Some(r) = r.filter(|r| r.width >= 0 && r.height >= 0) {
        out.extend([
            (r.x, r.y),
            (r.x + r.width / 2, r.y + r.height / 2),
            (r.x + r.width - 1, r.y + r.height - 1),
            (r.x + r.width, r.y + r.height),
            (r.x + r.width / 4, r.y + r.height / 2),
        ]);
    }
    out
}

/// One window's adapter while open.
struct Window {
    adapter: Adapter,
}

struct Player {
    app: SharedApp,
    events: Sink,
    requests: Sink,
    windows: BTreeMap<usize, Window>,
}

impl Player {
    fn new(name: &str) -> Self {
        Self {
            app: AppContext::new(Some(name.to_owned())),
            events: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            windows: BTreeMap::new(),
        }
    }

    fn step(&mut self, step: Step) {
        match step {
            Step::Open {
                window,
                update,
                focused,
            } => {
                let handler = Arc::new(ActionHandlerWrapper::new(Requests {
                    sink: Arc::clone(&self.requests),
                }));
                let adapter = Adapter::with_wrapped_action_handler(
                    window,
                    &self.app,
                    Callback {
                        sink: Arc::clone(&self.events),
                    },
                    update,
                    focused,
                    WindowBounds::default(),
                    handler,
                );
                self.windows.insert(window, Window { adapter });
            }
            Step::Update { window, update } => {
                if let Some(w) = self.windows.get_mut(&window) {
                    w.adapter.update(update);
                }
            }
            Step::Focus { window, focused } => {
                if let Some(w) = self.windows.get_mut(&window) {
                    w.adapter.update_window_focus_state(focused);
                }
            }
            Step::Bounds {
                window,
                outer,
                inner,
            } => {
                if let Some(w) = self.windows.get_mut(&window) {
                    w.adapter
                        .set_root_window_bounds(WindowBounds::new(outer, inner));
                }
            }
            Step::Close { window } => {
                self.windows.remove(&window);
            }
        }
    }

    /// Everything the publisher can ask, after a step.
    fn probe(&self) -> StepText {
        let mut events = drain(&self.events);
        events.sort();
        let mut tree = String::new();
        let mut text = String::new();
        let mut actions = String::new();
        let mut root = PlatformRoot::new(&self.app);
        let _ = writeln!(
            tree,
            "root name={} desc={} children={} toolkit={} version={} id={} role={} \
             index={} state={:#x} interfaces={:#x}",
            res(root.name()),
            res(root.description()),
            res(root.child_count()),
            res(root.toolkit_name()),
            res(root.toolkit_version()),
            res(root.id()),
            root.role() as u32,
            root.index_in_parent(),
            root.state().bits(),
            root.interfaces().bits(),
        );
        let ids: Result<Vec<String>, _> =
            root.map_child_ids(|(adapter, id)| format!("w{adapter}/{}", nid(id)));
        let _ = writeln!(tree, "root children {}", res(ids));
        for i in 0..4 {
            let child = root
                .child_id_at_index(i)
                .map(|c| c.map(|(a, id)| (a, nid(id))));
            let _ = writeln!(tree, "root child {i} {}", res(child));
        }
        let cache: Result<Vec<(usize, NodeId, String)>, _> =
            root.map_descendant_cache_nodes(|c| (c.adapter_id, c.id, cache_text(&c)));
        let cache = match cache {
            Ok(cache) => cache,
            Err(e) => {
                let _ = writeln!(tree, "cache err {e}");
                Vec::new()
            }
        };
        for (_, _, item) in &cache {
            let _ = writeln!(tree, "cache {item}");
        }
        for (adapter, id, _) in &cache {
            let Some(window) = self.windows.get(adapter) else {
                continue;
            };
            let node = window.adapter.platform_node(*id);
            let at = format!("w{adapter}/{}", nid(*id));
            self.probe_node(&node, &at, &mut tree);
            self.probe_text(&node, &at, &mut text);
            self.probe_actions(&node, &at, &mut actions);
        }
        // The registry's application id, written as an AT-SPI registry would.
        let _ = writeln!(
            actions,
            "root set_id {} then {}",
            res(root.set_id(7)),
            res(root.id())
        );
        StepText {
            events,
            tree,
            text,
            actions,
        }
    }

    fn probe_node(&self, node: &PlatformNode, at: &str, out: &mut String) {
        let ids = |id: NodeId| nid(id);
        let _ = writeln!(
            out,
            "{at} name={} desc={} parent={} children={} accessible_id={} index={}",
            res(node.name()),
            res(node.description()),
            res(node.parent().map(target)),
            res(node.child_count()),
            res(node.accessible_id()),
            res(node.index_in_parent()),
        );
        let children: Result<Vec<String>, _> = node.map_children(ids);
        let _ = writeln!(out, "{at} children {}", res(children));
        let count = node.child_count().unwrap_or(0).max(0) as usize;
        for i in 0..=count {
            let _ = writeln!(
                out,
                "{at} child {i} {}",
                res(node.child_at_index(i).map(|c| c.map(ids)))
            );
        }
        let cache = node.cache_node().map(|c| cache_text(&c));
        let _ = writeln!(out, "{at} cache {}", res(cache));
        let relations = node.relation_set(ids).map(|set| {
            let mut set: Vec<(u32, Vec<String>)> =
                set.into_iter().map(|(k, v)| (k as u32, v)).collect();
            set.sort();
            set
        });
        let _ = writeln!(
            out,
            "{at} relations={} role={} role_name={} state={:#x} attributes={} interfaces={}",
            res(relations),
            res(node.role().map(|r| r as u32)),
            res(node.localized_role_name()),
            node.state().bits(),
            res(node.attributes().map(sorted_map)),
            res(node.interfaces().map(|i| format!("{:#x}", i.bits()))),
        );
        let actions = node.actions().map(|list| {
            list.into_iter()
                .map(|a| format!("{}|{}|{}", a.localized_name, a.description, a.key_binding))
                .collect::<Vec<_>>()
        });
        let _ = writeln!(
            out,
            "{at} n_actions={} names={},{},{} actions={}",
            res(node.n_actions()),
            res(node.action_name(-1)),
            res(node.action_name(0)),
            res(node.action_name(1)),
            res(actions),
        );
        let mut window_extents = None;
        for coords in COORDS {
            let extents = node.extents(coords);
            if coords == CoordType::Window {
                window_extents = extents.as_ref().ok().copied();
            }
            let _ = writeln!(out, "{at} extents {coords:?} {}", res(extents.map(arect)));
        }
        for (x, y) in points(window_extents) {
            for coords in COORDS {
                let _ = writeln!(
                    out,
                    "{at} at {x},{y} {coords:?} contains={} hit={}",
                    res(node.contains(x, y, coords)),
                    res(node.accessible_at_point(x, y, coords).map(|h| h.map(ids))),
                );
            }
        }
        let _ = writeln!(
            out,
            "{at} layer={} anchors={} start={} end={} object0={} object1={} uri0={} uri1={} \
             valid={}",
            res(node.layer().map(|l| l as u32)),
            res(node.n_anchors()),
            res(node.hyperlink_start_index()),
            res(node.hyperlink_end_index()),
            res(node.hyperlink_object(0).map(|o| o.map(ids))),
            res(node.hyperlink_object(1).map(|o| o.map(ids))),
            res(node.uri(0)),
            res(node.uri(1)),
            res(node.hyperlink_is_valid()),
        );
        let _ = writeln!(
            out,
            "{at} selected_count={}",
            res(node.n_selected_children())
        );
        for i in 0..=count {
            let _ = writeln!(
                out,
                "{at} selected {i} {} is_selected={}",
                res(node.selected_child(i).map(|c| c.map(ids))),
                res(node.is_child_selected(i)),
            );
        }
        let _ = writeln!(
            out,
            "{at} min={} max={} step={} value={}",
            res(node.minimum_value()),
            res(node.maximum_value()),
            res(node.minimum_increment()),
            res(node.current_value()),
        );
    }

    fn probe_text(&self, node: &PlatformNode, at: &str, out: &mut String) {
        let Ok(count) = node.character_count() else {
            return;
        };
        let _ = writeln!(
            out,
            "{at} count={count} caret={} selections={} selection0={} selection1={} \
             all={} defaults={}",
            res(node.caret_offset()),
            res(node.n_selections()),
            res(node.selection(0)),
            res(node.selection(1)),
            res(node.text(0, -1)),
            res(node.default_text_attributes().map(sorted_map)),
        );
        for offset in -1..=count + 1 {
            for g in GRANULARITIES {
                let _ = writeln!(
                    out,
                    "{at} @{offset} {g:?} {}",
                    res(node.string_at_offset(offset, g))
                );
            }
            let run = |defaults| {
                node.text_attribute_run(offset, defaults)
                    .map(|(m, s, e)| (sorted_map(m), s, e))
            };
            let _ = writeln!(
                out,
                "{at} @{offset} attrs={} run={} run_defaults={} family={} bogus={}",
                res(node
                    .text_attributes(offset)
                    .map(|(m, s, e)| (sorted_map(m), s, e))),
                res(run(false)),
                res(run(true)),
                res(node.text_attribute_value(offset, "family-name")),
                res(node.text_attribute_value(offset, "bogus")),
            );
            for coords in COORDS {
                let _ = writeln!(
                    out,
                    "{at} @{offset} extents {coords:?} {}",
                    res(node.character_extents(offset, coords).map(arect))
                );
            }
        }
        let pairs = [
            (0, 0),
            (0, 1),
            (1, 3),
            (0, count),
            (2, count - 1),
            (count, 0),
            (-1, 2),
            (count, count + 1),
            (1, -1),
        ];
        for (a, b) in pairs {
            let mut line = format!("{at} [{a},{b}] text={}", res(node.text(a, b)));
            for coords in COORDS {
                let _ = write!(
                    line,
                    " {coords:?}={}",
                    res(node.range_extents(a, b, coords).map(arect))
                );
            }
            let _ = writeln!(out, "{line}");
        }
        let extents = node.extents(CoordType::Window).ok();
        for (x, y) in points(extents) {
            for coords in COORDS {
                let _ = writeln!(
                    out,
                    "{at} offset at {x},{y} {coords:?} {}",
                    res(node.offset_at_point(x, y, coords))
                );
            }
        }
    }

    fn probe_actions(&self, node: &PlatformNode, at: &str, out: &mut String) {
        let mut call = |what: String, result: String| {
            let requests = drain(&self.requests);
            let _ = writeln!(out, "{at} {what} -> {result} {requests:?}");
        };
        call("do_action 0".into(), res(node.do_action(0)));
        call("do_action 1".into(), res(node.do_action(1)));
        call("grab_focus".into(), res(node.grab_focus()));
        for kind in SCROLLS {
            call(format!("scroll_to {kind:?}"), res(node.scroll_to(kind)));
        }
        for coords in COORDS {
            call(
                format!("scroll_to_point {coords:?}"),
                res(node.scroll_to_point(coords, 30, 40)),
            );
        }
        let count = node.child_count().unwrap_or(0).max(0) as usize;
        for i in 0..=count {
            call(format!("select_child {i}"), res(node.select_child(i)));
            call(format!("deselect_child {i}"), res(node.deselect_child(i)));
            call(
                format!("deselect_selected_child {i}"),
                res(node.deselect_selected_child(i)),
            );
        }
        call("select_all".into(), res(node.select_all()));
        call("clear_selection".into(), res(node.clear_selection()));
        call(
            "set_current_value".into(),
            res(node.set_current_value(42.5)),
        );
        let Ok(chars) = node.character_count() else {
            return;
        };
        for offset in [-1, 0, 1, chars / 2, chars, chars + 1] {
            call(
                format!("set_caret_offset {offset}"),
                res(node.set_caret_offset(offset)),
            );
        }
        for (n, a, b) in [(0, 0, 2), (0, 1, -1), (1, 0, 1), (0, chars, 0), (0, 5, 99)] {
            call(
                format!("set_selection {n} {a} {b}"),
                res(node.set_selection(n, a, b)),
            );
        }
        call("add_selection 1 2".into(), res(node.add_selection(1, 2)));
        call("remove_selection 0".into(), res(node.remove_selection(0)));
        call("remove_selection 1".into(), res(node.remove_selection(1)));
        for kind in SCROLLS {
            call(
                format!("scroll_substring_to {kind:?}"),
                res(node.scroll_substring_to(0, chars, kind)),
            );
        }
        call(
            "scroll_substring_to 9 99".into(),
            res(node.scroll_substring_to(9, 99, ScrollType::TopLeft)),
        );
        for coords in COORDS {
            call(
                format!("scroll_substring_to_point {coords:?}"),
                res(node.scroll_substring_to_point(1, 2, coords, 50, 60)),
            );
        }
    }
}

fn cache_text(c: &CacheNode) -> String {
    format!(
        "w{}/{} parent={} index={} children={} interfaces={:#x} name={:?} desc={:?} role={} \
         states={:#x}",
        c.adapter_id,
        nid(c.id),
        target(c.parent),
        c.index_in_parent,
        c.child_count,
        c.interfaces.bits(),
        c.name,
        c.description,
        c.role as u32,
        c.states.bits(),
    )
}

/// The granularities of the blank-line table: `Sentence` is unsupported on
/// both sides, and the scenarios hold that.
pub(crate) const BLANK_LINE_GRANULARITIES: [Granularity; 4] = [
    Granularity::Char,
    Granularity::Word,
    Granularity::Line,
    Granularity::Paragraph,
];

/// A row key of the blank-line table.
pub(crate) fn blank_line_key(document: &Document, offset: i32, g: Granularity) -> String {
    format!("{} @{offset} {g:?}", document.label())
}

/// What the publisher answers `GetStringAtOffset(offset, g)` with on
/// `document`'s text input, its window opened alone. On the registry crates
/// this may never return (a blank line before the offset sent
/// accesskit_consumer's paragraph search round forever), so the recorder
/// asks each on its own thread.
pub(crate) fn blank_line_answer(document: &Document, offset: i32, g: Granularity) -> String {
    let mut player = Player::new("aterm");
    player.step(Step::Open {
        window: 0,
        update: document.update(),
        focused: true,
    });
    let root = PlatformRoot::new(&player.app);
    let ids: Vec<NodeId> = root
        .map_descendant_cache_nodes(|c| c.id)
        .unwrap_or_default();
    let Some(window) = player.windows.get(&0) else {
        return "no window".to_owned();
    };
    ids.into_iter()
        .map(|id| window.adapter.platform_node(id))
        .find(|node| node.character_count().is_ok())
        .map_or_else(
            || "no text".to_owned(),
            |node| res(node.string_at_offset(offset, g)),
        )
}

/// FNV-1a, 64 bits: the same digest on both sides of the oracle.
pub(crate) fn digest(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The transcript of `scenario`: one block per step. `full` writes each
/// probe's text; otherwise its digest.
pub(crate) fn play(scenario: Scenario, full: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "scenario {}", scenario.name);
    let mut player = Player::new("aterm");
    for (i, step) in scenario.steps.into_iter().enumerate() {
        let kind = match &step {
            Step::Open { window, .. } => format!("open w{window}"),
            Step::Update { window, update } => {
                format!("update w{window} ({} nodes)", update.nodes.len())
            }
            Step::Focus { window, focused } => format!("focus w{window} {focused}"),
            Step::Bounds { window, .. } => format!("bounds w{window}"),
            Step::Close { window } => format!("close w{window}"),
        };
        player.step(step);
        let probe = player.probe();
        let _ = writeln!(out, "step {i} {kind}");
        for event in &probe.events {
            let _ = writeln!(out, "  {event}");
        }
        for (name, block) in [
            ("tree", &probe.tree),
            ("text", &probe.text),
            ("actions", &probe.actions),
        ] {
            if full {
                for line in block.lines() {
                    let _ = writeln!(out, "  {name}| {line}");
                }
            } else {
                let _ = writeln!(
                    out,
                    "  {name} {:016x} {}",
                    digest(block),
                    block.lines().count()
                );
            }
        }
    }
    // The windows close as the application exits.
    let names: Vec<usize> = player.windows.keys().copied().collect();
    for window in names {
        player.windows.remove(&window);
    }
    let mut closing = drain(&player.events);
    closing.sort();
    let _ = writeln!(out, "exit");
    for event in closing {
        let _ = writeln!(out, "  {event}");
    }
    out
}

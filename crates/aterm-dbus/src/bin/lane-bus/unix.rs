// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus` on unix: the verbs, over this crate's [`Connection`].

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_dbus::{Connection, Message, Value, address};

/// One call's deadline.
const CALL: Duration = Duration::from_secs(10);

/// The hang detector of `atspi-listen`, and of a `seat` step that waits for
/// the lab (whose own waits are one-minute hang detectors): it never decides a
/// verdict.
const LISTEN_LIMIT: Duration = Duration::from_secs(600);

/// A wait for something that must happen (a frame's focus to move): a hang
/// detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// How often a wait polls.
const POLL: Duration = Duration::from_millis(100);

/// The AT-SPI state bit `active` (at-spi2-core's `ATSPI_STATE_ACTIVE`): a
/// frame's window holds the keyboard focus.
const ACTIVE_BIT: u32 = 1;

/// The walk's bounds: a tree deeper or larger than this is reported cut.
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 2000;

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const BUS: &str = "org.freedesktop.DBus";
const RD: &str = "org.gnome.Mutter.RemoteDesktop";
const RD_PATH: &str = "/org/gnome/Mutter/RemoteDesktop";
const RD_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";

/// The AT-SPI 2 roles, by number (at-spi2-core's `AtspiRole`).
const ROLES: [&str; 130] = [
    "invalid",
    "accelerator_label",
    "alert",
    "animation",
    "arrow",
    "calendar",
    "canvas",
    "check_box",
    "check_menu_item",
    "color_chooser",
    "column_header",
    "combo_box",
    "date_editor",
    "desktop_icon",
    "desktop_frame",
    "dial",
    "dialog",
    "directory_pane",
    "drawing_area",
    "file_chooser",
    "filler",
    "focus_traversable",
    "font_chooser",
    "frame",
    "glass_pane",
    "html_container",
    "icon",
    "image",
    "internal_frame",
    "label",
    "layered_pane",
    "list",
    "list_item",
    "menu",
    "menu_bar",
    "menu_item",
    "option_pane",
    "page_tab",
    "page_tab_list",
    "panel",
    "password_text",
    "popup_menu",
    "progress_bar",
    "push_button",
    "radio_button",
    "radio_menu_item",
    "root_pane",
    "row_header",
    "scroll_bar",
    "scroll_pane",
    "separator",
    "slider",
    "spin_button",
    "split_pane",
    "status_bar",
    "table",
    "table_cell",
    "table_column_header",
    "table_row_header",
    "tearoff_menu_item",
    "terminal",
    "text",
    "toggle_button",
    "tool_bar",
    "tool_tip",
    "tree",
    "tree_table",
    "unknown",
    "viewport",
    "window",
    "extended",
    "header",
    "footer",
    "paragraph",
    "ruler",
    "application",
    "autocomplete",
    "editbar",
    "embedded",
    "entry",
    "chart",
    "caption",
    "document_frame",
    "heading",
    "page",
    "section",
    "redundant_object",
    "form",
    "link",
    "input_method_window",
    "table_row",
    "tree_item",
    "document_spreadsheet",
    "document_presentation",
    "document_text",
    "document_web",
    "document_email",
    "comment",
    "list_box",
    "grouping",
    "image_map",
    "notification",
    "info_bar",
    "level_bar",
    "title_bar",
    "block_quote",
    "audio",
    "video",
    "definition",
    "article",
    "landmark",
    "log",
    "marquee",
    "math",
    "rating",
    "timer",
    "static",
    "math_fraction",
    "math_root",
    "subscript",
    "superscript",
    "description_list",
    "description_term",
    "description_value",
    "footnote",
    "content_deletion",
    "content_insertion",
    "mark",
    "suggestion",
    "push_button_menu",
];

/// The AT-SPI 2 states, by bit (at-spi2-core's `AtspiStateType`).
const STATES: [&str; 45] = [
    "invalid",
    "active",
    "armed",
    "busy",
    "checked",
    "collapsed",
    "defunct",
    "editable",
    "enabled",
    "expandable",
    "expanded",
    "focusable",
    "focused",
    "has_tooltip",
    "horizontal",
    "iconified",
    "modal",
    "multi_line",
    "multiselectable",
    "opaque",
    "pressed",
    "resizable",
    "selectable",
    "selected",
    "sensitive",
    "showing",
    "single_line",
    "stale",
    "transient",
    "vertical",
    "visible",
    "manages_descendants",
    "indeterminate",
    "required",
    "truncated",
    "animated",
    "invalid_entry",
    "supports_autocompletion",
    "selectable_text",
    "is_default",
    "visited",
    "checkable",
    "has_popup",
    "read_only",
    // Bit 44: AT-SPI's `ATSPI_STATE_LAST_DEFINED` marker, never set.
    "last_defined",
];

/// What failed, and whether it means "not here" (exit 3) or a fault (exit 1).
#[derive(Debug, PartialEq, Eq)]
enum Fail {
    NotHere(String),
    Fault(String),
    Usage(String),
}

impl Fail {
    fn fault(e: impl std::fmt::Display) -> Fail {
        Fail::Fault(e.to_string())
    }
}

pub(crate) fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("atspi-walk") => match (args.get(1), args.get(2).map(|p| p.parse::<u32>())) {
            (Some(app), Some(Ok(pid))) if args.len() == 3 => walk(app, pid),
            _ => Err(Fail::Usage("atspi-walk <app> <pid>".into())),
        },
        Some("atspi-listen") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(app), Some(out), Some(stop)) if args.len() == 4 => {
                listen(app, Path::new(out), Path::new(stop))
            }
            _ => Err(Fail::Usage("atspi-listen <app> <out> <stop>".into())),
        },
        Some("seat") => match (
            args.get(1).map(|p| p.parse::<u32>()),
            args.get(2),
            args.get(3).map(|p| p.parse::<u32>()),
        ) {
            (Some(Ok(mutter)), Some(app), Some(Ok(pid))) => {
                parse_steps(args.get(4..).unwrap_or_default())
                    .map_err(Fail::Usage)
                    .and_then(|steps| seat(mutter, app, pid, &steps))
            }
            _ => Err(Fail::Usage(
                "seat <mutter-pid> <app> <app-pid> <step>...".into(),
            )),
        },
        Some("xdnd") => match (args.get(1).map(|w| parse_window(w)), args.get(2), args.get(3)) {
            (Some(Some(window)), Some(uri), Some(marks)) if args.len() == 4 => {
                crate::xdnd::run(window, uri, Path::new(marks)).map_err(|e| match e {
                    crate::xdnd::Error::NoServer(why) => Fail::NotHere(why),
                    crate::xdnd::Error::Fault(why) => Fail::Fault(why),
                })
            }
            _ => Err(Fail::Usage("xdnd <window> <uri> <marks-dir>".into())),
        },
        Some("xgrab") => match (args.get(1).map(|w| parse_window(w)), args.get(2)) {
            (Some(Some(window)), Some(out)) if args.len() == 3 => {
                crate::xgrab::run(window, Path::new(out)).map_err(|e| match e {
                    crate::xdnd::Error::NoServer(why) => Fail::NotHere(why),
                    crate::xdnd::Error::Fault(why) => Fail::Fault(why),
                })
            }
            _ => Err(Fail::Usage("xgrab <window> <out>".into())),
        },
        _ => Err(Fail::Usage(
            "lane-bus atspi-walk <app> <pid> | atspi-listen <app> <out> <stop> | seat <mutter-pid> <app> <app-pid> <step>... | xdnd <window> <uri> <marks-dir> | xgrab <window> <out>"
                .into(),
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::NotHere(why)) => {
            eprintln!("lane-bus: NOT AVAILABLE: {why}");
            ExitCode::from(3)
        }
        Err(Fail::Fault(why)) => {
            eprintln!("lane-bus: {why}");
            ExitCode::from(1)
        }
        Err(Fail::Usage(why)) => {
            eprintln!("lane-bus: usage: {why}");
            ExitCode::from(2)
        }
    }
}

/// Call `member` and return the reply's body.
fn call(
    conn: &mut Connection,
    dest: &str,
    path: &str,
    iface: &str,
    member: &str,
    body: Vec<Value>,
) -> Result<Vec<Value>, String> {
    let msg = Message::method_call(Some(dest), path, Some(iface), member)
        .map_err(|e| e.to_string())?
        .with_body(body);
    conn.call(msg, CALL)
        .map(|reply| reply.body)
        .map_err(|e| e.to_string())
}

/// A property, unwrapped from its variant.
fn property(
    conn: &mut Connection,
    dest: &str,
    path: &str,
    iface: &str,
    name: &str,
) -> Result<Value, String> {
    let body = call(
        conn,
        dest,
        path,
        PROPERTIES,
        "Get",
        vec![iface.into(), name.into()],
    )?;
    match body.into_iter().next() {
        Some(Value::Variant(v)) => Ok(*v),
        Some(other) => Ok(other),
        None => Err("an empty answer".into()),
    }
}

/// The unique name that owns `name` on this bus, if any.
fn owner(conn: &mut Connection, name: &str) -> Option<String> {
    call(
        conn,
        BUS,
        "/org/freedesktop/DBus",
        BUS,
        "GetNameOwner",
        vec![name.into()],
    )
    .ok()?
    .first()?
    .as_str()
    .map(str::to_owned)
}

/// The process behind a connection on this bus.
fn pid_of(conn: &mut Connection, name: &str) -> Option<u32> {
    call(
        conn,
        BUS,
        "/org/freedesktop/DBus",
        BUS,
        "GetConnectionUnixProcessID",
        vec![name.into()],
    )
    .ok()?
    .first()?
    .as_u32()
}

/// The unique names on this bus that process `pid` holds.
fn names_of(conn: &mut Connection, pid: u32) -> Vec<String> {
    let names: Vec<String> = call(
        conn,
        BUS,
        "/org/freedesktop/DBus",
        BUS,
        "ListNames",
        Vec::new(),
    )
    .ok()
    .and_then(|b| {
        b.first().and_then(Value::as_array).map(|a| {
            a.items()
                .iter()
                .filter_map(Value::as_str)
                .filter(|n| n.starts_with(':'))
                .map(str::to_owned)
                .collect()
        })
    })
    .unwrap_or_default();
    names
        .into_iter()
        .filter(|n| pid_of(conn, n) == Some(pid))
        .collect()
}

/// The accessibility bus: `AT_SPI_BUS_ADDRESS`, else at-spi2-core's answer
/// to `org.a11y.Bus.GetAddress` on the session bus.
fn a11y_bus() -> Result<Connection, Fail> {
    let addr = match std::env::var("AT_SPI_BUS_ADDRESS") {
        Ok(a) if !a.is_empty() => a,
        _ => {
            let mut session =
                Connection::session().map_err(|e| Fail::NotHere(format!("session bus: {e}")))?;
            let body = call(
                &mut session,
                "org.a11y.Bus",
                "/org/a11y/bus",
                "org.a11y.Bus",
                "GetAddress",
                Vec::new(),
            )
            .map_err(|e| Fail::NotHere(format!("org.a11y.Bus.GetAddress: {e}")))?;
            body.first()
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Fail::NotHere("the session bus named no accessibility bus".into()))?
        }
    };
    let addresses = address::parse(&addr)
        .map_err(|e| Fail::NotHere(format!("accessibility bus {addr}: {e}")))?;
    Connection::open(&addresses).map_err(|e| Fail::NotHere(format!("accessibility bus: {e}")))
}

/// An object reference `(so)`.
fn object_ref(v: &Value) -> Option<(String, String)> {
    let fields = v.as_struct()?;
    let bus = fields.first()?.as_str()?;
    let path = match fields.get(1)? {
        Value::Path(p) => p.as_str(),
        other => other.as_str()?,
    };
    Some((bus.to_owned(), path.to_owned()))
}

/// The children an `a(so)` reply names.
fn children_of(body: &[Value]) -> Vec<(String, String)> {
    body.first()
        .and_then(Value::as_array)
        .map(|a| a.items().iter().filter_map(object_ref).collect())
        .unwrap_or_default()
}

/// The applications on the bus whose root is named `app` and whose
/// process is `pid`.
fn find_app(conn: &mut Connection, app: &str, pid: u32) -> Result<Vec<(String, String)>, Fail> {
    let body = call(conn, REGISTRY, ROOT, ACCESSIBLE, "GetChildren", Vec::new())
        .map_err(|e| Fail::NotHere(format!("the registry's children: {e}")))?;
    let mut found = Vec::new();
    for (bus, path) in children_of(&body) {
        let name = property(conn, &bus, &path, ACCESSIBLE, "Name");
        if name.as_ref().ok().and_then(Value::as_str) == Some(app)
            && pid_of(conn, &bus) == Some(pid)
        {
            found.push((bus, path));
        }
    }
    Ok(found)
}

/// `atspi-walk <app> <pid>`.
fn walk(app: &str, pid: u32) -> Result<(), Fail> {
    let mut conn = a11y_bus()?;
    let apps = find_app(&mut conn, app, pid)?;
    let Some((bus, root)) = apps.first().cloned() else {
        return Err(Fail::NotHere(format!(
            "no application named {app} of process {pid} on the accessibility bus"
        )));
    };
    println!("apps {}", apps.len());
    let mut stack = vec![(bus, root, 0usize)];
    let mut nodes = 0usize;
    while let Some((bus, path, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            println!("cut at {MAX_NODES} nodes");
            break;
        }
        let (line, children) = describe(&mut conn, &bus, &path, depth);
        println!("{line}");
        if depth + 1 >= MAX_DEPTH {
            if !children.is_empty() {
                println!("cut at depth {MAX_DEPTH}");
            }
            continue;
        }
        for (b, p) in children.into_iter().rev() {
            stack.push((b, p, depth + 1));
        }
    }
    Ok(())
}

/// One node's line, and its children.
fn describe(
    conn: &mut Connection,
    bus: &str,
    path: &str,
    depth: usize,
) -> (String, Vec<(String, String)>) {
    let mut line = format!("node depth={depth} path={path}");
    let role = call(conn, bus, path, ACCESSIBLE, "GetRole", Vec::new())
        .ok()
        .and_then(|b| b.first().and_then(Value::as_u32));
    let _ = write!(
        line,
        " role={}",
        role.map_or_else(|| "ERR".into(), role_name)
    );
    for name in ["Name", "Description"] {
        let v = property(conn, bus, path, ACCESSIBLE, name);
        let _ = write!(
            line,
            " {}={}",
            name.to_lowercase(),
            field(v.as_ref().map(quote_value))
        );
    }
    let states = call(conn, bus, path, ACCESSIBLE, "GetState", Vec::new());
    let _ = write!(
        line,
        " states={}",
        field(states.as_deref().map(state_names))
    );
    let ifaces: Vec<String> = call(conn, bus, path, ACCESSIBLE, "GetInterfaces", Vec::new())
        .ok()
        .and_then(|b| {
            b.first().and_then(Value::as_array).map(|a| {
                a.items()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim_start_matches("org.a11y.atspi.").to_owned())
                    .collect()
            })
        })
        .unwrap_or_default();
    let _ = write!(line, " interfaces={}", ifaces.join(","));
    let attrs = call(conn, bus, path, ACCESSIBLE, "GetAttributes", Vec::new());
    let _ = write!(
        line,
        " attributes={}",
        field(attrs.as_deref().map(attributes))
    );
    if ifaces.iter().any(|i| i == "Text") {
        let text = "org.a11y.atspi.Text";
        let count = property(conn, bus, path, text, "CharacterCount");
        let caret = property(conn, bus, path, text, "CaretOffset");
        let all = call(
            conn,
            bus,
            path,
            text,
            "GetText",
            vec![Value::I32(0), Value::I32(-1)],
        );
        let _ = write!(
            line,
            " characters={} caret={} text={}",
            field(count.as_ref().map(render)),
            field(caret.as_ref().map(render)),
            field(
                all.as_ref()
                    .map(|b| b.first().map_or_else(|| "-".into(), quote_value))
            )
        );
    }
    if ifaces.iter().any(|i| i == "Component") {
        // Window-relative (1) and screen (0) extents.
        for (key, coord) in [("extents_window", 1u32), ("extents_screen", 0u32)] {
            let ext = call(
                conn,
                bus,
                path,
                "org.a11y.atspi.Component",
                "GetExtents",
                vec![Value::U32(coord)],
            );
            let _ = write!(line, " {key}={}", field(ext.as_deref().map(extents)));
        }
    }
    if ifaces.iter().any(|i| i == "Action") {
        let action = "org.a11y.atspi.Action";
        let n = property(conn, bus, path, action, "NActions")
            .ok()
            .and_then(|v| v.as_i32())
            .unwrap_or(0);
        let names: Vec<String> = (0..n.clamp(0, 64))
            .map(|i| {
                call(conn, bus, path, action, "GetName", vec![Value::I32(i)])
                    .ok()
                    .and_then(|b| b.first().and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| "ERR".into())
            })
            .collect();
        let _ = write!(line, " actions={}", names.join("|"));
    }
    let children = call(conn, bus, path, ACCESSIBLE, "GetChildren", Vec::new());
    let kids = children.as_deref().map(children_of).unwrap_or_default();
    let count = property(conn, bus, path, ACCESSIBLE, "ChildCount");
    let _ = write!(
        line,
        " children={} child_count={}",
        kids.len(),
        field(count.as_ref().map(render))
    );
    (line, kids)
}

/// A reading, or `ERR(<why>)` with no spaces, so a line stays one record.
fn field(r: Result<String, &String>) -> String {
    match r {
        Ok(s) => s,
        Err(e) => format!("ERR({})", e.replace(' ', "_")),
    }
}

fn role_name(role: u32) -> String {
    usize::try_from(role)
        .ok()
        .and_then(|i| ROLES.get(i))
        .map_or_else(|| format!("role{role}"), |n| (*n).to_owned())
}

/// `GetState`'s `au`: two words, bit `n` of the 64 being state `n`.
fn state_names(body: &[Value]) -> String {
    let words: Vec<u32> = body
        .first()
        .and_then(Value::as_array)
        .map(|a| a.items().iter().filter_map(Value::as_u32).collect())
        .unwrap_or_default();
    let mut names = Vec::new();
    for (w, word) in words.iter().enumerate() {
        for bit in 0..32usize {
            if word & (1 << bit) != 0 {
                let n = w * 32 + bit;
                names.push(
                    STATES
                        .get(n)
                        .map_or_else(|| format!("state{n}"), |s| (*s).to_owned()),
                );
            }
        }
    }
    if names.is_empty() {
        "-".into()
    } else {
        names.join(",")
    }
}

/// `GetAttributes`' `a{ss}`, sorted by key.
fn attributes(body: &[Value]) -> String {
    let mut pairs: Vec<String> = body
        .first()
        .and_then(Value::as_dict)
        .map(|d| {
            d.entries()
                .iter()
                .map(|(k, v)| format!("{}:{}", render(k), render(v)))
                .collect()
        })
        .unwrap_or_default();
    pairs.sort();
    if pairs.is_empty() {
        "-".into()
    } else {
        pairs.join(",")
    }
}

/// `GetExtents`' `(iiii)` as `x,y,w,h`.
fn extents(body: &[Value]) -> String {
    body.first()
        .and_then(Value::as_struct)
        .map(|f| f.iter().map(render).collect::<Vec<_>>().join(","))
        .unwrap_or_else(|| "-".into())
}

/// A string, quoted with its control characters escaped, so it fits on one
/// line and reads back unambiguously.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn quote_value(v: &Value) -> String {
    match v.as_str() {
        Some(s) => quote(s),
        None => render(v),
    }
}

/// Any value, on one line: strings quoted, an object reference as
/// `ref(<path>)`, containers bracketed.
fn render(v: &Value) -> String {
    if let Some((_, path)) = object_ref(v) {
        return format!("ref({path})");
    }
    match v {
        Value::Byte(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::I16(n) => n.to_string(),
        Value::U16(n) => n.to_string(),
        Value::I32(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F64(n) => n.to_string(),
        Value::Str(s) => quote(s),
        Value::Path(p) => p.as_str().to_owned(),
        Value::Sig(s) => s.as_str().to_owned(),
        Value::Variant(inner) => render(inner),
        Value::Array(a) => format!(
            "[{}]",
            a.items().iter().map(render).collect::<Vec<_>>().join(",")
        ),
        Value::Dict(d) => format!(
            "{{{}}}",
            d.entries()
                .iter()
                .map(|(k, v)| format!("{}:{}", render(k), render(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Struct(fields) => format!(
            "({})",
            fields.iter().map(render).collect::<Vec<_>>().join(",")
        ),
    }
}

/// One event's line: `<ms> <pid> <Interface>.<Member> <path> <body>`. An AT-SPI
/// event's body is `(siiva{sv})`: the detail, two numbers, any data and
/// properties, which print only when there are any.
fn event_line(ms: u128, pid: u32, msg: &Message) -> String {
    let iface = msg
        .interface
        .as_deref()
        .unwrap_or("-")
        .trim_start_matches("org.a11y.atspi.Event.");
    let member = msg.member.as_deref().unwrap_or("-");
    let path = msg.path.as_ref().map_or("-", |p| p.as_str());
    let mut line = format!("{ms} {pid} {iface}.{member} {path}");
    let b = &msg.body;
    if let [detail, d1, d2, any, rest @ ..] = b.as_slice() {
        let _ = write!(
            line,
            " detail={} d1={} d2={} any={}",
            quote_value(detail),
            render(d1),
            render(d2),
            render(any)
        );
        for extra in rest {
            let shown = render(extra);
            if shown != "{}" {
                let _ = write!(line, " props={shown}");
            }
        }
    } else {
        for v in b {
            let _ = write!(line, " {}", render(v));
        }
    }
    line
}

/// `atspi-listen <app> <out> <stop>`.
fn listen(app: &str, out: &Path, stop: &Path) -> Result<(), Fail> {
    let mut conn = a11y_bus()?;
    // Every signal: AT-SPI's events are signals without a destination.
    conn.add_match("type='signal'", CALL).map_err(Fail::fault)?;
    let registry = owner(&mut conn, REGISTRY).unwrap_or_default();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .map_err(|e| Fail::Fault(format!("{}: {e}", out.display())))?;
    let started = Instant::now();
    // sender → its process, when it is an application named `app`.
    let mut names: Vec<(String, Option<u32>)> = Vec::new();
    while !stop.exists() {
        if started.elapsed() > LISTEN_LIMIT {
            return Err(Fail::Fault(format!(
                "no stop file after {} s (a hang detector)",
                LISTEN_LIMIT.as_secs()
            )));
        }
        let msg = match conn.recv(Some(Duration::from_millis(200))) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            // A malformed message is consumed and named; the connection stays.
            Err(e @ (aterm_dbus::Error::Malformed { .. } | aterm_dbus::Error::Unsupported(_))) => {
                let _ = writeln!(file, "{} ERR {}", started.elapsed().as_millis(), e);
                continue;
            }
            Err(e) => return Err(Fail::fault(e)),
        };
        let Some(sender) = msg.sender.clone() else {
            continue;
        };
        let is_event = msg
            .interface
            .as_deref()
            .is_some_and(|i| i.starts_with("org.a11y.atspi.Event."));
        if !is_event || sender == registry || sender == BUS {
            continue;
        }
        let ours = match names.iter().find(|(s, _)| *s == sender) {
            Some((_, ours)) => *ours,
            None => {
                let name = property(&mut conn, &sender, ROOT, ACCESSIBLE, "Name");
                let ours = (name.ok().as_ref().and_then(Value::as_str) == Some(app))
                    .then(|| pid_of(&mut conn, &sender))
                    .flatten();
                names.push((sender.clone(), ours));
                ours
            }
        };
        if let Some(pid) = ours {
            let line = event_line(started.elapsed().as_millis(), pid, &msg);
            writeln!(file, "{line}")
                .and_then(|()| file.flush())
                .map_err(|e| Fail::Fault(format!("{}: {e}", out.display())))?;
        }
    }
    Ok(())
}

/// An X window id as xdotool prints it (decimal), or `0x…`.
fn parse_window(word: &str) -> Option<u32> {
    match word.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => word.parse().ok(),
    }
}

/// `seat`'s `app` for a client that publishes no AT-SPI frame.
const NO_APP: &str = "-";

/// One step of `seat`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Key {
        keysym: u32,
        pressed: bool,
    },
    Button {
        code: i32,
        pressed: bool,
    },
    /// Move the pointer by this many logical pixels.
    Motion {
        dx: i32,
        dy: i32,
    },
    /// Move the pointer by the `DX DY` this file holds when the step runs.
    MotionFrom(String),
    /// Hold for this many milliseconds: a stimulus (a key held down), never a
    /// wait for something to happen.
    Sleep(u64),
    /// Create this file: tells the lab a point was reached.
    Touch(String),
    /// Wait until this file exists: the lab says go on (a hang detector of
    /// [`LISTEN_LIMIT`]).
    Wait(String),
}

impl Step {
    /// Whether the step drives the pointer (Mutter makes the seat's pointer
    /// with the session's first such event).
    fn drives_pointer(&self) -> bool {
        matches!(
            self,
            Step::Button { .. } | Step::Motion { .. } | Step::MotionFrom(_)
        )
    }
}

/// The motion that makes a session's pointer before its keyboard: up and left
/// past any lane screen, so the seat parks it in the top-left corner, where
/// no lane window lies. Every pointer session starts there, so its steps aim
/// from the corner, never from where an earlier session left the pointer.
const PARK: Step = Step::Motion {
    dx: -4000,
    dy: -4000,
};

/// The keysyms `seat` names, beside `0x…`.
const KEYSYMS: [(&str, u32); 12] = [
    ("Shift_L", 0xffe1),
    ("Control_L", 0xffe3),
    ("Alt_L", 0xffe9),
    ("Return", 0xff0d),
    ("Escape", 0xff1b),
    ("BackSpace", 0xff08),
    ("Tab", 0xff09),
    ("space", 0x20),
    ("shift", 0xffe1),
    ("ctrl", 0xffe3),
    ("alt", 0xffe9),
    ("enter", 0xff0d),
];

fn keysym(word: &str) -> Option<u32> {
    if let Some(hex) = word.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some((_, k)) = KEYSYMS.iter().find(|(n, _)| *n == word) {
        return Some(*k);
    }
    // A single printable ASCII character is its own keysym (Latin-1).
    let mut chars = word.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Some(u32::from(c)),
        _ => None,
    }
}

/// The evdev button codes: BTN_LEFT, BTN_RIGHT, BTN_MIDDLE.
fn button(word: &str) -> Option<i32> {
    match word {
        "left" => Some(0x110),
        "right" => Some(0x111),
        "middle" => Some(0x112),
        n => n.parse().ok(),
    }
}

/// A pointer motion `DX,DY` (or `DX DY`, as a `move:@PATH` file holds it), in
/// whole logical pixels, each within the reach of any screen.
fn motion(text: &str) -> Option<(i32, i32)> {
    let mut it = text.trim().split([',', ' ']).filter(|w| !w.is_empty());
    let (dx, dy) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let reach = |d: i32| d.unsigned_abs() <= 100_000;
    (it.next().is_none() && reach(dx) && reach(dy)).then_some((dx, dy))
}

/// Parse `seat`'s steps.
fn parse_steps(words: &[String]) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for w in words {
        let (verb, arg) = w
            .split_once(':')
            .ok_or_else(|| format!("seat step '{w}': VERB:ARG"))?;
        let key = |k: &str| keysym(k).ok_or_else(|| format!("seat step '{w}': no keysym '{k}'"));
        match verb {
            "tap" => {
                let k = key(arg)?;
                steps.push(Step::Key {
                    keysym: k,
                    pressed: true,
                });
                steps.push(Step::Key {
                    keysym: k,
                    pressed: false,
                });
            }
            "down" | "up" => steps.push(Step::Key {
                keysym: key(arg)?,
                pressed: verb == "down",
            }),
            "chord" => {
                let keys = arg.split('+').map(key).collect::<Result<Vec<_>, _>>()?;
                steps.extend(keys.iter().map(|&k| Step::Key {
                    keysym: k,
                    pressed: true,
                }));
                steps.extend(keys.iter().rev().map(|&k| Step::Key {
                    keysym: k,
                    pressed: false,
                }));
            }
            "button" => {
                let code =
                    button(arg).ok_or_else(|| format!("seat step '{w}': no button '{arg}'"))?;
                steps.push(Step::Button {
                    code,
                    pressed: true,
                });
                steps.push(Step::Button {
                    code,
                    pressed: false,
                });
            }
            "press" | "release" => steps.push(Step::Button {
                code: button(arg).ok_or_else(|| format!("seat step '{w}': no button '{arg}'"))?,
                pressed: verb == "press",
            }),
            "move" => match arg.strip_prefix('@') {
                Some(path) if path.starts_with('/') => {
                    steps.push(Step::MotionFrom(path.to_string()));
                }
                Some(_) => return Err(format!("seat step '{w}': an absolute path after '@'")),
                None => {
                    let (dx, dy) = motion(arg)
                        .ok_or_else(|| format!("seat step '{w}': move:DX,DY in pixels"))?;
                    steps.push(Step::Motion { dx, dy });
                }
            },
            "sleep" => steps.push(Step::Sleep(
                arg.parse()
                    .map_err(|_| format!("seat step '{w}': milliseconds"))?,
            )),
            "touch" | "wait" if arg.starts_with('/') => steps.push(if verb == "touch" {
                Step::Touch(arg.to_string())
            } else {
                Step::Wait(arg.to_string())
            }),
            "touch" | "wait" => return Err(format!("seat step '{w}': an absolute path")),
            _ => {
                return Err(format!(
                    "seat step '{w}': tap, down, up, chord, button, press, release, move, sleep, \
                     touch or wait"
                ));
            }
        }
    }
    if steps.is_empty() {
        return Err("seat <step>...".into());
    }
    Ok(steps)
}

/// Whether the frame that application `app` of process `pid` publishes is
/// `active` (its window holds the keyboard focus); `None` while there is no
/// such application or frame, or a reading fails.
fn frame_active(conn: &mut Connection, app: &str, pid: u32) -> Option<bool> {
    let (bus, root) = find_app(conn, app, pid).ok()?.into_iter().next()?;
    let kids = call(conn, &bus, &root, ACCESSIBLE, "GetChildren", Vec::new()).ok()?;
    for (b, p) in children_of(&kids) {
        let role = call(conn, &b, &p, ACCESSIBLE, "GetRole", Vec::new()).ok()?;
        if role
            .first()
            .and_then(Value::as_u32)
            .map(role_name)
            .as_deref()
            != Some("frame")
        {
            continue;
        }
        let states = call(conn, &b, &p, ACCESSIBLE, "GetState", Vec::new()).ok()?;
        let low = states
            .first()
            .and_then(Value::as_array)
            .and_then(|a| a.items().first().and_then(Value::as_u32))?;
        return Some(low & (1 << ACTIVE_BIT) != 0);
    }
    None
}

/// Wait until `frame_active` reads `want`, within the hang detector.
fn await_focus(conn: &mut Connection, app: &str, pid: u32, want: bool) -> Result<(), Fail> {
    let t0 = Instant::now();
    loop {
        if frame_active(conn, app, pid) == Some(want) {
            return Ok(());
        }
        if t0.elapsed() > HANG {
            return Err(Fail::Fault(format!(
                "the frame of {app} (process {pid}) never became {} ({} s)",
                if want { "active" } else { "inactive" },
                HANG.as_secs()
            )));
        }
        std::thread::sleep(POLL);
    }
}

/// Wait until `path` exists, within `limit` (a hang detector).
fn await_file(path: &str, limit: Duration) -> Result<(), Fail> {
    let t0 = Instant::now();
    while !Path::new(path).exists() {
        if t0.elapsed() > limit {
            return Err(Fail::Fault(format!(
                "{path} never appeared ({} s)",
                limit.as_secs()
            )));
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// `seat <mutter-pid> <app> <app-pid> <step>...`: a RemoteDesktop session of
/// the Mutter that is process `mutter-pid`, for the steps' length; Mutter
/// ends it, and removes its devices, when it stops. A Shift tap goes first,
/// and the steps start once the application's frame is `active`: its window
/// has the new keyboard's focus, so no key is lost while the client binds the
/// keyboard (with no key at all, Mutter never gave the window focus within a
/// minute: measured 2026-10-16). The frame must be
/// inactive first, so a focus the last session's keyboard left behind (its
/// leave not handled yet) is never taken for this one's. A step that must
/// see its keys land ends with a `touch` the lab waits for and a `wait` that
/// keeps the devices until the lab has seen them.
///
/// When the steps drive the pointer, the session makes it first ([`PARK`]),
/// before the Shift tap, so the awaited focus proves the seat has the pointer
/// too. Mutter 46 runs an input event's filter, which moves the Wayland
/// seat's pointer focus, as the event comes, but hands a new device to the
/// Wayland seat only at the stage's next update (its `DEVICE_ADDED` waits in
/// the stage's queue). A motion in between moves a pointer the seat does not
/// have yet: no surface takes its focus, and the window under it hears no
/// `enter` until a later motion repicks (measured 2026-10-17: the window bound
/// the pointer 0.2 ms after Mutter announced it, but Mutter had already pinged
/// that window for the motion onto it, and never sent it an `enter`). The two
/// devices' `DEVICE_ADDED` keep the stage's queue in the order they were
/// made, so the pointer is announced before the keyboard whose focus the
/// steps wait for.
///
/// An `app` of `-` names a client that publishes no AT-SPI frame (aterm-wayland's
/// live seat test): no focus is awaited, and the client paces the steps itself
/// with `wait` steps on files it creates once its keyboard has the focus.
fn seat(mutter: u32, app: &str, pid: u32, steps: &[Step]) -> Result<(), Fail> {
    let mut a11y = if app == NO_APP {
        None
    } else {
        Some(a11y_bus()?)
    };
    if let Some(a11y) = a11y.as_mut() {
        await_focus(a11y, app, pid, false)?;
    }
    let mut conn = Connection::session().map_err(|e| Fail::NotHere(format!("session bus: {e}")))?;
    // The Mutter that is `mutter`: the one of its unique names that answers
    // for RemoteDesktop.
    let mut rd = None;
    for name in names_of(&mut conn, mutter) {
        if call(
            &mut conn,
            &name,
            RD_PATH,
            PROPERTIES,
            "GetAll",
            vec![RD.into()],
        )
        .is_ok()
        {
            rd = Some(name);
            break;
        }
    }
    let Some(rd) = rd else {
        return Err(Fail::NotHere(format!(
            "process {mutter} serves no {RD} on the session bus (not a Mutter)"
        )));
    };
    let rd = rd.as_str();
    let body =
        call(&mut conn, rd, RD_PATH, RD, "CreateSession", Vec::new()).map_err(Fail::Fault)?;
    let session = match body.first() {
        Some(Value::Path(p)) => p.as_str().to_owned(),
        other => {
            return Err(Fail::Fault(format!(
                "CreateSession answered {}",
                other.map_or_else(|| "nothing".into(), render)
            )));
        }
    };
    call(&mut conn, rd, &session, RD_SESSION, "Start", Vec::new()).map_err(Fail::Fault)?;
    if a11y.is_some() && steps.iter().any(Step::drives_pointer) {
        run_steps(&mut conn, rd, &session, &[PARK])?;
    }
    // Mutter gives the seat its keyboard with the session's first key event,
    // which no client hears (it binds the keyboard only then), and the
    // window takes the keyboard's focus once it has: a harmless Shift tap.
    let prime = [
        Step::Key {
            keysym: 0xffe1,
            pressed: true,
        },
        Step::Key {
            keysym: 0xffe1,
            pressed: false,
        },
    ];
    run_steps(&mut conn, rd, &session, &prime)?;
    let focused = match a11y.as_mut() {
        Some(a11y) => await_focus(a11y, app, pid, true),
        None => Ok(()),
    };
    let done = focused.and_then(|()| run_steps(&mut conn, rd, &session, steps));
    let _ = call(&mut conn, rd, &session, RD_SESSION, "Stop", Vec::new());
    done?;
    // The session ends once the window has lost the keyboard's focus: what
    // the lab does next lands on a window without one, every time.
    if let Some(a11y) = a11y.as_mut() {
        await_focus(a11y, app, pid, false)?;
    }
    println!("ok seat {} steps", steps.len());
    Ok(())
}

/// The seat's pointer moved by `(dx, dy)` logical pixels.
fn relative_motion(
    conn: &mut Connection,
    rd: &str,
    session: &str,
    dx: i32,
    dy: i32,
) -> Result<(), String> {
    call(
        conn,
        rd,
        session,
        RD_SESSION,
        "NotifyPointerMotionRelative",
        vec![Value::F64(f64::from(dx)), Value::F64(f64::from(dy))],
    )
    .map(drop)
}

/// The steps of one `seat` session, in order.
fn run_steps(conn: &mut Connection, rd: &str, session: &str, steps: &[Step]) -> Result<(), Fail> {
    for step in steps {
        let done = match step {
            Step::Touch(path) => std::fs::File::create(path)
                .map(drop)
                .map_err(|e| e.to_string()),
            Step::Wait(path) => {
                await_file(path, LISTEN_LIMIT)?;
                Ok(())
            }
            Step::Key { keysym, pressed } => call(
                conn,
                rd,
                session,
                RD_SESSION,
                "NotifyKeyboardKeysym",
                vec![Value::U32(*keysym), Value::Bool(*pressed)],
            )
            .map(drop),
            Step::Button { code, pressed } => call(
                conn,
                rd,
                session,
                RD_SESSION,
                "NotifyPointerButton",
                vec![Value::I32(*code), Value::Bool(*pressed)],
            )
            .map(drop),
            Step::Motion { dx, dy } => relative_motion(conn, rd, session, *dx, *dy),
            Step::MotionFrom(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|text| {
                    motion(&text).ok_or_else(|| format!("{path}: not `DX DY` ({text:?})"))
                })
                .and_then(|(dx, dy)| relative_motion(conn, rd, session, dx, dy)),
            Step::Sleep(ms) => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(())
            }
        };
        done.map_err(|e| Fail::Fault(format!("{step:?}: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        // A drag: the button's halves apart, the pointer moved between them.
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
    }
}

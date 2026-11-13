// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Summarizes X11 requests, atoms, selections, client messages, and errors from an `xtrace` log.

use std::collections::{BTreeMap, BTreeSet};

/// Display `xtrace` exposes to the traced client.
pub const PROXY: &str = ":93";

/// What the traced aterm-gui is typed, and the echo it must show.
pub const TYPED: &str = "echo X11-$((6*7))";
pub const ECHO: &str = "X11-42";

/// The unit the proxy runs as.
#[must_use]
pub fn unit_name() -> String {
    format!("{}xtrace", crate::linux_guest::UNIT_PREFIX)
}

/// Builds the `xtrace` argv relaying [`PROXY`] to `display` and logging to `log`.
#[must_use]
pub fn proxy_argv(display: &str, log: &str) -> Vec<String> {
    ["xtrace", "-d", display, "-D", PROXY, "-n", "-k", "-o", log]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// XI2's event types by bit (`XI2.h`, XI 2.4).
const XI2_EVENTS: [&str; 33] = [
    "0",
    "DeviceChanged",
    "KeyPress",
    "KeyRelease",
    "ButtonPress",
    "ButtonRelease",
    "Motion",
    "Enter",
    "Leave",
    "FocusIn",
    "FocusOut",
    "HierarchyChanged",
    "PropertyEvent",
    "RawKeyPress",
    "RawKeyRelease",
    "RawButtonPress",
    "RawButtonRelease",
    "RawMotion",
    "TouchBegin",
    "TouchUpdate",
    "TouchEnd",
    "TouchOwnership",
    "RawTouchBegin",
    "RawTouchUpdate",
    "RawTouchEnd",
    "BarrierHit",
    "BarrierLeave",
    "GesturePinchBegin",
    "GesturePinchUpdate",
    "GesturePinchEnd",
    "GestureSwipeBegin",
    "GestureSwipeUpdate",
    "GestureSwipeEnd",
];

struct Request<'a> {
    seq: String,
    ext: &'a str,
    name: &'a str,
    args: &'a str,
}

/// `CCC:<:SSSS: LEN: BODY`, a request the client sent; `None` for any other
/// line (replies, events, errors, the connection's setup).
fn request(line: &str) -> Option<Request<'_>> {
    let mut parts = line.splitn(4, ':');
    let conn = parts.next()?;
    if parts.next()? != "<" {
        return None;
    }
    let seq = parts.next()?;
    if !seq.chars().all(|c| c.is_ascii_hexdigit()) || seq.is_empty() {
        return None;
    }
    let rest = parts.next()?;
    let (_, body) = rest.split_once(": ")?;
    let at = body.find("Request(")?;
    let ext = body[..at].strip_suffix('-').unwrap_or("");
    let after = &body[at..];
    let (_, named) = after.split_once("): ")?;
    let (name, args) = named.split_once(' ').unwrap_or((named, ""));
    Some(Request {
        seq: format!("{conn}:{seq}"),
        ext,
        name,
        args,
    })
}

fn kind(r: &Request<'_>) -> String {
    if r.ext.is_empty() {
        r.name.to_string()
    } else {
        format!("{}:{}", r.ext, r.name)
    }
}

fn value<'a>(args: &'a str, key: &str, end: &[char]) -> Option<&'a str> {
    let at = args.find(&format!("{key}="))? + key.len() + 1;
    let rest = &args[at..];
    let stop = rest.find(|c: char| end.contains(&c)).unwrap_or(rest.len());
    Some(&rest[..stop])
}

fn hex(s: &str) -> Option<u128> {
    let h = s.strip_prefix("0x").unwrap_or(s);
    h.bytes()
        .all(|b| b.is_ascii_hexdigit())
        .then(|| u128::from_str_radix(h, 16).ok())
        .flatten()
}

fn role(root: &str, own: &BTreeSet<String>, id: &str) -> String {
    if id == root {
        "root".into()
    } else if own.contains(id) {
        "window".into()
    } else if hex(id) == Some(0) {
        "none".into()
    } else {
        "other".into()
    }
}

fn xi2_events(mask: u128) -> String {
    let names: Vec<String> = (0..128)
        .filter(|bit| mask & (1u128 << bit) != 0)
        .map(|bit| {
            XI2_EVENTS
                .get(bit)
                .map_or_else(|| format!("bit{bit}"), |n| (*n).to_string())
        })
        .collect();
    if names.is_empty() {
        "nothing".into()
    } else {
        names.join(",")
    }
}

/// The `x11.wire.*` lines (key, value) of one `xtrace` log, in that order.
#[must_use]
pub fn summary(log: &str) -> Vec<(&'static str, String)> {
    let mut root = String::new();
    let mut own: BTreeSet<String> = BTreeSet::new();
    let mut atom_names: BTreeMap<u128, String> = BTreeMap::new();
    let mut requests: BTreeSet<String> = BTreeSet::new();
    let mut interned: BTreeSet<String> = BTreeSet::new();
    let mut selects: BTreeSet<String> = BTreeSet::new();
    let mut sent: Vec<(String, String, Vec<u32>)> = Vec::new();
    let mut by_seq: BTreeMap<String, String> = BTreeMap::new();
    let mut errors: BTreeMap<String, u32> = BTreeMap::new();
    for line in log.lines() {
        if let Some(at) = line.find("roots={root=") {
            let rest = &line[at + "roots={root=".len()..];
            root = rest.split([' ', '}']).next().unwrap_or("").to_string();
            continue;
        }
        if let Some(at) = line.find("Reply to InternAtom: atom=") {
            let rest = &line[at + "Reply to InternAtom: atom=".len()..];
            if let Some((n, name)) = rest.split_once("(\"")
                && let Some(n) = hex(n)
            {
                atom_names.insert(n, name.strip_suffix("\")").unwrap_or(name).to_string());
            }
            continue;
        }
        if let Some(at) = line.find(":Error ") {
            let what = line[at + ":Error ".len()..]
                .split(':')
                .next()
                .unwrap_or("")
                .split_once('=')
                .map_or("?", |(_, n)| n)
                .to_string();
            let conn = line.split(':').next().unwrap_or("");
            let seq = value(line, "seq", &[' ', ',', ';']).unwrap_or("");
            let on = by_seq
                .get(&format!("{conn}:{seq}"))
                .cloned()
                .unwrap_or_else(|| "?".into());
            *errors.entry(format!("{what} on {on}")).or_default() += 1;
            continue;
        }
        let Some(r) = request(line) else {
            continue;
        };
        let k = kind(&r);
        by_seq.insert(r.seq.clone(), k.clone());
        requests.insert(k.clone());
        match k.as_str() {
            "InternAtom" => {
                if let Some(name) = value(r.args, "name", &[]) {
                    let name = name.trim_matches('\'');
                    let only = r.args.contains("only-if-exists=true");
                    interned.insert(if only {
                        format!("{name}?")
                    } else {
                        name.to_string()
                    });
                }
            }
            "CreateWindow" => {
                if let Some(id) = value(r.args, "window", &[' ']) {
                    own.insert(id.to_string());
                    if let Some(events) = value(r.args, "event-mask", &[' ', '}']) {
                        selects.insert(format!("{} events {events}", role(&root, &own, id)));
                    }
                }
            }
            "ChangeWindowAttributes" => {
                if let (Some(id), Some(events)) = (
                    value(r.args, "window", &[' ']),
                    value(r.args, "event-mask", &[' ', '}']),
                ) {
                    selects.insert(format!("{} events {events}", role(&root, &own, id)));
                }
            }
            "XInputExtension:XISelectEvents" => {
                let id = value(r.args, "win", &[' ']).unwrap_or("?");
                let mut rest = r.args;
                while let Some(at) = rest.find("device=") {
                    rest = &rest[at..];
                    let device = value(rest, "device", &[' ']).unwrap_or("?");
                    let mask = value(rest, "mask", &[';', ' ', '}']).and_then(hex);
                    let device = match device {
                        "0" => "all".to_string(),
                        "1" => "all-master".to_string(),
                        d => format!("device {d}"),
                    };
                    let events = mask.map_or_else(|| "?".to_string(), xi2_events);
                    selects.insert(format!("{} XI2 {device} {events}", role(&root, &own, id)));
                    rest = &rest["device=".len()..];
                }
            }
            "RANDR:SelectInput" => {
                let id = value(r.args, "window", &[' ']).unwrap_or("?");
                let enable = value(r.args, "enable", &[' ']).unwrap_or("?");
                selects.insert(format!("{} RANDR {enable}", role(&root, &own, id)));
            }
            "XKEYBOARD:SelectEvents" => {
                let data = value(r.args, "unparsed-data", &[';', ' ']).unwrap_or("?");
                let bytes: String = data
                    .split(',')
                    .map(|b| b.strip_prefix("0x").unwrap_or(b))
                    .collect();
                selects.insert(format!("XKB {bytes}"));
            }
            "SendEvent" if r.args.contains("ClientMessage(") => {
                let target = value(r.args, "window", &[' '])
                    .map_or_else(|| "?".into(), |id| role(&root, &own, id));
                let ty = r
                    .args
                    .split_once("type=")
                    .and_then(|(_, t)| t.split_once("(\""))
                    .and_then(|(_, t)| t.split_once("\")"))
                    .map_or("?", |(t, _)| t)
                    .to_string();
                let bytes: Vec<u8> = value(r.args, "data", &[';', ' '])
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|b| hex(b).and_then(|b| u8::try_from(b).ok()))
                    .collect();
                let words: Vec<u32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|w| u32::from_le_bytes(*w))
                    .collect();
                sent.push((target, ty, words));
            }
            _ => {}
        }
    }
    let name_of = |atom: u32| -> String {
        atom_names
            .get(&u128::from(atom))
            .cloned()
            .unwrap_or_else(|| format!("{atom:#x}"))
    };
    let messages: Vec<String> = sent
        .iter()
        .map(|(target, ty, words)| {
            if ty == "_NET_WM_STATE" && words.len() >= 3 {
                let action = match words[0] {
                    0 => "remove".to_string(),
                    1 => "add".to_string(),
                    2 => "toggle".to_string(),
                    n => format!("action {n}"),
                };
                let atoms: Vec<String> = words[1..3]
                    .iter()
                    .filter(|&&a| a != 0)
                    .map(|&a| name_of(a))
                    .collect();
                format!("{target} {ty} {action} {}", atoms.join(","))
            } else {
                format!("{target} {ty}")
            }
        })
        .collect();
    let or_none = |v: Vec<String>, sep: &str| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(sep)
        }
    };
    vec![
        (
            "x11.wire.requests",
            or_none(requests.into_iter().collect(), " "),
        ),
        (
            "x11.wire.atoms",
            format!(
                "{}: {}",
                interned.len(),
                or_none(interned.into_iter().collect(), " ")
            ),
        ),
        (
            "x11.wire.selects",
            or_none(selects.into_iter().collect(), "; "),
        ),
        ("x11.wire.messages", or_none(messages, "; ")),
        (
            "x11.wire.errors",
            or_none(
                errors
                    .into_iter()
                    .map(|(e, n)| if n == 1 { e } else { format!("{e} x{n}") })
                    .collect(),
                "; ",
            ),
        ),
    ]
}

#[cfg(test)]
#[path = "linux_xwire_tests.rs"]
mod tests;

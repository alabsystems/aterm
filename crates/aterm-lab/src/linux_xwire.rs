// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What an X11 client says on the wire, read from an `xtrace` log: the
//! baseline's `x11.wire.*` lines (X11-TRACE, `docs/LINUX-RUNTIME-LANE.md`,
//! "The X11 wire").
//!
//! The lane held X11 behaviour, not X11 requests: WC8-CARVE's review found a
//! commit (104dad2238) that had dropped the two `_NET_WM_STATE` REMOVE
//! messages every X11 window sends after it is made, which no screen's
//! pixels or replies showed. The Wayland screens already keep their wire's
//! words (`header.requests`, `touch.wire`, read from `WAYLAND_DEBUG`); this
//! is X11's: a second aterm-gui started through `xtrace` (a proxy display that
//! logs every request, reply, event and error), driven through a start, a
//! typed echo, a resize and the person's quit, and its log summed up into
//! lines that hold no ids, timing or counts, only what was asked:
//!
//! - `x11.wire.requests`: every request kind, once, sorted (`ChangeProperty`,
//!   `XInputExtension:XISelectEvents`);
//! - `x11.wire.atoms`: the atoms interned, by name, sorted, and how many;
//! - `x11.wire.selects`: the events selected, on which window (`root`, the
//!   client's own `window`, `none` for window 0), core and XI2 by name;
//! - `x11.wire.messages`: the client messages sent (`SendEvent`), in order,
//!   with `_NET_WM_STATE`'s action and atoms by name;
//! - `x11.wire.errors`: the X errors the server answered, by the request
//!   they answered.
//!
//! Traced at WC8-CARVE's base (6e0ceb69c3) and at main (1be2d78bac), these
//! lines differ exactly where the carve meant them to: the root's raw XI2
//! events (a06dcfbb0a) and 14 atoms (db9da71119, 6a6777e2b3)
//! (`docs/measured/linux-x11-gone-2026-10-21.md`).

use std::collections::{BTreeMap, BTreeSet};

/// The display `xtrace` offers the traced aterm-gui, past the lane's own
/// (Xvfb's `:91`, Xwayland's `:92`).
pub const PROXY: &str = ":93";

/// What the traced aterm-gui is typed, and the echo it must show.
pub const TYPED: &str = "echo X11-$((6*7))";
pub const ECHO: &str = "X11-42";

/// The unit the proxy runs as.
#[must_use]
pub fn unit_name() -> String {
    format!("{}xtrace", crate::linux_guest::UNIT_PREFIX)
}

/// `xtrace`'s argv: [`PROXY`] relayed to `display` (no authority copied: the
/// lane's servers take none), every line to `log` as it comes, and kept up
/// after its client goes, until its unit is stopped.
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

/// One request line: its connection, sequence number, extension (empty for
/// the core protocol), name and the rest of the line.
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

/// The request's name, with its extension when it has one
/// (`XInputExtension:XISelectEvents`).
fn kind(r: &Request<'_>) -> String {
    if r.ext.is_empty() {
        r.name.to_string()
    } else {
        format!("{}:{}", r.ext, r.name)
    }
}

/// The value of `key=` in `args`, up to the next space, `,` excluded only
/// where `end` says.
fn value<'a>(args: &'a str, key: &str, end: &[char]) -> Option<&'a str> {
    let at = args.find(&format!("{key}="))? + key.len() + 1;
    let rest = &args[at..];
    let stop = rest.find(|c: char| end.contains(&c)).unwrap_or(rest.len());
    Some(&rest[..stop])
}

/// A hex number as `0x…` prints it.
fn hex(s: &str) -> Option<u128> {
    u128::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// What `id` is to the client: the screen's `root`, a `window` it made,
/// `none` (window 0) or an `other`'s.
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

/// XI2's event mask, by name.
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
            // `atom=0xf2("_NET_WM_STATE_ABOVE")`
            let rest = &line[at + "Reply to InternAtom: atom=".len()..];
            if let Some((n, name)) = rest.split_once("(\"")
                && let Some(n) = hex(n)
            {
                atom_names.insert(n, name.trim_end_matches("\")").to_string());
            }
            continue;
        }
        if let Some(at) = line.find(":Error ") {
            // `001:>:000f:Error 3=Window: major=2, …, seq=000f`
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
                    .map(|b| b.trim_start_matches("0x"))
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
mod tests {
    use super::*;

    /// Lines of the log aterm-gui's trace at main gave (2026-10-21), cut to
    /// what each summary line reads.
    const LOG: &str = "\
001:<: am lsb-first want 11:0 authorising with '' of length 0
001:>: Success, version is 11:0 vendor='The X.Org Foundation' roots={root=0x0000021f default-colormap=0x00000020 white-pixel=0x00ffffff}
001:<:0001: 20: Request(98): QueryExtension name='BIG-REQUESTS'
001:<:0002:  4: BIG-REQUESTS-Request(133,0): Enable
001:<:000d: 24: Request(16): InternAtom only-if-exists=false(0x00) name='_XSETTINGS_S0'
001:<:000f: 16: Request(2): ChangeWindowAttributes window=0x00000000 value-list={event-mask=PropertyChange}
001:>:000f:Error 3=Window: major=2, minor=0, bad=0x00000000, seq=000f
001:<:001e: 24: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE'
001:>:001e:32: Reply to InternAtom: atom=0xf1(\"_NET_WM_STATE\")
001:<:001f: 28: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE_ABOVE'
001:>:001f:32: Reply to InternAtom: atom=0xf2(\"_NET_WM_STATE_ABOVE\")
001:<:0020: 28: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE_BELOW'
001:>:0020:32: Reply to InternAtom: atom=0xf3(\"_NET_WM_STATE_BELOW\")
001:<:003f: 12: RANDR-Request(140,4): SelectInput window=0x0000021f enable=ScreenChangeNotifyMask,OutputPropertyNotifyMask
001:<:00a6: 20: XInputExtension-Request(131,46): XISelectEvents win=0x0000021f masks={device=1 mask=0x0003e000;};
001:<:0098: 20: XInputExtension-Request(131,46): XISelectEvents win=0x0000021f masks={device=0 mask=0x00000800;};
001:<:0099: 16: XKEYBOARD-Request(135,1): SelectEvents opcode=0x87 opcode2=0x01 unparsed-data=0x00,0x01,0x07,0x00;
001:<:00a7: 44: Request(1): CreateWindow depth=0x00 window=0x00200002 parent=0x0000021f x=0 y=0 width=744 height=464 value-list={border-pixel=0x00000000 event-mask=KeyPress,KeyRelease,StructureNotify colormap=CopyFromParent(0x00000000)}
001:<:00b6: 20: XInputExtension-Request(131,46): XISelectEvents win=0x00200002 masks={device=1 mask=0x001c07f0;};
001:<:00b8: 44: Request(25): SendEvent propagate=false(0x00) destination=0x0000021f event-mask=SubstructureNotify,SubstructureRedirect ClientMessage(33) format=0x20 window=0x00200002 type=0xf1(\"_NET_WM_STATE\") data=0x00,0x00,0x00,0x00,0xf2,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00;
001:<:00b9: 44: Request(25): SendEvent propagate=false(0x00) destination=0x0000021f event-mask=SubstructureNotify,SubstructureRedirect ClientMessage(33) format=0x20 window=0x00200002 type=0xf1(\"_NET_WM_STATE\") data=0x00,0x00,0x00,0x00,0xf3,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00;
001:<:00c3:  8: Request(8): MapWindow window=0x00200002
001:<: Warning: buffer filled!
001:<:00f2: 16: XFIXES-Request(138,23): SetCursorName cursor=0x00200008 name='text'
001:<:0100: 12: Generic Event Extension-Request(128,0): QueryVersion major version=1 minor version=0
";

    fn line(key: &str, log: &str) -> String {
        summary(log)
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .unwrap_or_default()
    }

    #[test]
    fn the_requests_are_named_once_with_their_extension() {
        assert_eq!(
            line("x11.wire.requests", LOG),
            "BIG-REQUESTS:Enable ChangeWindowAttributes CreateWindow \
             Generic Event Extension:QueryVersion InternAtom MapWindow QueryExtension \
             RANDR:SelectInput SendEvent XFIXES:SetCursorName \
             XInputExtension:XISelectEvents XKEYBOARD:SelectEvents"
        );
    }

    #[test]
    fn the_atoms_are_named_and_counted() {
        assert_eq!(
            line("x11.wire.atoms", LOG),
            "4: _NET_WM_STATE _NET_WM_STATE_ABOVE _NET_WM_STATE_BELOW _XSETTINGS_S0"
        );
    }

    /// The root's raw XI2 events (bits 13-17), which a06dcfbb0a stopped
    /// selecting, read as names, and each window by its role.
    #[test]
    fn the_selections_name_the_window_and_the_events() {
        assert_eq!(
            line("x11.wire.selects", LOG),
            "XKB 00010700; \
             none events PropertyChange; \
             root RANDR ScreenChangeNotifyMask,OutputPropertyNotifyMask; \
             root XI2 all HierarchyChanged; \
             root XI2 all-master RawKeyPress,RawKeyRelease,RawButtonPress,RawButtonRelease,RawMotion; \
             window XI2 all-master ButtonPress,ButtonRelease,Motion,Enter,Leave,FocusIn,FocusOut,TouchBegin,TouchUpdate,TouchEnd; \
             window events KeyPress,KeyRelease,StructureNotify"
        );
    }

    /// The two REMOVE messages 104dad2238 dropped and 6a6777e2b3 sends again:
    /// without them the line says so.
    #[test]
    fn the_wm_state_messages_name_their_action_and_atoms() {
        assert_eq!(
            line("x11.wire.messages", LOG),
            "window _NET_WM_STATE remove _NET_WM_STATE_ABOVE; \
             window _NET_WM_STATE remove _NET_WM_STATE_BELOW"
        );
        let dropped: String = LOG
            .lines()
            .filter(|l| !l.contains("SendEvent"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(line("x11.wire.messages", &dropped), "none");
    }

    #[test]
    fn an_error_is_named_by_the_request_it_answered() {
        assert_eq!(
            line("x11.wire.errors", LOG),
            "Window on ChangeWindowAttributes"
        );
        let clean: String = LOG
            .lines()
            .filter(|l| !l.contains(":Error "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(line("x11.wire.errors", &clean), "none");
    }
}

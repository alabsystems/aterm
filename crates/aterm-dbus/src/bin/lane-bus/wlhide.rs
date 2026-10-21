// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus wlhide <listen> <server> <rule>...`: a Wayland compositor that
//! offers less than the one it is made of. A proxy listening on the socket
//! path `<listen>` carries each client to the compositor socket `<server>`
//! ([`crate::relay`], descriptors included) and edits the globals the
//! compositor announces on every registry the client makes:
//!
//! * `hide:<interface>`: the global is never announced (nor its removal), so
//!   the client runs as under a compositor without it (sway without
//!   `zxdg_decoration_manager_v1`: client-side decorations);
//! * `cap:<interface>=<version>`: announced at most at `<version>`, so the
//!   client binds that version or older (sway's `wl_compositor` at 5, a
//!   compositor older than `wl_surface.preferred_buffer_scale`).
//!
//! `-` as the only rule changes nothing (the lane's control). Every rule a
//! proxy applied is said on stderr as it applies, so the lab reads that the
//! client saw the edited compositor.
//!
//! The wire, from the Wayland protocol (wayland.freedesktop.org, "Wire
//! Format"): each message is a 32-bit object id, then a 32-bit word whose
//! upper 16 bits are the message's size in bytes (header included, a
//! multiple of 4) and lower 16 its opcode, then the arguments in host byte
//! order. `wl_display.get_registry` is request 1 of object 1, its new id the
//! one argument; on that registry, event 0 is `global(name: uint,
//! interface: string, version: uint)` (a string is its length with the NUL,
//! then the bytes padded to 4) and event 1 `global_remove(name: uint)`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::relay::{self, Filter};

/// What a rule does to an interface's global.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rule {
    Hide(String),
    Cap(String, u32),
}

fn parse_rules(words: &[String]) -> Result<Vec<Rule>, String> {
    if words == ["-"] {
        return Ok(Vec::new());
    }
    if words.is_empty() {
        return Err("no rule (`-` for none)".into());
    }
    words
        .iter()
        .map(|w| {
            if let Some(i) = w.strip_prefix("hide:").filter(|i| !i.is_empty()) {
                Ok(Rule::Hide(i.to_string()))
            } else if let Some((i, v)) = w.strip_prefix("cap:").and_then(|r| r.split_once('=')) {
                v.parse::<u32>()
                    .ok()
                    .filter(|v| *v > 0 && !i.is_empty())
                    .map(|v| Rule::Cap(i.to_string(), v))
                    .ok_or_else(|| format!("not a version: {w}"))
            } else {
                Err(format!(
                    "not a rule: {w} (hide:<interface>, cap:<interface>=<n>)"
                ))
            }
        })
        .collect()
}

fn word(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A message's size, from its header once the header is in.
fn size(buf: &[u8]) -> Result<Option<usize>, String> {
    let Some(head) = word(buf, 4) else {
        return Ok(None);
    };
    let n = usize::try_from(head >> 16).unwrap_or(0);
    if n < 8 || n % 4 != 0 {
        return Err(format!("a message of {n} bytes"));
    }
    Ok(Some(n))
}

/// What both sides know: the client's registries, and the globals hidden
/// from it by name.
#[derive(Default)]
struct Shared {
    registries: Vec<u32>,
    hidden: Vec<u32>,
}

type Known = Arc<Mutex<Shared>>;

/// The client's side: notes each registry it makes.
struct Requests {
    known: Known,
}

impl Filter for Requests {
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
        size(buf)
    }

    fn pass(&mut self, msg: &mut [u8]) -> bool {
        let (Some(object), Some(head)) = (word(msg, 0), word(msg, 4)) else {
            return true;
        };
        if object == 1
            && head & 0xffff == 1
            && let Some(id) = word(msg, 8)
        {
            self.known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .registries
                .push(id);
        }
        true
    }
}

/// The compositor's side: edits the registries' globals.
struct Events {
    rules: Vec<Rule>,
    known: Known,
}

impl Filter for Events {
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
        size(buf)
    }

    fn pass(&mut self, msg: &mut [u8]) -> bool {
        let (Some(object), Some(head)) = (word(msg, 0), word(msg, 4)) else {
            return true;
        };
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if !known.registries.contains(&object) {
            return true;
        }
        match head & 0xffff {
            0 => {
                let (Some(name), Some(len)) = (word(msg, 8), word(msg, 12)) else {
                    return true;
                };
                let len = usize::try_from(len).unwrap_or(usize::MAX);
                let Some(text) = msg.get(16..16usize.saturating_add(len)) else {
                    return true;
                };
                let interface =
                    String::from_utf8_lossy(text.strip_suffix(&[0]).unwrap_or(text)).into_owned();
                let at = 16 + len.div_ceil(4) * 4;
                for rule in &self.rules {
                    match rule {
                        Rule::Hide(i) if *i == interface => {
                            known.hidden.push(name);
                            eprintln!("lane-bus: wlhide: hid {interface} (global {name})");
                            return false;
                        }
                        Rule::Cap(i, cap) if *i == interface => {
                            if let Some(v) = word(msg, at)
                                && v > *cap
                                && let Some(slot) = msg.get_mut(at..at + 4)
                            {
                                slot.copy_from_slice(&cap.to_ne_bytes());
                                eprintln!(
                                    "lane-bus: wlhide: announced {interface} at version {cap}, not {v}"
                                );
                            }
                        }
                        _ => {}
                    }
                }
                true
            }
            1 => word(msg, 8).is_none_or(|name| !known.hidden.contains(&name)),
            _ => true,
        }
    }
}

pub(crate) fn run(listen: &str, server: &str, rules: &[String]) -> Result<(), String> {
    let rules = parse_rules(rules)?;
    eprintln!("lane-bus: wlhide: rules {rules:?}");
    relay::serve(Path::new(listen), Path::new(server), || {
        let known = Known::default();
        (
            Requests {
                known: Arc::clone(&known),
            },
            Events {
                rules: rules.clone(),
                known,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(object: u32, opcode: u16, args: &[u8]) -> Vec<u8> {
        let size = u32::try_from(8 + args.len()).unwrap();
        let mut m = object.to_ne_bytes().to_vec();
        m.extend_from_slice(&(size << 16 | u32::from(opcode)).to_ne_bytes());
        m.extend_from_slice(args);
        m
    }

    fn global(name: u32, interface: &str, version: u32) -> Vec<u8> {
        let mut a = name.to_ne_bytes().to_vec();
        let n = u32::try_from(interface.len() + 1).unwrap();
        a.extend_from_slice(&n.to_ne_bytes());
        a.extend_from_slice(interface.as_bytes());
        a.push(0);
        a.resize(8 + (interface.len() + 1).div_ceil(4) * 4, 0);
        a.extend_from_slice(&version.to_ne_bytes());
        msg(2, 0, &a)
    }

    fn through(f: &mut dyn Filter, mut stream: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(n) = f.len(stream).unwrap() {
            let mut m = stream[..n].to_vec();
            if f.pass(&mut m) {
                out.extend_from_slice(&m);
            }
            stream = &stream[n..];
        }
        out
    }

    #[test]
    fn a_hidden_global_never_comes_and_a_capped_one_comes_older() {
        let rules = parse_rules(&[
            "hide:zxdg_decoration_manager_v1".to_string(),
            "cap:wl_compositor=5".to_string(),
        ])
        .unwrap();
        let known = Known::default();
        let mut req = Requests {
            known: Arc::clone(&known),
        };
        let get_registry = msg(1, 1, &2u32.to_ne_bytes());
        assert_eq!(through(&mut req, &get_registry), get_registry);
        let mut ev = Events { rules, known };
        let mut stream = global(1, "wl_compositor", 6);
        stream.extend(global(2, "zxdg_decoration_manager_v1", 1));
        stream.extend(global(3, "wl_shm", 1));
        stream.extend(msg(2, 1, &2u32.to_ne_bytes()));
        stream.extend(msg(2, 1, &3u32.to_ne_bytes()));
        // Not a registry: object 5's event 0 passes untouched.
        stream.extend(msg(5, 0, &[1, 2, 3, 4]));
        let out = through(&mut ev, &stream);
        let mut want = global(1, "wl_compositor", 5);
        want.extend(global(3, "wl_shm", 1));
        want.extend(msg(2, 1, &3u32.to_ne_bytes()));
        want.extend(msg(5, 0, &[1, 2, 3, 4]));
        assert_eq!(out, want);
    }

    #[test]
    fn a_rule_is_read_or_refused() {
        assert_eq!(parse_rules(&["-".into()]).unwrap(), []);
        assert!(parse_rules(&[]).is_err());
        assert!(parse_rules(&["cap:wl_compositor=0".into()]).is_err());
        assert!(parse_rules(&["show:x".into()]).is_err());
        let mut four = 1u32.to_ne_bytes().to_vec();
        four.extend_from_slice(&(4u32 << 16).to_ne_bytes());
        assert!(size(&four).is_err(), "a message shorter than its header");
    }
}

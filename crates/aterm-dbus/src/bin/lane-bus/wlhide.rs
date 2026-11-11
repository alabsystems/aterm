// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus wlhide <listen> <server> <rule>...`: a Wayland compositor that offers less than the
//! one it is made of.

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
                v.bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| v.parse::<u32>().ok())
                    .flatten()
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
#[path = "wlhide_tests.rs"]
mod tests;

// Copyright 2019 x11rb Contributers
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from x11rb-protocol 0.13.2 (MIT OR Apache-2.0, used under
// Apache-2.0) src/resource_manager/parser.rs (`parse_database`,
// `parse_entry`, `parse_query`) and src/resource_manager/matcher.rs
// (`check_match`, `compare_matches`, `match_entry`), and rewritten by the
// aterm project in 2026: one module, no `#include` (a database read from the
// RESOURCE_MANAGER property has no directory to include from), and a bound on
// the matcher's states.

//! The X resource manager's database (Xrm): `name.class*rest: value` lines,
//! as `RESOURCE_MANAGER` on the root window holds them (`xrdb`), and the
//! precedence rules that pick one entry for a query such as `Xft.dpi`. The
//! answers are x11rb's for the same database and query
//! (`tests/recorded/x11rb-protocol-0.13.2-xrm.tsv`).
//!
//! Any client may write `RESOURCE_MANAGER`, so the parser takes arbitrary
//! bytes: it never panics, skips what it cannot read, and the matcher keeps
//! at most [`MAX_STATES`] partial matches per entry.

use std::cmp::Ordering;

/// The most partial matches the matcher tracks for one entry. A query is a
/// handful of components, which needs a handful; a hostile entry of many
/// loose bindings cannot make it more.
pub const MAX_STATES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Tight,
    Loose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Component {
    Name(Vec<u8>),
    Wildcard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    components: Vec<(Binding, Component)>,
    value: Vec<u8>,
}

/// A parsed resource database.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Database {
    entries: Vec<Entry>,
}

fn quark_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

fn span(data: &[u8], keep: impl Fn(u8) -> bool) -> (&[u8], &[u8]) {
    let end = data.iter().position(|&c| !keep(c)).unwrap_or(data.len());
    data.split_at(end)
}

fn skip_to_eol(data: &[u8]) -> &[u8] {
    span(data, |c| c != b'\n').1
}

fn is_octal(c: u8) -> bool {
    (b'0'..=b'7').contains(&c)
}

impl Database {
    /// Parse a database. Lines that do not parse are skipped, as Xlib skips
    /// them; `#include` and every other directive is ignored.
    pub fn parse(mut data: &[u8]) -> Database {
        let mut entries = Vec::new();
        while let Some(&first) = data.first() {
            match first {
                b'\n' => data = &data[1..],
                b'!' | b'#' => data = skip_to_eol(data),
                _ => {
                    let (entry, rest) = parse_entry(data);
                    data = rest;
                    entries.extend(entry);
                }
            }
        }
        Database { entries }
    }

    /// The value of the best entry for the query `name` / `class` (dotted,
    /// no wildcards: `Xft.dpi`), or `None`.
    pub fn get(&self, name: &str, class: &str) -> Option<&[u8]> {
        let name = parse_query(name.as_bytes())?;
        let class = parse_query(class.as_bytes())?;
        self.entries
            .iter()
            .filter_map(|entry| {
                let best = check_match(entry, &name, &class)
                    .into_iter()
                    .max_by(|a, b| compare(a, b))?;
                Some((entry, best))
            })
            .max_by(|(_, a), (_, b)| compare(a, b))
            .map(|(entry, _)| &entry.value[..])
    }

    /// [`Database::get`] as UTF-8.
    pub fn get_string(&self, name: &str, class: &str) -> Option<&str> {
        std::str::from_utf8(self.get(name, class)?).ok()
    }
}

fn parse_components(mut data: &[u8]) -> (Vec<(Binding, Component)>, &[u8]) {
    let mut out = Vec::new();
    loop {
        let mut binding = Binding::Tight;
        let mut rest = data;
        while let Some(&c) = rest.first() {
            match c {
                b'*' => binding = Binding::Loose,
                b'.' => {}
                _ => break,
            }
            rest = &rest[1..];
        }
        let component = if rest.first() == Some(&b'?') {
            rest = &rest[1..];
            Component::Wildcard
        } else {
            let (name, after) = span(rest, quark_byte);
            if name.is_empty() {
                break;
            }
            rest = after;
            Component::Name(name.to_vec())
        };
        out.push((binding, component));
        data = rest;
    }
    (out, data)
}

fn parse_entry(data: &[u8]) -> (Option<Entry>, &[u8]) {
    let (components, data) = parse_components(data);
    match components.last() {
        None | Some((_, Component::Wildcard)) => return (None, skip_to_eol(data)),
        Some(_) => {}
    }
    let data = span(data, |c| c == b' ').1;
    let Some((&b':', mut data)) = data.split_first() else {
        return (None, skip_to_eol(data));
    };
    loop {
        let rest = span(data, |c| c == b' ' || c == b'\t').1;
        if rest.starts_with(b"\\\n") {
            data = &rest[2..];
        } else {
            data = rest;
            break;
        }
    }
    let mut value = Vec::new();
    let mut i = 0;
    let mut octal: Option<(u8, Option<u8>)> = None;
    while let Some(&b) = data.get(i) {
        i += 1;
        if b == b'\n' {
            break;
        }
        if let Some((x, y)) = octal {
            if is_octal(b) {
                match y {
                    None => octal = Some((x, Some(b))),
                    Some(y) => {
                        let v = ((x - b'0') * 8 + (y - b'0')).wrapping_mul(8) + (b - b'0');
                        value.push(v);
                        octal = None;
                    }
                }
                continue;
            }
            value.push(b'\\');
            value.push(x);
            value.extend(y);
            octal = None;
        }
        if b != b'\\' {
            value.push(b);
            continue;
        }
        match data.get(i) {
            None => {
                value.push(b);
                continue;
            }
            Some(b' ') => value.push(b' '),
            Some(b'\t') => value.push(b'\t'),
            Some(b'n') => value.push(b'\n'),
            Some(b'\\') => value.push(b'\\'),
            Some(b'\n') => {}
            Some(&x) if is_octal(x) => octal = Some((x, None)),
            Some(&x) => {
                value.push(b);
                value.push(x);
            }
        }
        i += 1;
    }
    let rest = data.get(i..).unwrap_or_default();
    (Some(Entry { components, value }), rest)
}

/// A dotted query: components of quark bytes; `None` for anything else.
fn parse_query(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    loop {
        let (name, rest) = span(data, quark_byte);
        if name.is_empty() {
            break;
        }
        out.push(name);
        data = span(rest, |c| c == b'.').1;
    }
    data.is_empty().then_some(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Instance,
    Class,
    Wildcard,
}

/// How one query component was consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Skipped by a loose binding (`*`).
    Skipped,
    /// Matched, after the binding before it.
    Matched(Binding, How),
}

/// Every way `entry` matches the whole query: a nondeterministic walk where a
/// loose binding may also swallow a query component.
fn check_match(entry: &Entry, name: &[&[u8]], class: &[&[u8]]) -> Vec<Vec<Step>> {
    let mut states: Vec<(usize, Vec<Step>)> = vec![(0, Vec::new())];
    for i in 0..name.len().max(class.len()) {
        let (n, c) = (name.get(i).copied(), class.get(i).copied());
        let mut next = Vec::new();
        for (index, history) in states {
            let Some((binding, component)) = entry.components.get(index) else {
                continue;
            };
            if *binding == Binding::Loose && next.len() < MAX_STATES {
                let mut skipped = history.clone();
                skipped.push(Step::Skipped);
                next.push((index, skipped));
            }
            let how = match component {
                Component::Wildcard => Some(How::Wildcard),
                Component::Name(s) if Some(&s[..]) == n => Some(How::Instance),
                Component::Name(s) if Some(&s[..]) == c => Some(How::Class),
                Component::Name(_) => None,
            };
            if let Some(how) = how
                && next.len() < MAX_STATES
            {
                let mut matched = history;
                matched.push(Step::Matched(*binding, how));
                next.push((index + 1, matched));
            }
        }
        states = next;
    }
    states
        .into_iter()
        .filter(|(index, _)| *index == entry.components.len())
        .map(|(_, history)| history)
        .collect()
}

/// Xrm's precedence, component by component from the left: a match beats a
/// skip; an instance beats a class, which beats `?`; a tight binding beats a
/// loose one.
fn compare(a: &[Step], b: &[Step]) -> Ordering {
    fn rule1(a: Step, b: Step) -> bool {
        matches!((a, b), (Step::Matched(..), Step::Skipped))
    }
    fn rule2(a: Step, b: Step) -> bool {
        matches!(
            (a, b),
            (
                Step::Matched(_, How::Instance),
                Step::Matched(_, How::Class | How::Wildcard)
            ) | (
                Step::Matched(_, How::Class),
                Step::Matched(_, How::Wildcard)
            )
        )
    }
    fn rule3(a: Step, b: Step) -> bool {
        matches!(
            (a, b),
            (
                Step::Matched(Binding::Tight, _),
                Step::Matched(Binding::Loose, _)
            )
        )
    }
    let wins = |rule: fn(Step, Step) -> bool, x: Step, y: Step| {
        if rule(x, y) {
            Ordering::Greater
        } else if rule(y, x) {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    };
    for (&x, &y) in a.iter().zip(b) {
        let o = wins(rule1, x, y)
            .then_with(|| wins(rule2, x, y))
            .then_with(|| wins(rule3, x, y));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xft_dpi_from_an_xrdb_property() {
        let db = Database::parse(b"Xft.antialias:\t1\nXft.dpi:\t144\n*background: #000\n");
        assert_eq!(db.get_string("Xft.dpi", ""), Some("144"));
        assert_eq!(db.get_string("Xft.hinting", ""), None);
        assert_eq!(
            db.get_string("aterm.background", "ATerm.Background"),
            Some("#000")
        );
    }

    #[test]
    fn a_hostile_entry_stays_bounded() {
        let mut line = Vec::new();
        for _ in 0..4096 {
            line.extend_from_slice(b"*a");
        }
        line.extend_from_slice(b": x\n");
        let db = Database::parse(&line);
        let query = vec!["a"; 64].join(".");
        // Any answer, in bounded time and memory: it must simply return.
        let _ = db.get(&query, &query);
    }
}

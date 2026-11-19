// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compare what the dock exposes to assistive technology independently of when its sampler ran.

use std::collections::{BTreeMap, BTreeSet};

use super::{event_body, event_kind, node_role, proof, quoted_field};

/// The sampled preview always belongs to the script's shell. Keep its session identity,
/// state, foreground program and shortcut verbatim, including changes to any of them.
fn session_label(name: &str) -> Option<String> {
    let mut parts = name.splitn(5, ", ");
    let (title, state, program, age) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let (preview, shortcut) = parts.next()?.rsplit_once(", ")?;
    if age != "now" {
        age.strip_suffix(['m', 'h', 'd'])?.parse::<u64>().ok()?;
    }
    let typed = preview.strip_prefix(proof::PROMPT)?;
    if !typed.is_empty() && !typed.starts_with(' ') {
        return None;
    }
    Some(format!(
        "{title}, {state}, {program}, <age>, <shell preview>, {shortcut}"
    ))
}

fn nonnegative(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.)
}

/// The resource description announces a history window followed by one reading and bucket
/// count per metric. Compare its wording, channels, units and history length. The sampler
/// owns the latest value, scale, populated buckets, trace values and collection ages.
fn resource_description(description: &str) -> Option<String> {
    let (first, _) = description.split_once(", scale 0 to ")?;
    let (head, _) = first.rsplit_once(". ")?;
    let mut channels = Vec::new();
    for sentence in description[head.len()..].split(". ") {
        let Some((reading, scale)) = sentence.trim_end_matches('.').split_once(", scale 0 to ")
        else {
            continue;
        };
        let (scale, count) = scale.split_once(", ")?;
        let (max, unit) = scale.split_once(' ')?;
        nonnegative(max)?;
        let (present, buckets) = count.strip_suffix(" observed")?.split_once(" of ")?;
        if present.parse::<usize>().ok()? > buckets.parse::<usize>().ok()? {
            return None;
        }
        let metric = if let Some(metric) = reading.strip_suffix(" unavailable") {
            metric
        } else {
            let (label, value) = reading.strip_suffix(unit)?.trim_end().rsplit_once(' ')?;
            nonnegative(value)?;
            label.strip_suffix(" at least").unwrap_or(label)
        };
        channels.push(format!("{metric} ({unit}, {buckets} buckets)"));
    }
    Some(format!("{head}. {}.", channels.join(", ")))
}

/// The unmodified node is also recorded as a measurement where a live field was projected.
pub(super) fn node(observed: &str, hostname: &str) -> String {
    let mut stable = observed.to_owned();
    match node_role(observed) {
        Some("embedded")
            if !hostname.is_empty()
                && quoted_field(observed, "name").as_deref() == Some(hostname) =>
        {
            stable = stable.replacen(
                &format!(" name={hostname:?}"),
                " name=\"<guest hostname>\"",
                1,
            );
        }
        Some("push_button") => {
            for (key, project) in [
                ("name", session_label as fn(&str) -> Option<String>),
                ("description", resource_description),
            ] {
                if let Some(raw) = quoted_field(observed, key)
                    && let Some(value) = project(&raw)
                {
                    stable = stable.replacen(
                        &format!(" {key}={raw:?}"),
                        &format!(" {key}={value:?}"),
                        1,
                    );
                }
            }
        }
        _ => {}
    }
    stable
}

fn child_ref(body: &str) -> Option<&str> {
    proof::field(body, "any")?
        .strip_prefix("ref(")?
        .strip_suffix(')')
}

/// The numbered roster nests its title beside the Session details button. The heading can
/// already have been published without its wrapper when the listener attaches.
struct Header {
    outer: String,
    inner: String,
    heading: String,
}

impl Header {
    fn from_node(path: &str, node: &str, ancestors: &[(&str, bool)]) -> Option<Self> {
        let [.., (outer, true), (inner, true)] = ancestors else {
            return None;
        };
        (node_role(node) == Some("heading")
            && quoted_field(node, "name").as_deref() == Some("Sessions"))
        .then(|| Self {
            outer: (*outer).to_string(),
            inner: (*inner).to_string(),
            heading: path.to_string(),
        })
    }

    /// Signals that install the wrapper and move this heading into it.
    fn publishes(&self, kind: &str, source: &str, target: Option<&str>) -> bool {
        let (from, to) = match kind {
            "Object.ChildrenChanged:add@panel" => (&self.outer, &self.inner),
            "Object.ChildrenChanged:remove@panel" => (&self.outer, &self.heading),
            "Object.PropertyChange:accessible-parent@heading" => (&self.heading, &self.inner),
            _ => return false,
        };
        source == from.as_str() && target == Some(to.as_str())
    }
}

/// Paths come from the two actual walks. Two controls with the same AT-SPI role can have
/// different contracts, e.g. the live shell button and the fixed Session details button.
#[derive(Default)]
pub(super) struct Dock {
    frames: BTreeMap<String, String>,
    lists: BTreeSet<String>,
    row_lists: BTreeMap<String, String>,
    resources: BTreeMap<String, String>,
    heading_bounds: BTreeMap<String, BTreeSet<String>>,
    headers: Vec<Header>,
}

impl Dock {
    pub(super) fn observe(&mut self, path: &str, node: &str, ancestors: &[(&str, bool)]) {
        let Some(&(parent, _)) = ancestors.last() else {
            return;
        };
        match node_role(node) {
            Some("embedded") => {
                self.frames.insert(path.to_string(), parent.to_string());
            }
            Some("list") if self.frames.contains_key(parent) => {
                self.lists.insert(path.to_string());
            }
            Some("heading") => {
                if matches!(
                    quoted_field(node, "name").as_deref(),
                    Some("Sessions" | "Other sessions")
                ) && let Some(bounds) = proof::field(node, "extents_window")
                {
                    self.heading_bounds
                        .entry(path.to_string())
                        .or_default()
                        .insert(bounds.to_string());
                }
                if let Some(header) = Header::from_node(path, node, ancestors) {
                    self.headers.push(header);
                }
            }
            Some("push_button") => {
                if let [.., (list, _), (row, true)] = ancestors
                    && self.lists.contains(*list)
                    && quoted_field(node, "name")
                        .and_then(|s| session_label(&s))
                        .is_some()
                    && let Some(schema) =
                        quoted_field(node, "description").and_then(|s| resource_description(&s))
                {
                    self.row_lists
                        .insert((*row).to_string(), (*list).to_string());
                    self.resources.insert(path.to_string(), schema);
                }
            }
            _ => {}
        }
    }

    fn publishes(&self, event: &Event<'_>) -> bool {
        let child = child_ref(event.body);
        if self
            .headers
            .iter()
            .any(|h| h.publishes(&event.kind, event.source, child))
        {
            return true;
        }
        match event.kind.as_str() {
            "Object.ChildrenChanged:add@frame" => child
                .and_then(|p| self.frames.get(p))
                .is_some_and(|frame| frame == event.source),
            "Object.ChildrenChanged:add@list" => child
                .and_then(|p| self.row_lists.get(p))
                .is_some_and(|list| list == event.source),
            "Object.BoundsChanged@heading" => {
                self.heading_bounds.get(event.source).is_some_and(|bounds| {
                    proof::field(event.body, "any")
                        .and_then(|r| r.strip_prefix('(')?.strip_suffix(')'))
                        .is_some_and(|rect| bounds.contains(rect))
                })
            }
            "Object.PropertyChange:accessible-description@push_button" => {
                self.resources.get(event.source).is_some_and(|schema| {
                    quoted_field(event.body, "any")
                        .and_then(|s| resource_description(&s))
                        .as_ref()
                        == Some(schema)
                })
            }
            _ => false,
        }
    }

    /// An initial placeholder can be gone from both walks. Its removal is observable as
    /// the roster list dropping that exact path and the path immediately becoming defunct.
    fn removes_placeholder(&self, remove: &Event<'_>, defunct: &Event<'_>) -> bool {
        remove.kind == "Object.ChildrenChanged:remove@list"
            && self.lists.contains(remove.source)
            && defunct.kind == "Object.StateChanged:defunct=1@?"
            && child_ref(remove.body) == Some(defunct.source)
    }
}

struct Event<'a> {
    body: &'a str,
    source: &'a str,
    kind: String,
}

pub(super) fn event_kinds(
    lines: &[&str],
    roles: &[(String, String)],
    dock: &Dock,
) -> BTreeSet<String> {
    let events: Vec<_> = lines
        .iter()
        .map(|line| {
            let body = event_body(line);
            Event {
                body,
                source: body.split_whitespace().nth(1).unwrap_or(""),
                kind: event_kind(line, roles),
            }
        })
        .collect();
    let mut events = events.iter().peekable();
    let mut kinds = BTreeSet::new();
    while let Some(event) = events.next() {
        if events
            .peek()
            .is_some_and(|next| dock.removes_placeholder(event, next))
        {
            events.next();
        } else if !event
            .kind
            .starts_with("Object.PropertyChange:accessible-name@")
            && !dock.publishes(event)
        {
            kinds.insert(event.kind.clone());
        }
    }
    kinds
}

#[cfg(test)]
#[path = "navigator_tests.rs"]
mod tests;

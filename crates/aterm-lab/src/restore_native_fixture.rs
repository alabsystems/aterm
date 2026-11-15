// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compose saved mixed tabs from real standalone descriptors. The app's restore schema is
//! private; these envelopes interpret only placement and retain opaque payloads verbatim.

use std::collections::{BTreeMap, BTreeSet};

use aterm_toml::Value;
use serde::{Deserialize, Serialize};

use super::Case;

#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    windows: Vec<Window>,
    #[serde(flatten)]
    other: BTreeMap<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct Window {
    restored_tabs: Vec<Tab>,
    active_tab: usize,
    active_item: usize,
    #[serde(flatten)]
    other: BTreeMap<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct Tab {
    root: Value,
    // The real serializer omits an empty (root-leaf) focus path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    focused_path: Vec<String>,
    zoomed: bool,
    #[serde(flatten)]
    other: BTreeMap<String, Value>,
}

pub(super) struct Composition {
    pub(super) text: String,
    native: BTreeMap<String, Value>,
    pub(super) bootstrap_key: Option<String>,
}

fn parse(text: &str) -> Result<Manifest, String> {
    let manifest: Manifest = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
    if manifest.schema != 2 || manifest.windows.is_empty() {
        return Err("fixture needs nonempty schema-2 windows".into());
    }
    Ok(manifest)
}

fn word<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("fixture lacks string {field}"))
}

fn leaves<'a>(node: &'a Value, depth: usize, found: &mut Vec<&'a Value>) -> Result<(), String> {
    if depth > 32 {
        return Err("fixture split depth exceeds restore bound".into());
    }
    match word(node, "node")? {
        "leaf" => found.push(node.get("view").ok_or("fixture leaf lacks view")?),
        "split" => {
            if !matches!(word(node, "axis")?, "horizontal" | "vertical")
                || node
                    .get("ratio")
                    .and_then(Value::as_float)
                    .is_none_or(|ratio| !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0)
            {
                return Err("fixture has invalid split axis/ratio".into());
            }
            for side in ["first", "second"] {
                leaves(
                    node.get(side).ok_or("fixture split lacks child")?,
                    depth + 1,
                    found,
                )?;
            }
        }
        _ => return Err("fixture has unknown node".into()),
    }
    Ok(())
}

fn tab_leaves(tab: &Tab) -> Result<Vec<&Value>, String> {
    if tab.zoomed
        || ["parked", "ephemeral", "fleet"].iter().any(|name| {
            tab.other
                .get(*name)
                .is_some_and(|value| value.as_bool() != Some(false))
        })
    {
        return Err("fixture source tab carries unexpected presentation/ownership flags".into());
    }
    let mut focused = &tab.root;
    for side in &tab.focused_path {
        if word(focused, "node")? != "split" || !matches!(side.as_str(), "first" | "second") {
            return Err("fixture focus does not name a canonical child".into());
        }
        focused = focused.get(side).ok_or("fixture focus child is missing")?;
    }
    if word(focused, "node")? != "leaf" {
        return Err("fixture focus does not name a leaf".into());
    }
    let mut found = Vec::new();
    leaves(&tab.root, 0, &mut found)?;
    Ok(found)
}

fn terminal(tab: &Tab, expected: usize, seen: &mut BTreeSet<String>) -> Result<(), String> {
    let found = tab_leaves(tab)?;
    if found.len() != expected {
        return Err("fixture terminal subtree has wrong leaf count".into());
    }
    for view in found {
        let sid = word(view, "sid")?;
        if word(view, "kind")? != "terminal" || sid.is_empty() || !seen.insert(sid.to_owned()) {
            return Err("fixture requires distinct real terminal descriptors".into());
        }
    }
    Ok(())
}

fn editor(tab: &Tab, uri: &str) -> Result<(), String> {
    let found = tab_leaves(tab)?;
    if found.len() != 1
        || word(found[0], "kind")? != "native"
        || word(found[0], "restore_tag")? != "editor"
        || word(found[0], "uri")? != uri
    {
        return Err("fixture editor descriptor differs from requested URI".into());
    }
    Ok(())
}

fn native_positions(manifest: &Manifest) -> Result<BTreeMap<String, Value>, String> {
    fn visit(node: &Value, key: &str, found: &mut BTreeMap<String, Value>) -> Result<(), String> {
        if word(node, "node")? == "split" {
            let prefix = key.strip_suffix('/').unwrap_or(key);
            for (side, name) in ["first", "second"].into_iter().enumerate() {
                visit(
                    node.get(name).ok_or("fixture split lacks child")?,
                    &format!("{prefix}/{side}"),
                    found,
                )?;
            }
        } else {
            let view = node.get("view").ok_or("fixture leaf lacks view")?;
            if word(view, "kind")? == "native" {
                found.insert(key.to_owned(), view.clone());
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    for (w, window) in manifest.windows.iter().enumerate() {
        for (t, tab) in window.restored_tabs.iter().enumerate() {
            tab_leaves(tab)?;
            visit(&tab.root, &format!("{}:{}:/", w + 1, t + 1), &mut found)?;
        }
    }
    Ok(found)
}

fn bootstrap_key(manifest: &Manifest) -> Result<Option<String>, String> {
    fn find(node: &Value, key: &str) -> Result<Option<String>, String> {
        if word(node, "node")? == "split" {
            let prefix = key.strip_suffix('/').unwrap_or(key);
            for (side, name) in ["first", "second"].into_iter().enumerate() {
                if let Some(key) = find(
                    node.get(name).ok_or("fixture split lacks child")?,
                    &format!("{prefix}/{side}"),
                )? {
                    return Ok(Some(key));
                }
            }
            Ok(None)
        } else {
            let view = node.get("view").ok_or("fixture leaf lacks view")?;
            Ok((word(view, "kind")? == "terminal").then(|| key.to_owned()))
        }
    }
    for (tab, record) in manifest.windows[0].restored_tabs.iter().enumerate() {
        if let Some(key) = find(&record.root, &format!("1:{}:/", tab + 1))? {
            return Ok(Some(key));
        }
    }
    Ok(None)
}

pub(super) fn capture_native_only(text: &str, uris: &[String]) -> Result<Composition, String> {
    let manifest = parse(text)?;
    if uris.is_empty() || manifest.windows.len() != 1 {
        return Err("native-only fixture needs one nonempty window".into());
    }
    let window = &manifest.windows[0];
    if window.restored_tabs.len() != uris.len() || window.active_item != 0 || window.active_tab != 0
    {
        return Err("native-only fixture changed its tab count or first selection".into());
    }
    for (tab, uri) in window.restored_tabs.iter().zip(uris) {
        editor(tab, uri)?;
    }
    Ok(Composition {
        text: text.to_owned(),
        native: native_positions(&manifest)?,
        bootstrap_key: None,
    })
}

fn preserve_editor_first(
    text: &str,
    manifest: &Manifest,
    case: Case,
    uris: &[String],
) -> Result<Composition, String> {
    let extra = usize::from(case.extra_window());
    let main_count = uris
        .len()
        .checked_sub(extra)
        .filter(|n| *n > 0)
        .ok_or("editor-first fixture has no main-window editor")?;
    if manifest.windows.len() != 1 + extra {
        return Err("editor-first fixture has wrong window count".into());
    }
    let main = &manifest.windows[0];
    if main.restored_tabs.len() != main_count + 1 || main.active_item != 1 || main.active_tab != 0 {
        return Err("editor-first fixture did not retain the later active bootstrap".into());
    }
    let mut seen = BTreeSet::new();
    terminal(&main.restored_tabs[1], 1, &mut seen)?;
    for (n, uri) in uris[..main_count].iter().enumerate() {
        editor(&main.restored_tabs[if n == 0 { 0 } else { n + 1 }], uri)?;
    }
    if extra != 0 {
        let window = &manifest.windows[1];
        if window.restored_tabs.len() != 1 || window.active_item != 0 || window.active_tab != 0 {
            return Err("additional fixture window is not one native-only tab".into());
        }
        editor(&window.restored_tabs[0], &uris[main_count])?;
    }
    Ok(Composition {
        // These scenarios are made entirely by real UI operations. Keep their capture intact.
        text: text.to_owned(),
        native: native_positions(manifest)?,
        bootstrap_key: bootstrap_key(manifest)?,
    })
}

/// Retain complete leaf descriptors while changing only their canonical tree placement.
pub(super) fn compose(
    text: &str,
    case: Case,
    uris: &[String],
    split_leaves: usize,
) -> Result<Composition, String> {
    let mut manifest = parse(text)?;
    if case.editor_first() {
        return preserve_editor_first(text, &manifest, case, uris);
    }
    if manifest.windows.len() != 1 {
        return Err("mixed/native fixture needs exactly one window".into());
    }
    let window = &mut manifest.windows[0];
    let paired = case.paired();
    let expected = 1 + uris.len() * if paired { 2 } else { 1 };
    if uris.is_empty()
        || (case == Case::Split && (uris.len() != 1 || split_leaves < 2))
        || window.restored_tabs.len() != expected
        || window.active_tab != 0
        || window.active_item != 0
    {
        return Err("fixture source has wrong tab count or active bootstrap".into());
    }
    let mut source = std::mem::take(&mut window.restored_tabs).into_iter();
    let bootstrap = source.next().ok_or("fixture has no bootstrap")?;
    let mut seen = BTreeSet::new();
    terminal(&bootstrap, 1, &mut seen)?;
    let mut tabs = vec![bootstrap];
    for uri in uris {
        let mut editor = source.next().ok_or("fixture editor is missing")?;
        self::editor(&editor, uri)?;
        if paired {
            let terminal_tab = source.next().ok_or("fixture terminal partner is missing")?;
            if terminal_tab.other.iter().any(|(name, value)| {
                !matches!(name.as_str(), "parked" | "ephemeral" | "fleet")
                    || value.as_bool() != Some(false)
            }) {
                return Err("fixture terminal tab carries metadata that cannot be merged".into());
            }
            terminal(
                &terminal_tab,
                if case == Case::Split {
                    split_leaves - 1
                } else {
                    1
                },
                &mut seen,
            )?;
            editor.root = Value::Table(
                [
                    ("node".into(), Value::String("split".into())),
                    ("axis".into(), Value::String("horizontal".into())),
                    ("ratio".into(), Value::Float(0.5)),
                    ("first".into(), editor.root),
                    ("second".into(), terminal_tab.root),
                ]
                .into_iter()
                .collect(),
            );
            editor.focused_path = std::iter::once("second".to_owned())
                .chain(terminal_tab.focused_path)
                .collect();
        }
        tabs.push(editor);
    }
    window.restored_tabs = tabs;
    // Same projection removal as WindowLayout::forget_legacy_mirror: canonical trees now own
    // placement. Retaining the old standalone mirror would describe different tab boundaries.
    for name in ["tabs", "native_tabs", "tab_order"] {
        window.other.remove(name);
    }
    let native = native_positions(&manifest)?;
    let text = aterm_toml::to_string(&manifest).map_err(|e| e.to_string())?;
    Ok(Composition {
        text,
        native,
        bootstrap_key: bootstrap_key(&manifest)?,
    })
}

impl Composition {
    /// Project away only the proven live bootstrap added by a used native-only restore.
    /// This validates the capture in memory; it never changes the saved fixture on disk.
    pub(super) fn check_with_preserved_bootstrap(
        &self,
        text: &str,
        sid: &str,
    ) -> Result<(), String> {
        if self.bootstrap_key.is_some() {
            return Err("preserved bootstrap proof needs a native-only source".into());
        }
        let mut manifest = parse(text)?;
        if manifest.windows.len() != 1 {
            return Err("preserved bootstrap changed window count".into());
        }
        let window = &mut manifest.windows[0];
        if window.active_item != 0 || window.active_tab != 0 || window.restored_tabs.is_empty() {
            return Err("preserved bootstrap is not selected first".into());
        }
        let found = tab_leaves(&window.restored_tabs[0])?;
        if found.len() != 1
            || word(found[0], "kind")? != "terminal"
            || word(found[0], "sid")? != sid
        {
            return Err("preserved bootstrap identity differs".into());
        }
        window.restored_tabs.remove(0);
        self.check_round_trip(&aterm_toml::to_string(&manifest).map_err(|e| e.to_string())?)
    }
    pub(super) fn native_uris(&self) -> Result<BTreeMap<String, String>, String> {
        self.native
            .iter()
            .map(|(key, view)| Ok((key.clone(), word(view, "uri")?.to_owned())))
            .collect()
    }

    /// Runtime IDs and terminal epochs may change; placement and native payloads must survive.
    pub(super) fn check_round_trip(&self, text: &str) -> Result<(), String> {
        let expected = parse(&self.text)?;
        let manifest = parse(text)?;
        if manifest.windows.len() != expected.windows.len() {
            return Err("baseline round-trip changed window count".into());
        }
        let mut sessions = BTreeSet::new();
        for (window, expected) in manifest.windows.iter().zip(&expected.windows) {
            if window.restored_tabs.len() != expected.restored_tabs.len()
                || window.active_item != expected.active_item
                || window.active_tab != expected.active_tab
                || ["rows", "cols"]
                    .iter()
                    .any(|field| window.other.get(*field) != expected.other.get(*field))
            {
                return Err("baseline round-trip changed window/tab placement".into());
            }
            for (tab, expected_tab) in window.restored_tabs.iter().zip(&expected.restored_tabs) {
                if tab.focused_path != expected_tab.focused_path {
                    return Err("baseline round-trip changed saved pane focus".into());
                }
                same_placement(&expected_tab.root, &tab.root)?;
                for view in tab_leaves(tab)? {
                    if word(view, "kind")? != "native" {
                        let sid = word(view, "sid")?;
                        if sid.is_empty() || !sessions.insert(sid) {
                            return Err(
                                "baseline round-trip duplicated/lost terminal identity".into()
                            );
                        }
                    }
                }
            }
        }
        if native_positions(&manifest)? != self.native {
            return Err("baseline round-trip changed native document/journal descriptors".into());
        }
        Ok(())
    }
}

fn same_placement(expected: &Value, actual: &Value) -> Result<(), String> {
    if word(expected, "node")? != word(actual, "node")? {
        return Err("baseline round-trip changed saved split shape".into());
    }
    if word(expected, "node")? == "split" {
        if expected.get("axis") != actual.get("axis")
            || expected.get("ratio") != actual.get("ratio")
        {
            return Err("baseline round-trip changed saved divider".into());
        }
        for side in ["first", "second"] {
            same_placement(
                expected.get(side).ok_or("expected split child missing")?,
                actual.get(side).ok_or("saved split child missing")?,
            )?;
        }
    } else if expected.get("view").and_then(|view| view.get("kind"))
        != actual.get("view").and_then(|view| view.get("kind"))
    {
        return Err("baseline round-trip changed saved leaf placement".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "restore_native_fixture_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The site's `firebase.json`, read as firebase-tools reads its `hosting` block
//! (`convertConfig`), and written as the REST API's `ServingConfig` for a new version.

use std::path::Path;

use aterm_json::{Map, Value};

/// The directory `ship site` writes: `hosting.public` must name it, or the deploy would
/// publish another tree than the one the sync committed.
pub const PUBLIC: &str = "public";

/// What firebase-tools ignores in every deploy besides `hosting.ignore` (its `listFiles`).
pub const ALWAYS_IGNORED: [&str; 3] = [
    "**/firebase-debug.log",
    "**/firebase-debug.*.log",
    ".firebase/*",
];

/// The `hosting` keys this deploy reads. Any other is refused by name, never dropped: a
/// key left out of the version would change what the live site serves.
const KEYS: [&str; 8] = [
    "public",
    "ignore",
    "site",
    "headers",
    "redirects",
    "rewrites",
    "cleanUrls",
    "trailingSlash",
];

/// The redirect statuses a version admits; firebase.json's `type` defaults to 301.
const REDIRECT_STATUSES: [i64; 5] = [301, 302, 303, 307, 308];

/// A site's hosting block, as one deploy uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hosting {
    /// The ignore patterns, relative to `public/`: `hosting.ignore`'s, then [`ALWAYS_IGNORED`].
    pub ignore: Vec<String>,
    /// `hosting.site`, when it names one.
    pub site: Option<String>,
    /// The version's `ServingConfig`: `headers`, `redirects`, `rewrites`, `cleanUrls` and
    /// `trailingSlashBehavior`, each only where firebase.json sets it.
    pub config: Value,
}

/// `site_dir/firebase.json`, read and translated.
/// # Errors
/// A file that cannot be read, or any refusal of [`parse`], naming the file.
pub fn read(site_dir: &Path) -> Result<Hosting, String> {
    let path = site_dir.join("firebase.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text).map_err(|why| format!("{}: {why}", path.display()))
}

/// `text`, a firebase.json, translated as firebase-tools translates its hosting block.
/// # Errors
/// JSON that does not parse; no `hosting` object, or a multi-site array; a `public` other than
/// [`PUBLIC`]; a key outside [`KEYS`]; or a rule this deploy cannot express, each by name.
pub fn parse(text: &str) -> Result<Hosting, String> {
    let doc: Value =
        aterm_json::from_str(text).map_err(|e| format!("firebase.json is not JSON: {e}"))?;
    let hosting = match doc.get("hosting") {
        Some(Value::Object(hosting)) => hosting,
        Some(Value::Array(_)) => {
            return Err("hosting is an array of sites: ship site deploys one site".into());
        }
        Some(_) => return Err("hosting is not an object".into()),
        None => return Err("firebase.json has no hosting block".into()),
    };
    if let Some(key) = hosting.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "hosting.{key} is not a key ship site deploys (it reads {}); a version made \
             without it would serve another site than firebase deploy would",
            KEYS.join(", ")
        ));
    }
    match hosting.get("public").and_then(Value::as_str) {
        Some(PUBLIC) => {}
        Some(other) => {
            return Err(format!(
                "hosting.public is {other:?}: ship site writes and deploys {PUBLIC:?}"
            ));
        }
        None => return Err(format!("hosting.public must be {PUBLIC:?}")),
    }
    let mut ignore = strings(hosting.get("ignore"), "hosting.ignore")?;
    ignore.extend(ALWAYS_IGNORED.iter().map(|p| (*p).to_string()));
    let site = match hosting.get("site") {
        None => None,
        Some(Value::String(site)) => Some(site.clone()),
        Some(_) => return Err("hosting.site is not a string".into()),
    };
    let mut config = Map::new();
    if let Some(rules) = hosting.get("headers") {
        config.insert("headers".into(), headers(rules)?);
    }
    if let Some(rules) = hosting.get("redirects") {
        config.insert("redirects".into(), redirects(rules)?);
    }
    if let Some(rules) = hosting.get("rewrites") {
        config.insert("rewrites".into(), rewrites(rules)?);
    }
    if let Some(clean) = hosting.get("cleanUrls") {
        let clean = clean
            .as_bool()
            .ok_or("hosting.cleanUrls is not true or false")?;
        config.insert("cleanUrls".into(), Value::Bool(clean));
    }
    if let Some(slash) = hosting.get("trailingSlash") {
        let behavior = match slash.as_bool() {
            Some(true) => "ADD",
            Some(false) => "REMOVE",
            None => return Err("hosting.trailingSlash is not true or false".into()),
        };
        config.insert(
            "trailingSlashBehavior".into(),
            Value::String(behavior.into()),
        );
    }
    Ok(Hosting {
        ignore,
        site,
        config: Value::Object(config),
    })
}

/// `value` as a list of strings (absent is none).
fn strings(value: Option<&Value>, at: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("{at} is not a list"))?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{at}[{i}] is not a string"))
        })
        .collect()
}

/// The rules of `value` (a list of objects), each with the keys `allowed` alone.
fn rules<'v>(
    value: &'v Value,
    at: &str,
    allowed: &[&str],
) -> Result<Vec<(String, &'v Map)>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{at} is not a list"))?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let here = format!("{at}[{i}]");
        let Value::Object(rule) = item else {
            return Err(format!("{here} is not an object"));
        };
        if let Some(key) = rule.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!(
                "{here}.{key} is not a key ship site deploys here (it reads {})",
                allowed.join(", ")
            ));
        }
        out.push((here, rule));
    }
    Ok(out)
}

/// A rule's pattern as the API names it: `source` or `glob` is `{"glob": …}`, `regex` is
/// `{"regex": …}` (firebase-tools' `extractPattern`); exactly one, never empty.
fn pattern(rule: &Map, at: &str) -> Result<Map, String> {
    let mut found: Vec<(&str, &str)> = Vec::new();
    for (key, kind) in [("source", "glob"), ("glob", "glob"), ("regex", "regex")] {
        match rule.get(key) {
            None => {}
            Some(Value::String(p)) if !p.is_empty() => found.push((kind, p)),
            Some(_) => return Err(format!("{at}.{key} is not a pattern")),
        }
    }
    match found.as_slice() {
        [(kind, p)] => {
            let mut out = Map::new();
            out.insert((*kind).into(), Value::String((*p).into()));
            Ok(out)
        }
        [] => Err(format!("{at} has no pattern (source, glob or regex)")),
        _ => Err(format!(
            "{at} names more than one pattern (source, glob, regex)"
        )),
    }
}

/// A rule's string field `key`, which must be there.
fn field<'r>(rule: &'r Map, key: &str, at: &str) -> Result<&'r str, String> {
    rule.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{at}.{key} is missing or not a string"))
}

/// `headers`: `[{pattern, headers: [{key, value}]}]` becomes `[{pattern, headers: {key: value}}]`,
/// in order, since a later rule's header overrides an earlier one's.
fn headers(value: &Value) -> Result<Value, String> {
    let mut out = Vec::new();
    for (at, rule) in rules(
        value,
        "hosting.headers",
        &["source", "glob", "regex", "headers"],
    )? {
        let mut entry = pattern(rule, &at)?;
        let pairs = rule
            .get("headers")
            .and_then(Value::as_array)
            .filter(|pairs| !pairs.is_empty())
            .ok_or_else(|| format!("{at}.headers is missing or empty"))?;
        let mut set = Map::new();
        for (j, pair) in pairs.iter().enumerate() {
            let here = format!("{at}.headers[{j}]");
            let Value::Object(pair) = pair else {
                return Err(format!("{here} is not an object"));
            };
            if let Some(key) = pair.keys().find(|k| *k != "key" && *k != "value") {
                return Err(format!("{here}.{key} is not key or value"));
            }
            let name = field(pair, "key", &here)?;
            let text = pair
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{here}.value is missing or not a string"))?;
            if set
                .insert(name.to_string(), Value::String(text.into()))
                .is_some()
            {
                return Err(format!("{at} sets the header {name} twice"));
            }
        }
        entry.insert("headers".into(), Value::Object(set));
        out.push(Value::Object(entry));
    }
    Ok(Value::Array(out))
}

/// `redirects`: `[{pattern, destination, type?}]` becomes `[{pattern, location, statusCode}]`.
fn redirects(value: &Value) -> Result<Value, String> {
    let allowed = ["source", "glob", "regex", "destination", "type"];
    let mut out = Vec::new();
    for (at, rule) in rules(value, "hosting.redirects", &allowed)? {
        let mut entry = pattern(rule, &at)?;
        let location = field(rule, "destination", &at)?;
        let status = match rule.get("type") {
            None => 301,
            Some(t) => t
                .as_i64()
                .filter(|s| REDIRECT_STATUSES.contains(s))
                .ok_or_else(|| format!("{at}.type is not one of {REDIRECT_STATUSES:?}"))?,
        };
        entry.insert("location".into(), Value::String(location.into()));
        entry.insert("statusCode".into(), Value::from(status));
        out.push(Value::Object(entry));
    }
    Ok(Value::Array(out))
}

/// `rewrites`: `[{pattern, destination}]` becomes `[{pattern, path}]`. A rewrite to a function,
/// a Cloud Run service or Dynamic Links needs a deploy of its own, so it is refused by name.
fn rewrites(value: &Value) -> Result<Value, String> {
    let items = value.as_array().ok_or("hosting.rewrites is not a list")?;
    for (i, item) in items.iter().enumerate() {
        for target in ["function", "run", "dynamicLinks", "tag"] {
            if item.get(target).is_some() {
                return Err(format!(
                    "hosting.rewrites[{i}] rewrites to {target}: ship site deploys static \
                     files and path rewrites only"
                ));
            }
        }
    }
    let mut out = Vec::new();
    for (at, rule) in rules(
        value,
        "hosting.rewrites",
        &["source", "glob", "regex", "destination"],
    )? {
        let mut entry = pattern(rule, &at)?;
        let path = field(rule, "destination", &at)?;
        entry.insert("path".into(), Value::String(path.into()));
        out.push(Value::Object(entry));
    }
    Ok(Value::Array(out))
}

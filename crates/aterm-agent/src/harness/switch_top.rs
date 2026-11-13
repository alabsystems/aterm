// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The model catalog baked into a Claude Code build: its newest model of each family and the
//! efforts each model takes, cached under `<state>/upgrade/builds` by the upgrade sweep. A
//! session's model card and effort slider read it ([`catalog`]). Nothing here chooses a model:
//! a relaunch keeps the session's own `--model`.

use std::io::Read as _;
use std::path::Path;

use super::upgrade::is_model_id;

/// Whether `s` is a three-part dotted version (`2.1.281`), each part 1-6 digits without leading zeros.
#[must_use]
pub fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 6
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

/// `x.y.z` as a comparable triple.
#[must_use]
pub fn version_triple(s: &str) -> Option<(u64, u64, u64)> {
    if !is_version(s) {
        return None;
    }
    let mut it = s.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// One model entry of the baked catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedModel {
    /// Model identifier (`claude-opus-5-5`).
    pub id: String,
    /// Model family (`opus`).
    pub family: String,
    /// Display name (`Opus 5.5`).
    pub display_name: String,
    /// Whether the model has a native 1M context window without a `[1m]` suffix.
    pub native_1m: bool,
    /// Effort levels admitted by the model's `capabilities`.
    pub efforts: Vec<String>,
}

/// What one build's catalog says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Baked {
    /// `latest_per_family`, in the order written.
    pub latest_per_family: Vec<(String, String)>,
    /// Every model entry found.
    pub models: Vec<BakedModel>,
}

impl Baked {
    /// The entry for `id` (the `[1m]` suffix ignored).
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&BakedModel> {
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        self.models.iter().find(|m| m.id == base)
    }

    /// The build's newest model of `family`.
    #[must_use]
    pub fn latest(&self, family: &str) -> Option<&str> {
        self.latest_per_family
            .iter()
            .find(|(f, _)| f == family)
            .map(|(_, id)| id.as_str())
    }

    /// The entry of the build's newest model of `family`: `None` where the catalog names no
    /// newest of it, or lists no entry for the one it names.
    #[must_use]
    pub fn latest_of(&self, family: &str) -> Option<&BakedModel> {
        self.model(self.latest(family)?)
    }
}

const MARKER: &[u8] = b"latest_per_family:{";
const WINDOW_BEFORE: usize = 512 * 1024;
const CHUNK: usize = 8 * 1024 * 1024;

fn parse_pairs(s: &str) -> Vec<(String, String)> {
    let body = s.split('}').next().unwrap_or("");
    body.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            let k = k.trim().trim_matches('"');
            let v = v.trim().strip_prefix('"')?.strip_suffix('"')?;
            (!k.is_empty()
                && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                && is_model_id(v))
            .then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn field<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let at = entry.find(&format!("{key}:\""))? + key.len() + 2;
    let rest = entry.get(at..)?;
    rest.get(..rest.find('"')?)
}

/// Parse the catalog out of `text`, a window that ends with the `latest_per_family` object.
#[must_use]
pub fn parse_window(text: &str) -> Option<Baked> {
    let at = text.rfind("latest_per_family:{")?;
    let latest = parse_pairs(text.get(at + MARKER.len()..)?);
    if latest.is_empty() {
        return None;
    }
    let mut models: Vec<BakedModel> = Vec::new();
    for (i, _) in text.get(..at)?.match_indices("{id:\"claude-") {
        let entry = text.get(i..at)?;
        let entry = entry
            .get(1..)
            .and_then(|e| e.find("{id:\"claude-").map(|n| &entry[..=n]))
            .unwrap_or(entry);
        let Some(id) = field(entry, "id").filter(|id| is_model_id(id)) else {
            continue;
        };
        let Some(family) = field(entry, "family")
            .filter(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_lowercase()))
        else {
            continue;
        };
        let display_name = field(entry, "display_name").unwrap_or(id).to_string();
        let native_1m = entry.contains("native_1m:!0") || entry.contains("native_1m:true");
        let caps = entry
            .find("capabilities:[")
            .and_then(|c| entry.get(c + "capabilities:[".len()..))
            .and_then(|c| c.split(']').next())
            .unwrap_or("");
        let has = |cap: &str| caps.contains(&format!("\"{cap}\""));
        let mut efforts = Vec::new();
        if has("effort") {
            efforts.extend(["low", "medium", "high"].map(str::to_string));
        }
        if has("xhigh_effort") {
            efforts.push("xhigh".to_string());
        }
        if has("max_effort") {
            efforts.push("max".to_string());
        }
        if models.iter().any(|m| m.id == id) {
            continue;
        }
        models.push(BakedModel {
            id: id.to_string(),
            family: family.to_string(),
            display_name,
            native_1m,
            efforts,
        });
    }
    Some(Baked {
        latest_per_family: latest,
        models,
    })
}

/// What one pass over a build's binary finds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BuildFacts {
    /// The baked catalog, when the marker was found.
    pub baked: Option<Baked>,
    /// Most frequent `VERSION:"x.y.z"` literal seen at least twice in the binary.
    pub version: Option<String>,
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let off = hay.get(i..)?.iter().position(|&b| b == first)?;
        i += off;
        if hay.get(i..i + needle.len())? == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Scans a Claude Code binary once in bounded chunks for its catalog and version.
#[must_use]
pub fn scan_build(binary: &Path) -> BuildFacts {
    const VNEEDLE: &[u8] = b"VERSION:\"";
    let Ok((mut f, _)) = super::disk::versions::open_regular_file(binary) else {
        return BuildFacts::default();
    };
    let mut out = BuildFacts::default();
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    while let Ok(n) = f.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let mut hay = std::mem::take(&mut carry);
        let carried = hay.len();
        hay.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        if out.baked.is_none()
            && let Some(pos) = find_from(&hay, MARKER, carried.saturating_sub(MARKER.len()))
        {
            let mut end = (pos + 4096).min(hay.len());
            if end == hay.len() {
                let mut more = vec![0u8; 4096];
                if let Ok(m) = f.read(&mut more) {
                    hay.extend_from_slice(more.get(..m).unwrap_or(&[]));
                    end = (pos + 4096).min(hay.len());
                }
            }
            let start = pos.saturating_sub(WINDOW_BEFORE);
            let text = String::from_utf8_lossy(hay.get(start..end).unwrap_or(&[]));
            out.baked = parse_window(&text);
        }
        let mut from = carried.saturating_sub(VNEEDLE.len() + 24);
        while let Some(at) = find_from(&hay, VNEEDLE, from) {
            from = at + 1;
            let v0 = at + VNEEDLE.len();
            let tail = hay.get(v0..(v0 + 24).min(hay.len())).unwrap_or(&[]);
            if let Some(end) = tail.iter().position(|&b| b == b'"')
                && v0 + end >= carried
                && let Some(v) = tail.get(..end).and_then(|t| std::str::from_utf8(t).ok())
                && is_version(v)
            {
                *counts.entry(v.to_string()).or_default() += 1;
            }
        }
        let keep = hay.len().saturating_sub(WINDOW_BEFORE + MARKER.len());
        carry = hay.split_off(keep);
    }
    out.version = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .max_by_key(|(_, n)| *n)
        .map(|(v, _)| v);
    out
}

/// The catalog alone (tests, one-off reads).
#[must_use]
pub fn scan(binary: &Path) -> Option<Baked> {
    scan_build(binary).baked
}

/// A [`BuildFacts`] as JSON (the on-disk cache).
#[must_use]
pub fn render_facts(b: &BuildFacts) -> String {
    use aterm_json::{Map, Value};
    let mut root = Map::new();
    root.insert(
        "version".into(),
        b.version.clone().map_or(Value::Null, Value::String),
    );
    if let Some(k) = &b.baked {
        root.insert(
            "latest_per_family".into(),
            Value::Array(
                k.latest_per_family
                    .iter()
                    .map(|(f, i)| {
                        Value::Array(vec![Value::String(f.clone()), Value::String(i.clone())])
                    })
                    .collect(),
            ),
        );
        root.insert(
            "models".into(),
            Value::Array(
                k.models
                    .iter()
                    .map(|m| {
                        let mut o = Map::new();
                        o.insert("id".into(), Value::String(m.id.clone()));
                        o.insert("family".into(), Value::String(m.family.clone()));
                        o.insert("display_name".into(), Value::String(m.display_name.clone()));
                        o.insert("native_1m".into(), Value::Bool(m.native_1m));
                        o.insert(
                            "efforts".into(),
                            Value::Array(m.efforts.iter().cloned().map(Value::String).collect()),
                        );
                        Value::Object(o)
                    })
                    .collect(),
            ),
        );
    }
    aterm_json::to_string(&Value::Object(root)).unwrap_or_default()
}

/// Parse [`render_facts`]'s output; every id re-validated.
#[must_use]
pub fn parse_facts(text: &str) -> Option<BuildFacts> {
    use aterm_json::Value;
    let v: Value = aterm_json::from_str(text).ok()?;
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .filter(|s| is_version(s))
        .map(str::to_string);
    let baked = match (
        v.get("latest_per_family").and_then(Value::as_array),
        v.get("models").and_then(Value::as_array),
    ) {
        (Some(l), Some(m)) => Some(Baked {
            latest_per_family: l
                .iter()
                .filter_map(|p| {
                    let a = p.as_array()?;
                    let id = a.get(1)?.as_str()?;
                    let fam = a.first()?.as_str()?;
                    (!fam.is_empty()
                        && fam.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                        && is_model_id(id))
                    .then(|| (fam.to_string(), id.to_string()))
                })
                .collect(),
            models: m
                .iter()
                .filter_map(|o| {
                    let id = o.get("id")?.as_str()?;
                    if !is_model_id(id) {
                        return None;
                    }
                    let family = o
                        .get("family")?
                        .as_str()
                        .filter(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_lowercase()))?;
                    let display_name = o.get("display_name")?.as_str().filter(|d| {
                        !d.is_empty()
                            && !d
                                .chars()
                                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                    })?;
                    Some(BakedModel {
                        id: id.to_string(),
                        family: family.to_string(),
                        display_name: display_name.to_string(),
                        native_1m: o.get("native_1m")?.as_bool()?,
                        efforts: o
                            .get("efforts")?
                            .as_array()?
                            .iter()
                            .filter_map(|e| e.as_str().map(str::to_string))
                            .collect(),
                    })
                })
                .collect(),
        }),
        _ => None,
    };
    Some(BuildFacts { baked, version })
}

/// Returns the baked catalog of `binary`, cached under `cache_dir` by path, size, and mtime.
/// A `cache_dir` that does not stand yet caches nothing: the binary is scanned and its catalog
/// returned.
#[must_use]
pub fn baked_cached(binary: &Path, cache_dir: &Path) -> Option<Baked> {
    let m = std::fs::metadata(binary).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let key = format!("{}|{}|{mtime}", binary.display(), m.len());
    let h = aterm_hash::fnv1a_64(key.as_bytes());
    if let Some(parent) = cache_dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        if parent.file_name().is_some_and(|n| n == "upgrade")
            && let Some(state) = parent.parent().filter(|p| !p.as_os_str().is_empty())
            && super::upgrade_drive::check_owned_plain_dir_existing(state).is_err()
        {
            return None;
        }
        if super::upgrade_drive::check_owned_plain_dir_existing(parent).is_err() {
            return None;
        }
    }
    let existing = match super::upgrade_drive::check_owned_plain_dir_existing(cache_dir) {
        Ok(existing) => existing,
        Err(_) => return None,
    };
    let file = cache_dir.join(format!("{h:016x}.json"));
    if existing
        && let Some(facts) = super::accounts::read_bounded(&file, 1024 * 1024)
            .ok()
            .flatten()
            .and_then(|t| parse_facts(&t))
    {
        return facts.baked;
    }
    let facts = scan_build(binary);
    if existing {
        let _ = super::atomic_write::write_atomic_ordered(&file, &render_facts(&facts));
    }
    facts.baked
}

/// The cached catalog of the build that says `version`, among [`baked_cached`]'s files under
/// `cache_dir`: read, never written or scanned for. `None` where no cached build says it.
#[must_use]
pub fn cached_for_version(cache_dir: &Path, version: &str) -> Option<Baked> {
    /// The most cache files looked at: the folder holds one per build seen.
    const FILES_MAX: usize = 256;
    std::fs::read_dir(cache_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .take(FILES_MAX)
        .filter_map(|path| {
            super::accounts::read_bounded(&path, 1024 * 1024)
                .ok()
                .flatten()
        })
        .filter_map(|text| parse_facts(&text))
        .find(|facts| facts.version.as_deref() == Some(version) && facts.baked.is_some())
        .and_then(|facts| facts.baked)
}

/// The cached catalog of the build a session on Claude Code `version` runs, under the harness's
/// `state` ([`cached_for_version`] of `<state>/upgrade/builds`): `None` where no sweep has cached
/// the build, or `version` is not known. It reads, and writes nothing.
#[must_use]
pub fn catalog(state: &Path, version: Option<&str>) -> Option<Baked> {
    let dir = super::upgrade_drive::check_upgrade_tree(state, Some("builds"))
        .ok()
        .flatten()?;
    cached_for_version(&dir, version?)
}

#[cfg(test)]
#[path = "switch_top_tests.rs"]
mod tests;

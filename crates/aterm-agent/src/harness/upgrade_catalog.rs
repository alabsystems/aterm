// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAKED CATALOG inside a Claude Code build
//! (`docs/DESIGN-claude-live-upgrade-models-2026-09-24.md`).
//!
//! Every 2.1.28x build carries a hand-maintained model catalog as a JavaScript
//! object literal (READ, strings L312348): one `{id:"claude-…",family:"…",
//! display_name:"…",…,context:{window:…,native_1m:!0,…},…,capabilities:[…]}`
//! per model and, after them, `latest_per_family:{fable:"claude-fable-5-1",
//! opus:"claude-opus-5-5",…}` — the build's own statement of the newest model
//! of each family. That literal is how a NEW build recommends a new model
//! without any network call, and the model entries are how the harness knows
//! whether a model id is one the build understands (a full id the build does
//! not know runs with no capabilities, default pricing and a 200k window).
//!
//! The scan reads the binary in bounded chunks (a 217 MB file is never held
//! whole), finds `latest_per_family:{`, and parses the entries in the window
//! before it. It runs once per build: the result is cached by the caller,
//! keyed on the build's path, size and mtime.

use std::io::Read as _;
use std::path::Path;

use super::upgrade::is_model_id;

/// Whether `s` is a three-part dotted version (`2.1.281`), each part 1-6 digits.
#[must_use]
pub fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
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
    /// `claude-opus-5-5`.
    pub id: String,
    /// `opus`.
    pub family: String,
    /// `Opus 5.5`.
    pub display_name: String,
    /// `context.native_1m` — a 1M window with no `[1m]` suffix.
    pub native_1m: bool,
    /// The effort levels its `capabilities` admit (`low`,`medium`,`high`
    /// from `effort`; `xhigh` from `xhigh_effort`; `max` from `max_effort`).
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
}

const MARKER: &[u8] = b"latest_per_family:{";
/// How far before the marker the model entries may start.
const WINDOW_BEFORE: usize = 512 * 1024;
/// Chunk size for the bounded read.
const CHUNK: usize = 8 * 1024 * 1024;

/// `key:"value"` pairs up to the first `}`.
fn parse_pairs(s: &str) -> Vec<(String, String)> {
    let body = s.split('}').next().unwrap_or("");
    body.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            let k = k.trim().trim_matches('"');
            let v = v.trim().strip_prefix('"')?.strip_suffix('"')?;
            (k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') && is_model_id(v))
                .then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// A quoted string value after `key:"` inside `entry`.
fn field<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let at = entry.find(&format!("{key}:\""))? + key.len() + 2;
    let rest = entry.get(at..)?;
    rest.get(..rest.find('"')?)
}

/// Parse the catalog out of the text around the marker. `text` is the window
/// that ENDS with the `latest_per_family` object.
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
        // An entry ends where the next one begins.
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
    /// The version the build reports: the MOST FREQUENT `VERSION:"x.y.z"`
    /// literal, seen at least twice — the bundle's build-info object appears
    /// 264 times in 2.1.280 and a bundled dependency's `VERSION:"2.0.0"`
    /// once (MEASURED).
    pub version: Option<String>,
}

/// First index of `needle` in `hay`, first-byte filtered.
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

/// Scan a Claude Code binary ONCE for its catalog and its version, reading
/// in bounded chunks (a 217 MB file is never held whole). The result is
/// cached by the caller per (path, size, mtime), in memory and on disk, so a
/// build is read once in its life.
#[must_use]
pub fn scan_build(binary: &Path) -> BuildFacts {
    const VNEEDLE: &[u8] = b"VERSION:\"";
    let Ok(mut f) = std::fs::File::open(binary) else {
        return BuildFacts::default();
    };
    let mut out = BuildFacts::default();
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    // Keep the last WINDOW_BEFORE bytes so a marker is seen with its entries.
    let mut carry: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    while let Ok(n) = f.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let mut hay = std::mem::take(&mut carry);
        let carried = hay.len();
        hay.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        // Version literals that START in the new bytes (older ones were
        // counted with the previous chunk).
        let mut from = carried.saturating_sub(VNEEDLE.len() + 24);
        while let Some(at) = find_from(&hay, VNEEDLE, from) {
            from = at + 1;
            if at + VNEEDLE.len() + 24 <= carried {
                continue;
            }
            let v0 = at + VNEEDLE.len();
            let tail = hay.get(v0..(v0 + 24).min(hay.len())).unwrap_or(&[]);
            if let Some(end) = tail.iter().position(|&b| b == b'"')
                && let Some(v) = tail.get(..end).and_then(|t| std::str::from_utf8(t).ok())
                && is_version(v)
            {
                *counts.entry(v.to_string()).or_default() += 1;
            }
        }
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
                    let fam = a.first()?.as_str()?.to_string();
                    is_model_id(id).then(|| (fam, id.to_string()))
                })
                .collect(),
            models: m
                .iter()
                .filter_map(|o| {
                    let id = o.get("id")?.as_str()?;
                    if !is_model_id(id) {
                        return None;
                    }
                    Some(BakedModel {
                        id: id.to_string(),
                        family: o.get("family")?.as_str()?.to_string(),
                        display_name: o.get("display_name")?.as_str()?.to_string(),
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

/// The binary a managed twin (`<prefix>/agents/claude`, atpkg's POSIX shim)
/// executes: the path of its last `exec '<path>' "$@"` line.
#[must_use]
pub fn twin_binary(twin_text: &str) -> Option<std::path::PathBuf> {
    twin_text.lines().rev().find_map(|l| {
        let rest = l.trim().strip_prefix("exec '")?;
        let (path, tail) = rest.split_once('\'')?;
        (tail.trim() == "\"$@\"" && path.starts_with('/')).then(|| std::path::PathBuf::from(path))
    })
}

/// The baked catalog of `binary`, scanned ONCE per build in its life: cached on
/// disk under `cache_dir`, keyed by path, size and mtime (the sweep runs every
/// minute; a 217 MB scan must not).
#[must_use]
pub fn baked_cached(binary: &Path, cache_dir: &Path) -> Option<Baked> {
    let m = std::fs::metadata(binary).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    // FNV-1a over the key: a file name, not a secret.
    let key = format!("{}|{}|{mtime}", binary.display(), m.len());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let file = cache_dir.join(format!("{h:016x}.json"));
    if let Some(facts) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| parse_facts(&t))
    {
        return facts.baked;
    }
    let facts = scan_build(binary);
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = super::upgrade_models::write_atomic(&file, &render_facts(&facts));
    facts.baked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed copy of the 2.1.280 literal (strings L312348), two entries.
    const WINDOW: &str = concat!(
        r#"models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5",knowledge_cutoff:"May 2026","#,
        r#"context:{window:2e5,native_1m:!1,supports_1m_beta:!0},capabilities:["effort","max_effort","adaptive_thinking"],default_effort:"high"},"#,
        r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5",knowledge_cutoff:"June 2026","#,
        r#"context:{window:1e6,native_1m:!0,supports_1m_beta:!0,supports_1m_suffix:!0},capabilities:["effort","max_effort","xhigh_effort","adaptive_thinking"],default_effort:"medium",advisor_rank:4}],"#,
        r#"aliases:{opus:{default:"claude-opus-5-5"}},defaults:{},best:"fable","#,
        r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5",sonnet:"claude-sonnet-5",haiku:"claude-haiku-4-5"},alias_migration:{}};"#,
    );

    #[test]
    fn the_280_literal_parses() {
        let b = parse_window(WINDOW).expect("parses");
        assert_eq!(b.latest("opus"), Some("claude-opus-5-5"));
        assert_eq!(b.latest("fable"), Some("claude-fable-5-1"));
        assert_eq!(b.latest("mythos"), None);
        let o55 = b.model("claude-opus-5-5").expect("entry");
        assert_eq!(o55.family, "opus");
        assert_eq!(o55.display_name, "Opus 5.5");
        assert!(o55.native_1m);
        assert_eq!(o55.efforts, ["low", "medium", "high", "xhigh", "max"]);
        let o5 = b.model("claude-opus-5[1m]").expect("the suffix is ignored");
        assert!(!o5.native_1m);
        assert!(!o5.efforts.contains(&"xhigh".to_string()));
    }

    #[test]
    fn a_hostile_value_is_not_a_model() {
        let w = WINDOW.replace(
            r#"opus:"claude-opus-5-5",sonnet"#,
            r#"opus:"claude-opus-5-5;rm -rf ~",sonnet"#,
        );
        assert_ne!(w, WINDOW, "the fixture must contain the replaced text");
        let b = parse_window(&w).expect("the rest still parses");
        assert_eq!(b.latest("opus"), None);
        assert_eq!(parse_window("no marker here"), None);
    }

    #[test]
    fn build_facts_round_trip_through_the_disk_cache() {
        let b = BuildFacts {
            baked: parse_window(WINDOW),
            version: Some("2.1.280".into()),
        };
        assert_eq!(parse_facts(&render_facts(&b)), Some(b));
        assert_eq!(parse_facts("{}"), Some(BuildFacts::default()));
    }

    #[test]
    fn the_scan_finds_a_marker_across_a_chunk_boundary() {
        let dir =
            std::env::temp_dir().join(format!("aterm-upgrade-catalog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("claude");
        let mut bytes = vec![b'x'; CHUNK - 100];
        bytes.extend_from_slice(WINDOW.as_bytes());
        bytes.extend(std::iter::repeat_n(b'y', 1000));
        bytes.extend_from_slice(b"VERSION:\"2.0.0\" VERSION:\"2.1.281\" VERSION:\"2.1.281\"");
        std::fs::write(&path, &bytes).expect("write");
        let f = scan_build(&path);
        let b = f.baked.expect("found");
        assert_eq!(b.latest("opus"), Some("claude-opus-5-5"));
        assert_eq!(b.models.len(), 2);
        assert_eq!(
            f.version.as_deref(),
            Some("2.1.281"),
            "the most frequent literal wins"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_twin_names_its_binary_on_its_last_exec_line() {
        let twin = "#!/bin/sh\ncase \"$1\" in\n  update) exec '/x/atpkg' __selfupdate;;\nesac\nexport DISABLE_AUTOUPDATER='1'\nexec '/P/store/claude/1000002000001000282/bin/claude' \"$@\"\n";
        assert_eq!(
            twin_binary(twin),
            Some(std::path::PathBuf::from(
                "/P/store/claude/1000002000001000282/bin/claude"
            ))
        );
        assert_eq!(twin_binary("exec 'relative/claude' \"$@\""), None);
        assert_eq!(twin_binary("#!/bin/sh\n"), None);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a person's click on a Claude Code session's model fact switches it: the model order
//! (`<state>/upgrade/models.json`'s `priority`, else [`SEED`]) and the catalog baked into the
//! session's build, cached under `<state>/upgrade/builds` by the upgrade sweep. Nothing here
//! chooses a model for a relaunch: a relaunch keeps the session's own `--model`.

use std::io::Read as _;
use std::path::Path;

use super::claude_model::{base_id, family_version};
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

/// The click's model order where `models.json` names none, best first.
pub const SEED: [&str; 3] = ["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5"];

/// The most models an order holds.
const ORDER_MAX: usize = 32;

/// The effort levels `/effort` takes, least first.
pub const EFFORT_LADDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The effort taken for a model's top where no read catalog lists the model.
pub const TOP_EFFORT_UNREAD: &str = "xhigh";

/// The model families the automatic choice passes over while another stands: billed to usage
/// credits where the account's billing is not read. A `[claude_footer] switch_model` setting
/// can still name one.
pub const CREDITS_BILLED_UNREAD: &[&str] = &["fable"];

/// [`SEED`] as an order.
#[must_use]
pub fn seed() -> Vec<String> {
    SEED.iter().map(|s| (*s).to_string()).collect()
}

/// The order a `models.json` text names in its `priority` array: `None` where it is not JSON,
/// names no model, more than [`ORDER_MAX`], a word that is no model id, or one twice.
#[must_use]
pub fn parse_order(text: &str) -> Option<Vec<String>> {
    let v: aterm_json::Value = aterm_json::from_str(text).ok()?;
    let ids: Vec<String> = v
        .get("priority")?
        .as_array()?
        .iter()
        .map(|x| {
            x.as_str()
                .filter(|s| family_version(s).is_some())
                .map(str::to_string)
        })
        .collect::<Option<_>>()?;
    let mut dedup = ids.clone();
    dedup.sort();
    dedup.dedup();
    (!ids.is_empty() && ids.len() <= ORDER_MAX && dedup.len() == ids.len()).then_some(ids)
}

/// The highest effort `baked` admits for `model` ([`EFFORT_LADDER`]), [`TOP_EFFORT_UNREAD`]
/// where no catalog was read or it lists neither the model nor an effort for it.
#[must_use]
pub fn top_effort(model: &str, baked: Option<&Baked>) -> &'static str {
    baked
        .and_then(|b| b.model(model))
        .and_then(|m| {
            EFFORT_LADDER
                .iter()
                .rev()
                .find(|level| m.efforts.iter().any(|e| e == **level))
                .copied()
        })
        .unwrap_or(TOP_EFFORT_UNREAD)
}

/// Where a person's switch "to the top" goes on one Claude Code build: the best model of the
/// order the build offers, at that model's highest effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Top {
    /// The model id (`claude-opus-5-5`).
    pub model: String,
    /// Its highest effort ([`top_effort`]).
    pub effort: &'static str,
    /// The build's own catalog, where one was read: what [`Self::effort_of`] another model reads.
    pub baked: Option<Baked>,
}

impl Top {
    /// The highest effort of `model` on this build ([`top_effort`]).
    #[must_use]
    pub fn effort_of(&self, model: &str) -> &'static str {
        top_effort(model, self.baked.as_ref())
    }
}

fn passed_over(id: &str) -> bool {
    family_version(id).is_some_and(|(family, _)| CREDITS_BILLED_UNREAD.contains(&family.as_str()))
}

/// [`Top`] of `order` on the build whose catalog is `baked`. With a catalog: the order's first
/// model the build lists, else the build's own newest of a family (in the order its catalog
/// writes them). With none: the order's first. A family of [`CREDITS_BILLED_UNREAD`] is passed
/// over while another stands; an empty order is [`SEED`]'s first.
#[must_use]
pub fn top(order: &[String], baked: Option<Baked>) -> Top {
    let on_offer: Vec<&str> = baked.as_ref().map_or_else(Vec::new, |b| {
        let mut out: Vec<&str> = Vec::new();
        let latest = b.latest_per_family.iter().map(|(_, id)| id.as_str());
        for id in latest.chain(order.iter().map(String::as_str)) {
            let base = base_id(id);
            if family_version(base).is_some()
                && !passed_over(base)
                && b.model(base).is_some()
                && !out.contains(&base)
            {
                out.push(base);
            }
        }
        out
    });
    let model = order
        .iter()
        .map(|id| base_id(id))
        .find(|id| on_offer.contains(id))
        .or_else(|| on_offer.first().copied())
        .or_else(|| order.iter().map(String::as_str).find(|id| !passed_over(id)))
        .or_else(|| order.first().map(String::as_str))
        .unwrap_or(SEED[0])
        .to_string();
    Top {
        effort: top_effort(&model, baked.as_ref()),
        model,
        baked,
    }
}

/// Where a person's switch "to the top" goes for a session on Claude Code `version`
/// ([`top`]): of the order under `state` ([`SEED`] where none is written) and that build's
/// cached catalog (`None` where no sweep has cached the build, or `version` is not known). It
/// reads, and writes nothing.
#[must_use]
pub fn switch_top(state: &Path, version: Option<&str>) -> Top {
    use super::upgrade_drive::check_upgrade_tree;
    let order = check_upgrade_tree(state, None)
        .ok()
        .flatten()
        .and_then(|dir| {
            super::accounts::read_bounded(&dir.join("models.json"), 1024 * 1024)
                .ok()
                .flatten()
        })
        .and_then(|text| parse_order(&text))
        .unwrap_or_else(seed);
    let baked = version.and_then(|version| {
        check_upgrade_tree(state, Some("builds"))
            .ok()
            .flatten()
            .and_then(|dir| cached_for_version(&dir, version))
    });
    top(&order, baked)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for bad in ["02.1.280", "2.01.280", "2.1.0280", "0.0.00"] {
            assert!(!is_version(bad), "{bad}");
            assert_eq!(version_triple(bad), None, "{bad}");
            assert_eq!(
                parse_facts(&format!(r#"{{"version":"{bad}"}}"#)).and_then(|facts| facts.version),
                None,
                "{bad}"
            );
        }
        assert_eq!(version_triple("0.0.0"), Some((0, 0, 0)));
    }

    #[test]
    fn a_cached_catalog_is_found_by_the_version_its_build_says() {
        let dir = crate::supervise::ScratchDir::new("upgrade-catalog", "by-version");
        let facts = |version: &str, id: &str| BuildFacts {
            version: Some(version.to_string()),
            baked: Some(Baked {
                latest_per_family: vec![("opus".to_string(), id.to_string())],
                models: vec![BakedModel {
                    id: id.to_string(),
                    family: "opus".to_string(),
                    display_name: "Opus".to_string(),
                    native_1m: false,
                    efforts: vec!["high".to_string(), "max".to_string()],
                }],
            }),
        };
        for (name, f) in [
            ("a.json", facts("2.1.300", "claude-opus-5")),
            ("b.json", facts("2.1.321", "claude-opus-5-5")),
        ] {
            std::fs::write(dir.join(name), render_facts(&f)).expect("write");
        }
        std::fs::write(dir.join("c.json"), "not json").expect("write");
        let found = cached_for_version(&dir, "2.1.321").expect("the build's catalog");
        assert_eq!(found.latest("opus"), Some("claude-opus-5-5"));
        assert_eq!(cached_for_version(&dir, "2.1.999"), None);
        assert_eq!(cached_for_version(&dir.join("absent"), "2.1.321"), None);
    }

    #[test]
    fn the_scan_finds_a_marker_across_a_chunk_boundary() {
        const BOUNDARY_SINGLE: &[u8] = b"VERSION:\"9.9.9\"";
        let dir = crate::supervise::ScratchDir::new("upgrade-catalog", "boundary");
        let path = dir.join("claude");
        let mut bytes = vec![b'x'; CHUNK - 15 - BOUNDARY_SINGLE.len()];
        bytes.extend_from_slice(BOUNDARY_SINGLE);
        bytes.extend_from_slice(WINDOW.as_bytes());
        bytes.extend(std::iter::repeat_n(b'y', 1000));
        bytes.extend_from_slice(b"VERSION:\"2.1.281\" VERSION:\"2.1.281\"");
        std::fs::write(&path, &bytes).expect("write");
        let f = scan_build(&path);
        let b = f.baked.expect("found");
        assert_eq!(b.latest("opus"), Some("claude-opus-5-5"));
        assert_eq!(b.models.len(), 2);
        assert_eq!(
            f.version.as_deref(),
            Some("2.1.281"),
            "the most frequent literal wins without double-counting carry"
        );

        let mut near_end = vec![b'x'; CHUNK - WINDOW.len()];
        near_end.extend_from_slice(WINDOW.as_bytes());
        assert_eq!(near_end.len(), CHUNK);
        near_end.extend_from_slice(b"VERSION:\"2.1.281\" VERSION:\"2.1.281\"");
        std::fs::write(&path, &near_end).expect("write");
        let f2 = scan_build(&path);
        assert_eq!(
            f2.baked.as_ref().and_then(|b| b.latest("opus")),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            f2.version.as_deref(),
            Some("2.1.281"),
            "a marker near chunk end still scans version literals in the lookahead window"
        );
    }

    fn baked(ids: &[&str], latest: &[(&str, &str)]) -> Baked {
        Baked {
            latest_per_family: latest
                .iter()
                .map(|(f, i)| ((*f).to_string(), (*i).to_string()))
                .collect(),
            models: ids
                .iter()
                .map(|id| BakedModel {
                    id: (*id).to_string(),
                    family: family_version(id).expect("id").0,
                    display_name: (*id).to_string(),
                    native_1m: true,
                    efforts: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn the_top_is_the_orders_best_the_build_offers_at_its_highest_effort() {
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let order = ids(&["claude-opus-6", "claude-opus-5-5", "claude-opus-5"]);
        let none = top(&order, None);
        assert_eq!(
            (none.model.as_str(), none.effort),
            ("claude-opus-6", TOP_EFFORT_UNREAD),
            "no catalog read: the order's first, at the unread top"
        );
        let mut b = baked(
            &["claude-opus-5-5", "claude-opus-5", "claude-sonnet-5"],
            &[("opus", "claude-opus-5-5"), ("sonnet", "claude-sonnet-5")],
        );
        b.models[0].efforts = ids(&["low", "medium", "high", "xhigh", "max"]);
        b.models[1].efforts = ids(&["low", "medium", "high"]);
        let old = top(&order, Some(b.clone()));
        assert_eq!(
            (old.model.as_str(), old.effort),
            ("claude-opus-5-5", "max"),
            "a build that does not know the order's first goes to the next it offers"
        );
        assert_eq!(old.effort_of("claude-opus-5"), "high");
        assert_eq!(
            old.effort_of("claude-sonnet-5"),
            TOP_EFFORT_UNREAD,
            "a model with no effort listed"
        );
        assert_eq!(
            top(&ids(&["claude-opus-7"]), Some(b)).model,
            "claude-opus-5-5",
            "nothing of the order on offer: the build's own newest"
        );
        assert_eq!(
            top(&ids(&["claude-fable-5-1", "claude-opus-5-5"]), None).model,
            "claude-opus-5-5",
            "a family billed to credits where billing is unread is passed over"
        );
        assert_eq!(top(&seed(), None).model, SEED[0]);
        assert_eq!(top(&[], None).model, SEED[0]);
    }

    #[test]
    fn an_order_is_read_from_its_priority_array_and_refused_whole_when_bad() {
        assert_eq!(
            parse_order(r#"{"priority":["claude-opus-6","claude-opus-5-5"],"history":[]}"#),
            Some(vec![
                "claude-opus-6".to_string(),
                "claude-opus-5-5".to_string()
            ])
        );
        for bad in [
            "not json",
            r#"{"priority":[]}"#,
            r#"{"priority":["claude-opus-6","claude-opus-6"]}"#,
            r#"{"priority":["claude-opus-6","gpt-5"]}"#,
            r#"{"history":[]}"#,
        ] {
            assert_eq!(parse_order(bad), None, "{bad}");
        }
    }
}

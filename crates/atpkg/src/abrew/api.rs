// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The formulae.brew.sh JSON API client with an on-disk cache (design §3).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aterm_json::Value;

use super::fetch;
use super::formula::{Cask, Formula};
use super::machine::Machine;
use super::prefix::{Prefix, write_metadata};

const API_BASE: &str = "https://formulae.brew.sh/api";
const API_BYTES: usize = 64 * 1024 * 1024;

pub struct Api {
    prefix: Prefix,
    writable: bool,
}

impl Api {
    #[must_use]
    pub const fn new(prefix: Prefix) -> Api {
        Api {
            prefix,
            writable: true,
        }
    }

    /// A cache-read-only client: fetches parse in memory and nothing is created on disk.
    #[must_use]
    pub const fn read_only(prefix: Prefix) -> Api {
        Api {
            prefix,
            writable: false,
        }
    }

    fn cache_path(&self, file: &str) -> PathBuf {
        self.prefix.cache().join("api").join(file)
    }

    fn fresh(path: &std::path::Path, max_age: Duration) -> bool {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age < max_age)
    }

    fn doc_with_freshness(
        &self,
        rel_url: &str,
        cache_file: &str,
        max_age_s: u64,
        allow_stale: bool,
    ) -> Result<Value, String> {
        self.doc_checked(rel_url, cache_file, max_age_s, allow_stale)
            .map_err(fetch::GetError::into_message)
    }

    fn doc_checked(
        &self,
        rel_url: &str,
        cache_file: &str,
        max_age_s: u64,
        allow_stale: bool,
    ) -> Result<Value, fetch::GetError> {
        if self.writable {
            self.prefix
                .ensure_api_cache()
                .map_err(fetch::GetError::other)?;
        }
        let path = self.cache_path(cache_file);
        if Self::fresh(&path, Duration::from_secs(max_age_s))
            && let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, API_BYTES)
            && let Ok(v) = parse_json(&bytes)
        {
            return Ok(v);
        }
        match get_document(rel_url) {
            Ok(bytes) => {
                let value = parse_json(&bytes).map_err(fetch::GetError::other)?;
                if self.writable {
                    let _ = write_metadata(&path, &bytes);
                }
                Ok(value)
            }
            Err(e) => {
                if allow_stale
                    && let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, API_BYTES)
                {
                    eprintln!("abrew: offline; using stale API cache for {cache_file}");
                    parse_json(&bytes).map_err(fetch::GetError::other)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Drop cached per-package documents but keep `keep` (the freshly fetched indexes).
    pub fn refresh_except(&self, keep: &[&str]) {
        if self.prefix.ensure_api_cache().is_err() {
            return;
        }
        let dir = self.prefix.cache().join("api");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if keep.contains(&name.as_str()) {
                    continue;
                }
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Drop every cached API document (`abrew update`).
    pub fn refresh(&self) {
        if self.prefix.ensure_api_cache().is_err() {
            return;
        }
        let dir = self.prefix.cache().join("api");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    fn formula_doc(&self, name: &str) -> Result<Value, String> {
        let short = Formula::short_name(name);
        self.doc_checked(
            &format!("formula/{short}.json"),
            &format!("formula-{short}.json"),
            3600,
            true,
        )
        .map_err(|e| not_found_error(e, "formula", short))
    }

    fn cask_doc(&self, token: &str) -> Result<Value, String> {
        let short = Formula::short_name(token);
        self.doc_checked(
            &format!("cask/{short}.json"),
            &format!("cask-{short}.json"),
            3600,
            true,
        )
        .map_err(|e| not_found_error(e, "cask", short))
    }

    pub fn formula(&self, name: &str) -> Result<Formula, String> {
        formula_record(&self.formula_doc(name)?, name, Machine::this())
    }

    /// Deletion may not interpret a missing or malformed dependency field as empty.
    pub(super) fn dependency_formula(&self, name: &str) -> Result<Formula, String> {
        dependency_formula(
            &self.formula_doc(name)?,
            Formula::short_name(name),
            Machine::this(),
        )
    }

    pub fn cask(&self, token: &str) -> Result<Cask, String> {
        cask_record(&self.cask_doc(token)?, token)
    }

    /// The raw API record for `info --json`.
    pub(super) fn formula_value(&self, name: &str) -> Result<Value, String> {
        let _ = self.formula(name)?;
        self.formula_doc(name)
    }

    /// [`Api::formula_value`] for a cask token.
    pub(super) fn cask_value(&self, token: &str) -> Result<Value, String> {
        let _ = self.cask(token)?;
        self.cask_doc(token)
    }

    /// Index rows already in the cache, without any fetch (`None` when no cached index stands).
    pub fn cached_index_rows(&self, casks: bool) -> Option<Vec<IndexRow>> {
        let file = if casks {
            "cask-index.json"
        } else {
            "formula-index.json"
        };
        let bytes =
            crate::metadata_io::read_bounded_regular(&self.cache_path(file), API_BYTES).ok()?;
        let v = parse_json(&bytes).ok()?;
        Some(index_rows(&v, casks))
    }

    /// Index rows fetched now, ignoring cache freshness: the "after" side of an update diff.
    pub fn index_rows_fresh(&self, casks: bool) -> Result<Vec<IndexRow>, String> {
        let (rel, file) = if casks {
            ("cask.json", "cask-index.json")
        } else {
            ("formula.json", "formula-index.json")
        };
        let v = self.doc_with_freshness(rel, file, 0, false)?;
        Ok(index_rows(&v, casks))
    }

    /// The full formula or cask index (for `search`): (name, desc).
    pub fn index_fresh(&self, casks: bool) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .index_inner(casks, 0, false)?
            .into_iter()
            .map(|(name, _, desc)| (name, desc))
            .collect())
    }

    pub fn index(&self, casks: bool) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .index_inner(casks, 86_400, true)?
            .into_iter()
            .map(|(name, _, desc)| (name, desc))
            .collect())
    }

    /// The index with cask display names (`desc` search prints and matches them).
    pub fn desc_index(&self, casks: bool) -> Result<Vec<(String, String, String)>, String> {
        self.index_inner(casks, 86_400, true)
    }

    /// Declared install options per formula from the raw index records.
    pub fn options_index(&self) -> Result<Vec<OptionsRow>, String> {
        let v = self.doc_with_freshness("formula.json", "formula-index.json", 86_400, true)?;
        let mut out = Vec::new();
        if let Some(arr) = v.as_array() {
            for item in arr {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let (options, head) = parse_options_record(item);
                let full_name = item
                    .get("full_name")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string();
                out.push(OptionsRow {
                    full_name,
                    options,
                    head,
                });
            }
        }
        Ok(out)
    }

    fn index_inner(
        &self,
        casks: bool,
        max_age_s: u64,
        allow_stale: bool,
    ) -> Result<Vec<(String, String, String)>, String> {
        let (rel, file) = if casks {
            ("cask.json", "cask-index.json")
        } else {
            ("formula.json", "formula-index.json")
        };
        let v = self.doc_with_freshness(rel, file, max_age_s, allow_stale)?;
        let mut out = Vec::new();
        if let Some(arr) = v.as_array() {
            for item in arr {
                let name = if casks {
                    item.get("token")
                } else {
                    item.get("name")
                }
                .and_then(Value::as_str)
                .unwrap_or_default();
                let display = if casks {
                    item.get("name")
                        .and_then(Value::as_array)
                        .map(|names| {
                            names
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let desc = item.get("desc").and_then(Value::as_str).unwrap_or_default();
                if !name.is_empty() {
                    out.push((name.to_string(), display, desc.to_string()));
                }
            }
        }
        Ok(out)
    }
}

/// Where admission reads package records from ([`Api`] or [`super::census`]).
pub trait Catalog {
    /// The machine records are resolved, and admission judged, for.
    fn machine(&self) -> &Machine;
    /// The formula `name` names (a `homebrew/core/` or tap prefix dropped).
    fn formula(&self, name: &str) -> Result<Formula, String>;
    /// The cask `token` names.
    fn cask(&self, token: &str) -> Result<Cask, String>;
}

impl Catalog for Api {
    fn machine(&self) -> &Machine {
        Machine::this()
    }
    fn formula(&self, name: &str) -> Result<Formula, String> {
        Api::formula(self, name)
    }
    fn cask(&self, token: &str) -> Result<Cask, String> {
        Api::cask(self, token)
    }
}

/// One formula record, resolved for `machine` and bound to the request.
pub(super) fn formula_record(v: &Value, name: &str, machine: &Machine) -> Result<Formula, String> {
    let short = Formula::short_name(name);
    let mut f = Formula::from_api(v)?;
    f.resolve_variations(machine);
    if f.name != short
        && !f.aliases.iter().any(|a| a == short)
        && !f.oldnames.iter().any(|a| a == short)
    {
        return Err(format!(
            "abrew: formula metadata for {short} names {}",
            f.name
        ));
    }
    Ok(f)
}

/// One cask record, bound to the requested token.
pub(super) fn cask_record(v: &Value, token: &str) -> Result<Cask, String> {
    let short = Formula::short_name(token);
    let c = Cask::from_api(v)?;
    if c.token != short {
        return Err(format!(
            "abrew: cask metadata for {short} names {}",
            c.token
        ));
    }
    Ok(c)
}

fn get_document(rel_url: &str) -> Result<Vec<u8>, fetch::GetError> {
    let bytes = fetch::curl_get_with_status(&format!("{API_BASE}/{rel_url}"), &[])?;
    if bytes.len() > API_BYTES {
        return Err(fetch::GetError::other(format!(
            "abrew: API document exceeds the {API_BYTES}-byte limit"
        )));
    }
    Ok(bytes)
}

/// The full formula (`formula.json`) or cask (`cask.json`) index fetched in memory.
pub fn fetch_index(casks: bool) -> Result<Vec<u8>, String> {
    get_document(if casks { "cask.json" } else { "formula.json" })
        .map_err(fetch::GetError::into_message)
}

/// One index record's report fields, for the update diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRow {
    pub name: String,
    pub desc: String,
    pub version: String,
    /// Casks only; formulae carry `false`.
    pub auto_updates: bool,
}

fn index_rows(v: &Value, casks: bool) -> Vec<IndexRow> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            let name = if casks {
                item.get("token")
            } else {
                item.get("name")
            }
            .and_then(Value::as_str)
            .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let version = if casks {
                item.get("version").and_then(Value::as_str)
            } else {
                item.get("versions")
                    .and_then(|vs| vs.get("stable"))
                    .and_then(Value::as_str)
            }
            .unwrap_or_default();
            out.push(IndexRow {
                name: name.to_string(),
                desc: item
                    .get("desc")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                version: match item.get("revision").and_then(Value::as_u64) {
                    Some(revision) if !casks && revision > 0 && !version.is_empty() => {
                        format!("{version}_{revision}")
                    }
                    _ => version.to_string(),
                },
                auto_updates: casks
                    && item
                        .get("auto_updates")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
            });
        }
    }
    out
}

pub struct OptionsRow {
    pub full_name: String,
    /// (flag without `--`, description), sorted by flag.
    pub options: Vec<(String, String)>,
    pub head: bool,
}

/// One raw API record's declared options and HEAD presence, for `brew options`.
pub(super) fn parse_options_record(v: &Value) -> (Vec<(String, String)>, bool) {
    let mut options: Vec<(String, String)> = v
        .get("options")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    let flag = opt.get("option").and_then(Value::as_str)?;
                    if flag.is_empty() {
                        return None;
                    }
                    let desc = opt
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some((flag.to_string(), desc.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    options.sort();
    let head = v
        .get("urls")
        .and_then(|u| u.get("head"))
        .is_some_and(|h| h.as_object().is_some());
    (options, head)
}

pub(super) fn dependency_formula(
    v: &Value,
    name: &str,
    machine: &Machine,
) -> Result<Formula, String> {
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    let mut formula = Formula::from_api(v)?;
    if !formula.resolve_variations(machine) {
        return Err(format!(
            "abrew: {name} has unknown system dependency metadata"
        ));
    }
    if formula.name != name {
        return Err(format!(
            "abrew: dependency metadata for {name} names {}",
            formula.name
        ));
    }
    if !super::valid_cask_version(&formula.version) {
        return Err("abrew: invalid formula version".to_string());
    }
    if v.get("revision").and_then(Value::as_u64) != Some(formula.revision) {
        return Err(format!("abrew: {name} has no known package revision"));
    }
    let raw = v
        .get("dependencies")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("abrew: {name} has no complete dependency array"))?;
    for dep in raw {
        let dep = dep
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("abrew: malformed dependency of {name}"))?;
        if !super::valid_name(dep) {
            return Err(format!("abrew: malformed dependency of {name}"));
        }
    }
    for dep in &formula.dependencies {
        if dep.is_empty() || !super::valid_name(dep) {
            return Err(format!("abrew: malformed dependency of {name}"));
        }
    }
    Ok(formula)
}

fn not_found_error(error: fetch::GetError, kind: &str, name: &str) -> String {
    if error.is_not_found() {
        format!("abrew: no such {kind}: {name}")
    } else {
        error.into_message()
    }
}

fn parse_json(bytes: &[u8]) -> Result<Value, String> {
    aterm_json::from_slice(bytes).map_err(|e| format!("abrew: bad API JSON: {e}"))
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn ghcr_token(repo: &str) -> Result<String, String> {
    if repo.is_empty()
        || repo
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')))
    {
        return Err(format!("abrew: invalid GHCR repository: {repo}"));
    }
    let scope = percent_encode(&format!("repository:{repo}:pull"));
    let url = format!("https://ghcr.io/token?service=ghcr.io&scope={scope}");
    let bytes = fetch::curl_get(&url, &[])?;
    let v = parse_json(&bytes)?;
    v.get("token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "abrew: GHCR token response has no token".to_string())
}

/// The GHCR repository a bottle URL belongs to (`homebrew/core/wget`), if it is a GHCR blob URL.
#[must_use]
pub fn ghcr_repo_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://ghcr.io/v2/")?;
    let repo = rest.split("/blobs/").next()?;
    if repo.is_empty() || repo == rest {
        return None;
    }
    Some(repo.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghcr_repo_with_query_delimiters_is_refused() {
        assert!(ghcr_token("homebrew/core/wget&scope=admin").is_err());
        assert!(ghcr_token("").is_err());
        assert_eq!(percent_encode("repository:a/b:pull"), "repository:a/b:pull");
        assert_eq!(percent_encode("a&b"), "a%26b");
    }

    #[test]
    #[cfg(unix)]
    fn formula_metadata_must_name_the_request() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure_api_cache().unwrap();
        let api = Api::new(prefix.clone());
        write_metadata(
            &api.cache_path("formula-demo.json"),
            br#"{"name":"other","versions":{"stable":"1"}}"#,
        )
        .unwrap();
        assert!(api.formula("demo").is_err());
        write_metadata(
            &api.cache_path("formula-demo.json"),
            br#"{"name":"real","aliases":["demo"],"versions":{"stable":"1"}}"#,
        )
        .unwrap();
        assert_eq!(api.formula("demo").unwrap().name, "real");
    }

    #[test]
    #[cfg(unix)]
    fn cached_documents_use_real_bounded_metadata_and_preserve_link_targets() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure_api_cache().unwrap();
        let api = Api::new(prefix.clone());
        let path = api.cache_path("formula-demo.json");
        let owner = tmp.path().join("owner-note");
        std::fs::write(&owner, b"keep").unwrap();
        std::os::unix::fs::symlink(&owner, &path).unwrap();
        let document = br#"{"name":"demo","versions":{"stable":"1"}}"#;
        write_metadata(&path, document).unwrap();
        assert!(!path.is_symlink());
        assert_eq!(std::fs::read(&owner).unwrap(), b"keep");
        assert_eq!(api.formula("demo").unwrap().version, "1");
        assert!(!prefix.cellar().exists());

        let outside = tmp.path().join("outside");
        std::fs::rename(prefix.cache().join("api"), &outside).unwrap();
        std::os::unix::fs::symlink(&outside, prefix.cache().join("api")).unwrap();
        assert!(
            api.formula("demo")
                .unwrap_err()
                .contains("not a real directory")
        );
        api.refresh();
        assert_eq!(
            std::fs::read(outside.join("formula-demo.json")).unwrap(),
            document
        );
        assert_eq!(std::fs::read(&owner).unwrap(), b"keep");
    }

    #[test]
    fn ghcr_repo_parse() {
        assert_eq!(
            ghcr_repo_of("https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc").as_deref(),
            Some("homebrew/core/wget")
        );
        assert_eq!(ghcr_repo_of("https://example.com/x.tar.gz"), None);
    }

    #[test]
    fn options_record_sorts_flags_and_sees_head() {
        let v: Value = aterm_json::from_slice(
            br#"{"options":[{"option":"with-z","description":"Zed"},{"option":"with-a","description":"Ay"},{"option":"","description":"skip"}],"urls":{"head":{"url":"https://example.com/x.git"}}}"#,
        )
        .unwrap();
        let (options, head) = parse_options_record(&v);
        assert_eq!(
            options,
            [
                ("with-a".to_string(), "Ay".to_string()),
                ("with-z".to_string(), "Zed".to_string())
            ]
        );
        assert!(head);
        let bare: Value =
            aterm_json::from_slice(br#"{"options":[],"urls":{"stable":{}}}"#).unwrap();
        assert_eq!(parse_options_record(&bare), (vec![], false));
    }
}

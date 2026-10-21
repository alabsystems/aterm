// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The formulae.brew.sh JSON API client with an on-disk cache
//! (design §3). Cached JSON is served when fresh (1 h; indexes 24 h)
//! or when the network fails and a stale copy stands. Cache entries are
//! bounded regular files, published without following their final links.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aterm_json::Value;

use super::fetch;
use super::formula::{Cask, Formula};
use super::prefix::{Prefix, write_metadata};

const API_BASE: &str = "https://formulae.brew.sh/api";
// Full formula/cask indexes exceed 20 MiB. Bound both cached reads and newly
// retained documents, leaving room for index growth without unbounded metadata.
const API_BYTES: usize = 64 * 1024 * 1024;

pub struct Api {
    prefix: Prefix,
    writable: bool,
}

impl Api {
    #[must_use]
    pub fn new(prefix: Prefix) -> Api {
        Api {
            prefix,
            writable: true,
        }
    }

    /// A cache-read-only client: fetches parse in memory and nothing is
    /// created on disk (dry runs must leave no prefix behind).
    #[must_use]
    pub fn read_only(prefix: Prefix) -> Api {
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

    /// Fetch (or reuse) one API document as parsed JSON.
    fn doc(&self, rel_url: &str, cache_file: &str, max_age_s: u64) -> Result<Value, String> {
        self.doc_with_freshness(rel_url, cache_file, max_age_s, true)
    }

    fn doc_with_freshness(
        &self,
        rel_url: &str,
        cache_file: &str,
        max_age_s: u64,
        allow_stale: bool,
    ) -> Result<Value, String> {
        if self.writable {
            self.prefix.ensure_api_cache()?;
        }
        let path = self.cache_path(cache_file);
        if Self::fresh(&path, Duration::from_secs(max_age_s))
            && let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, API_BYTES)
            && let Ok(v) = parse_json(&bytes)
        {
            return Ok(v);
        }
        let url = format!("{API_BASE}/{rel_url}");
        match fetch::curl_get(&url, &[]) {
            Ok(bytes) => {
                if bytes.len() > API_BYTES {
                    return Err(format!(
                        "abrew: API document exceeds the {API_BYTES}-byte limit"
                    ));
                }
                let value = parse_json(&bytes)?;
                if self.writable {
                    let _ = write_metadata(&path, &bytes);
                }
                Ok(value)
            }
            Err(e) => {
                // Stale cache beats no answer (offline work, design §6).
                if allow_stale
                    && let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, API_BYTES)
                {
                    eprintln!("abrew: offline; using stale API cache for {cache_file}");
                    parse_json(&bytes)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Drop cached per-package documents but keep `keep` (the freshly
    /// fetched indexes): a staged update publishes new indexes before
    /// invalidating detail docs, so a failed update never truncates
    /// the working cache to empty.
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
        self.doc(
            &format!("formula/{short}.json"),
            &format!("formula-{short}.json"),
            3600,
        )
        .map_err(|e| not_found_error(&e, "formula", short))
    }

    fn cask_doc(&self, token: &str) -> Result<Value, String> {
        let short = Formula::short_name(token);
        self.doc(
            &format!("cask/{short}.json"),
            &format!("cask-{short}.json"),
            3600,
        )
        .map_err(|e| not_found_error(&e, "cask", short))
    }

    pub fn formula(&self, name: &str) -> Result<Formula, String> {
        let short = Formula::short_name(name);
        let mut f = Formula::from_api(&self.formula_doc(name)?)?;
        f.resolve_variations();
        // Bind metadata to the request: the record must name the
        // requested formula (or carry it as a known alias/oldname), so a
        // mismatched cache/redirect body cannot rename the install.
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

    /// Deletion may not interpret a missing or malformed dependency field as empty.
    pub(super) fn dependency_formula(&self, name: &str) -> Result<Formula, String> {
        dependency_formula(&self.formula_doc(name)?, Formula::short_name(name))
    }

    pub fn cask(&self, token: &str) -> Result<Cask, String> {
        let short = Formula::short_name(token);
        let v = self.cask_doc(token)?;
        let c = Cask::from_api(&v)?;
        if c.token != short {
            return Err(format!(
                "abrew: cask metadata for {short} names {}",
                c.token
            ));
        }
        Ok(c)
    }

    /// The raw API record for `info --json`: brew's JSON info is
    /// the API document itself, so abrew emits the same record it
    /// parsed (identity-bound like [`Api::formula`]).
    pub(super) fn formula_value(&self, name: &str) -> Result<Value, String> {
        let _ = self.formula(name)?;
        self.formula_doc(name)
    }

    /// [`Api::formula_value`] for a cask token.
    pub(super) fn cask_value(&self, token: &str) -> Result<Value, String> {
        let _ = self.cask(token)?;
        self.cask_doc(token)
    }

    /// The full formula or cask index (for `search`): (name, desc).
    /// The index fetched now, ignoring cache freshness (`abrew update`).
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

    /// The index with cask display names (`desc` search prints and
    /// matches them): (name/token, display names joined, desc).
    /// Formula rows carry an empty display field.
    pub fn desc_index(&self, casks: bool) -> Result<Vec<(String, String, String)>, String> {
        self.index_inner(casks, 86_400, true)
    }

    /// Declared install options per formula from the raw index
    /// records. Only rows the parse can fully see are returned;
    /// options are empty across homebrew/core today.
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

/// One formula's declared install options, for `brew options`.
pub struct OptionsRow {
    pub full_name: String,
    /// (flag without `--`, description), sorted by flag.
    pub options: Vec<(String, String)>,
    pub head: bool,
}

/// One raw API record's declared options and HEAD presence, for
/// `brew options`: ([(flag without `--`, description)] sorted by
/// flag as brew dumps them, has HEAD spec).
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

pub(super) fn dependency_formula(v: &Value, name: &str) -> Result<Formula, String> {
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    let mut formula = Formula::from_api(v)?;
    if !formula.resolve_variations() {
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
    // The raw document must carry a complete top-level array of plain
    // names, and the system-resolved list (a variation may replace it)
    // must be plain too: neither may smuggle a malformed entry past.
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

/// A 404 from the per-package API is a plain "no such package",
/// not a transport failure worth showing curl's stderr for.
fn not_found_error(error: &str, kind: &str, name: &str) -> String {
    if error.contains("404") {
        format!("abrew: no such {kind}: {name}")
    } else {
        error.to_string()
    }
}

fn parse_json(bytes: &[u8]) -> Result<Value, String> {
    aterm_json::from_slice(bytes).map_err(|e| format!("abrew: bad API JSON: {e}"))
}

/// A GHCR bearer token for one bottle blob repository, e.g.
/// `homebrew/core/wget` (bottle files live on ghcr.io).
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
    // The repo path comes from an API URL: query delimiters are invalid
    // in it, and the scope value is percent-encoded before use.
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

/// The GHCR repository a bottle URL belongs to
/// (`https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:…` →
/// `homebrew/core/wget`), if it is a GHCR blob URL.
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
        // An alias binding is admitted.
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

        // A linked canonical cache base must refuse before the downloader,
        // even when its fresh, otherwise valid JSON could satisfy the request.
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

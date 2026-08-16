// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, generation-local font discovery and admission.
//!
//! A config generation may name several related faces.  Scanning the system
//! tree separately for every name makes one typo multiply directory I/O and
//! `name`-table parsing on the caller.  [`resolve_and_admit`] walks once,
//! resolves every request against that immutable catalogue, and reads each
//! selected file once.  Every source of work has an explicit cap.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::font_file;

/// Production limits for one font-environment generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Real directories whose entries may be enumerated.
    pub max_dirs: usize,
    /// Directory entries inspected across the whole tree.
    pub max_entries: usize,
    /// Font files retained in the immutable catalogue.
    pub max_font_files: usize,
    /// Requested config values admitted in one generation.
    pub max_requests: usize,
    /// Combined directory-entry and name-table work units.
    pub max_work: usize,
    /// Bytes read while probing otherwise-unmatched `name` tables.
    pub max_name_bytes: usize,
    /// All bytes read in the generation, including name probes and selected
    /// immutable assets. Bytes reused from the name cache count only once.
    pub max_aggregate_bytes: usize,
    /// Directory recursion depth (root is depth zero).
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_dirs: 256,
            max_entries: 16_384,
            max_font_files: 8_192,
            max_requests: 64,
            max_work: 24_576,
            max_name_bytes: 128 * 1024 * 1024,
            max_aggregate_bytes: 512 * 1024 * 1024,
            max_depth: super::FONT_SCAN_MAX_DEPTH,
        }
    }
}

/// Exact work observed while building one catalogue generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub dirs: usize,
    pub entries: usize,
    pub font_files: usize,
    pub name_files: usize,
    pub name_bytes: usize,
    pub aggregate_bytes: usize,
    pub work: usize,
    /// Stable cap labels reached by the scan/admission.
    pub exhausted: BTreeSet<&'static str>,
}

impl Stats {
    #[must_use]
    pub fn truncated(&self) -> bool {
        !self.exhausted.is_empty()
    }
}

/// One immutable font file admitted for a config generation.
#[derive(Clone, Debug)]
pub struct AdmittedFont {
    pub path: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Result for one request, in input order.
#[derive(Clone, Debug)]
pub struct Entry {
    pub requested: String,
    pub result: Result<AdmittedFont, String>,
}

/// All requested faces plus bounded-work diagnostics for one generation.
#[derive(Clone, Debug)]
pub struct Batch {
    pub entries: Vec<Entry>,
    pub stats: Stats,
}

impl Batch {
    /// Return the result corresponding to input index `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Result<AdmittedFont, String>> {
        self.entries.get(index).map(|entry| &entry.result)
    }
}

#[derive(Debug)]
struct Catalog {
    limits: Limits,
    files: Vec<PathBuf>,
    stats: Stats,
    /// Bytes already read for a name-table probe. Reused as the immutable
    /// admitted asset if that path wins, closing a second-read TOCTOU window.
    bytes: HashMap<PathBuf, Arc<Vec<u8>>>,
}

impl Catalog {
    fn scan(dirs: &[PathBuf], limits: Limits) -> Self {
        let mut catalog = Self {
            limits,
            files: Vec::new(),
            stats: Stats::default(),
            bytes: HashMap::new(),
        };
        for dir in dirs {
            catalog.walk(dir, 0);
            if catalog.scan_capped() {
                break;
            }
        }
        catalog.stats.font_files = catalog.files.len();
        catalog
    }

    fn scan_capped(&self) -> bool {
        self.stats.entries >= self.limits.max_entries
            || self.files.len() >= self.limits.max_font_files
            || self.stats.work >= self.limits.max_work
    }

    fn mark_scan_cap(&mut self) {
        if self.stats.dirs >= self.limits.max_dirs {
            self.stats.exhausted.insert("directories");
        }
        if self.stats.entries >= self.limits.max_entries {
            self.stats.exhausted.insert("entries");
        }
        if self.files.len() >= self.limits.max_font_files {
            self.stats.exhausted.insert("font files");
        }
        if self.stats.work >= self.limits.max_work {
            self.stats.exhausted.insert("work units");
        }
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        if self.stats.dirs >= self.limits.max_dirs {
            self.stats.exhausted.insert("directories");
            return;
        }
        if self.scan_capped() {
            self.mark_scan_cap();
            return;
        }
        self.stats.dirs += 1;
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };

        // Never collect an unbounded directory merely to sort it. The +1 is a
        // sentinel proving truncation; at most `remaining + 1` DirEntry values
        // are resident at once.
        let remaining = self
            .limits
            .max_entries
            .saturating_sub(self.stats.entries)
            .min(self.limits.max_work.saturating_sub(self.stats.work));
        if remaining == 0 {
            self.mark_scan_cap();
            return;
        }
        let mut entries: Vec<_> = read_dir
            .filter_map(Result::ok)
            .take(remaining + 1)
            .collect();
        if entries.len() > remaining {
            entries.pop();
            self.stats.exhausted.insert("entries");
        }
        entries.sort_by_key(std::fs::DirEntry::path);

        for entry in entries {
            if self.scan_capped() {
                self.mark_scan_cap();
                break;
            }
            self.stats.entries += 1;
            self.stats.work += 1;
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < self.limits.max_depth {
                    self.walk(&path, depth + 1);
                }
            } else if is_font(&path)
                && (!file_type.is_symlink() || path.is_file())
                && self.files.len() < self.limits.max_font_files
            {
                self.files.push(path);
            }
        }
        if self.files.len() >= self.limits.max_font_files {
            self.stats.exhausted.insert("font files");
        }
    }

    fn resolve_many(&mut self, requests: &[String]) -> Vec<Result<PathBuf, String>> {
        let mut results = vec![None; requests.len()];
        let mut wants: HashMap<String, Vec<usize>> = HashMap::new();
        let mut prefix_hits: HashMap<String, PathBuf> = HashMap::new();

        for (index, request) in requests.iter().enumerate() {
            let trimmed = request.trim();
            if index >= self.limits.max_requests {
                self.stats.exhausted.insert("requests");
                results[index] = Some(Err(format!(
                    "font request limit ({}) exceeded",
                    self.limits.max_requests
                )));
                continue;
            }
            if trimmed.is_empty() {
                results[index] = Some(Err("empty font name or path".to_string()));
            } else if trimmed.contains(['/', '\\']) {
                results[index] = Some(Ok(PathBuf::from(trimmed)));
            } else {
                let normalized = normalize_family(trimmed);
                if normalized.is_empty() {
                    results[index] = Some(Err("empty normalized font family".to_string()));
                } else {
                    wants.entry(normalized).or_default().push(index);
                }
            }
        }

        // One filename pass resolves exact stems and remembers the first weaker
        // prefix. Every requested family shares this pass.
        for path in &self.files {
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let normalized = normalize_family(stem);
            for (want, indexes) in &wants {
                if normalized == *want {
                    for &index in indexes {
                        results[index] = Some(Ok(path.clone()));
                    }
                } else if normalized.starts_with(want) {
                    prefix_hits
                        .entry(want.clone())
                        .or_insert_with(|| path.clone());
                }
            }
        }

        let mut unresolved: BTreeSet<String> = wants
            .iter()
            .filter(|(_, indexes)| indexes.iter().any(|&index| results[index].is_none()))
            .map(|(want, _)| want.clone())
            .collect();

        // Name tables are the expensive fallback. Parse each candidate at most
        // once and compare it against every still-unresolved family.
        if !unresolved.is_empty() {
            for path in self.files.clone() {
                if unresolved.is_empty() {
                    break;
                }
                if self.stats.work >= self.limits.max_work {
                    self.stats.exhausted.insert("work units");
                    break;
                }
                let name_remaining = self
                    .limits
                    .max_name_bytes
                    .saturating_sub(self.stats.name_bytes);
                let aggregate_remaining = self
                    .limits
                    .max_aggregate_bytes
                    .saturating_sub(self.stats.aggregate_bytes);
                let read_cap = name_remaining
                    .min(aggregate_remaining)
                    .min(super::NAME_SCAN_MAX_BYTES as usize);
                if read_cap == 0 {
                    if name_remaining == 0 {
                        self.stats.exhausted.insert("name bytes");
                    }
                    if aggregate_remaining == 0 {
                        self.stats.exhausted.insert("aggregate bytes");
                    }
                    break;
                }
                self.stats.work += 1;
                let Ok(raw) = font_file::read_bounded_font_file(&path, read_cap) else {
                    continue;
                };
                self.stats.name_files += 1;
                self.stats.name_bytes += raw.len();
                self.stats.aggregate_bytes += raw.len();
                let bytes = Arc::new(raw);
                let matches: Vec<String> = unresolved
                    .iter()
                    .filter(|want| super::font_name_table_matches(&bytes, want))
                    .cloned()
                    .collect();
                if !matches.is_empty() {
                    self.bytes.insert(path.clone(), Arc::clone(&bytes));
                }
                for want in matches {
                    if let Some(indexes) = wants.get(&want) {
                        for &index in indexes {
                            results[index] = Some(Ok(path.clone()));
                        }
                    }
                    unresolved.remove(&want);
                }
            }
        }

        let truncated = self.stats.truncated();
        for (want, indexes) in wants {
            for index in indexes {
                if results[index].is_none() {
                    results[index] = prefix_hits.get(&want).cloned().map(Ok).or_else(|| {
                        Some(Err(if truncated {
                            format!(
                                "{:?} was not found before bounded font discovery exhausted {:?}",
                                requests[index], self.stats.exhausted
                            )
                        } else {
                            format!("{:?} does not resolve to a font file", requests[index])
                        }))
                    });
                }
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                result.unwrap_or_else(|| {
                    Err(format!(
                        "{:?} does not resolve to a font file",
                        requests[index]
                    ))
                })
            })
            .collect()
    }

    fn admit(&mut self, path: &Path) -> Result<AdmittedFont, String> {
        if let Some(bytes) = self.bytes.get(path) {
            return Ok(AdmittedFont {
                path: path.to_string_lossy().into_owned(),
                bytes: Arc::clone(bytes),
            });
        }
        let remaining = self
            .limits
            .max_aggregate_bytes
            .saturating_sub(self.stats.aggregate_bytes);
        if remaining == 0 {
            self.stats.exhausted.insert("aggregate bytes");
            return Err(format!(
                "font aggregate-byte budget ({}) exhausted",
                self.limits.max_aggregate_bytes
            ));
        }
        let cap = remaining.min(font_file::MAX_FONT_FILE_BYTES);
        let raw = font_file::read_bounded_font_file(path, cap).map_err(|error| {
            if cap < font_file::MAX_FONT_FILE_BYTES {
                self.stats.exhausted.insert("aggregate bytes");
                format!(
                    "font {:?} exceeds the remaining aggregate-byte budget ({remaining} bytes): {error}",
                    path
                )
            } else {
                format!("font {:?} failed bounded admission ({error})", path)
            }
        })?;
        self.stats.aggregate_bytes += raw.len();
        let bytes = Arc::new(raw);
        self.bytes.insert(path.to_path_buf(), Arc::clone(&bytes));
        Ok(AdmittedFont {
            path: path.to_string_lossy().into_owned(),
            bytes,
        })
    }
}

fn is_font(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            super::FONT_EXTS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn normalize_family(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-' && *ch != '_')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// Resolve and admit every config-authored family/path in one bounded system
/// catalogue generation. Results retain input order and duplicate paths share
/// one immutable byte allocation.
#[must_use]
pub fn resolve_and_admit(requests: &[String]) -> Batch {
    resolve_and_admit_in_dirs(requests, &crate::font_search_dirs(), Limits::default())
}

fn resolve_and_admit_in_dirs(requests: &[String], dirs: &[PathBuf], limits: Limits) -> Batch {
    let need_catalog = requests.iter().take(limits.max_requests).any(|request| {
        !request.trim().contains(['/', '\\'])
            && crate::display_face_for_family(request.trim()).is_none()
    });
    let mut catalog = if need_catalog {
        Catalog::scan(dirs, limits)
    } else {
        Catalog {
            limits,
            files: Vec::new(),
            stats: Stats::default(),
            bytes: HashMap::new(),
        }
    };
    let paths = catalog.resolve_many(requests);
    let entries = requests
        .iter()
        .cloned()
        .zip(paths)
        .map(|(requested, result)| {
            // The `display:` scheme resolves to embedded bytes ahead of the system
            // catalogue — the exact interception `from_system_with_family`
            // performs at startup, so the two paths cannot disagree.
            let result = match crate::display_face_for_family(requested.trim()) {
                Some(bytes) => Ok(AdmittedFont {
                    path: requested.trim().to_string(),
                    bytes: Arc::new(bytes.to_vec()),
                }),
                None => result.and_then(|path| catalog.admit(&path)),
            };
            Entry { requested, result }
        })
        .collect();
    Batch {
        entries,
        stats: catalog.stats,
    }
}

/// Bounded file list used by renderer diagnostics and runtime discovery. This
/// preserves the stable directory/lexical ordering while removing their former
/// unbounded recursion, entry collection, and file accumulation.
pub(crate) fn system_font_files() -> Vec<PathBuf> {
    Catalog::scan(&crate::font_search_dirs(), Limits::default()).files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aterm-font-catalog-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn one_scan_resolves_duplicates_and_shares_admitted_bytes() {
        let root = fixture("batch");
        let path = root.join("Example-Mono.ttf");
        std::fs::write(&path, b"font bytes").unwrap();
        let requests = vec!["Example Mono".to_string(), "ExampleMono".to_string()];
        let batch =
            resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), Limits::default());
        let first = batch.get(0).unwrap().as_ref().unwrap();
        let second = batch.get(1).unwrap().as_ref().unwrap();
        assert_eq!(first.path, path.to_string_lossy());
        assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
        assert_eq!(batch.stats.dirs, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn many_files_and_missing_families_stop_at_every_scan_cap() {
        let root = fixture("caps");
        for index in 0..40 {
            std::fs::write(root.join(format!("Face{index:03}.ttf")), b"not a font").unwrap();
        }
        let limits = Limits {
            max_dirs: 1,
            max_entries: 7,
            max_font_files: 5,
            max_requests: 3,
            max_work: 7,
            max_name_bytes: 8,
            max_aggregate_bytes: 16,
            max_depth: 0,
        };
        let requests = (0..12).map(|i| format!("Missing {i}")).collect::<Vec<_>>();
        let batch = resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), limits);
        assert!(batch.stats.dirs <= limits.max_dirs);
        assert!(batch.stats.entries <= limits.max_entries);
        assert!(batch.stats.font_files <= limits.max_font_files);
        assert!(batch.stats.work <= limits.max_work);
        assert!(batch.stats.name_bytes <= limits.max_name_bytes);
        assert!(batch.stats.aggregate_bytes <= limits.max_aggregate_bytes);
        assert!(batch.stats.truncated());
        assert!(batch.entries.iter().all(|entry| entry.result.is_err()));
        assert!(
            batch.entries[3]
                .result
                .as_ref()
                .unwrap_err()
                .contains("request limit")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn aggregate_byte_budget_is_shared_across_explicit_assets() {
        let root = fixture("bytes");
        let a = root.join("a.ttf");
        let b = root.join("b.ttf");
        std::fs::write(&a, [1; 10]).unwrap();
        std::fs::write(&b, [2; 10]).unwrap();
        let requests = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ];
        let limits = Limits {
            max_aggregate_bytes: 15,
            ..Limits::default()
        };
        let batch = resolve_and_admit_in_dirs(&requests, &[], limits);
        assert!(batch.entries[0].result.is_ok());
        assert!(
            batch.entries[1]
                .result
                .as_ref()
                .unwrap_err()
                .contains("aggregate-byte")
        );
        assert!(batch.stats.aggregate_bytes <= limits.max_aggregate_bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}

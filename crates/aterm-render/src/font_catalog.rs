// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, generation-local font discovery and admission.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::font_file;
use crate::{FaceRef, FaceTarget};

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
    /// All bytes read in the generation, including name probes and selected immutable assets.
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
            max_name_bytes: 256 * 1024 * 1024,
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
    #[inline]
    #[must_use]
    pub fn truncated(&self) -> bool {
        !self.exhausted.is_empty()
    }
}

/// One face admitted for a config generation: its file's immutable bytes and the face's collection index in them ([`FaceRef`]).
#[derive(Clone, Debug)]
pub struct AdmittedFont {
    pub path: String,
    pub index: u32,
    pub bytes: Arc<Vec<u8>>,
}

impl AdmittedFont {
    /// The face this admission resolved to.
    #[inline]
    #[must_use]
    pub fn face(&self) -> FaceRef {
        FaceRef::new(self.path.clone(), self.index)
    }
}

/// Which face of a resolved file a request takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceChoice {
    /// The face at this index: a `name`-table match, or a compiled-in face.
    At(u32),
    /// The face nearest this style, chosen from the admitted bytes: a file
    /// found by its file name or named by path (`crate::best_face`).
    Nearest(FaceTarget),
}

impl FaceChoice {
    fn index(self, bytes: &[u8]) -> u32 {
        match self {
            Self::At(index) => index,
            Self::Nearest(target) => {
                crate::best_face(bytes, None, target).map_or(0, |(_, index)| index)
            }
        }
    }
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
    #[inline]
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
    /// Every directory the walk asked `read_dir` of (roots, absent and unreadable ones
    /// included) with the mtime seen then: the validity key of the [`system_font_files`] memo.
    visited: Vec<(PathBuf, Option<std::time::SystemTime>)>,
}

impl Catalog {
    fn scan(dirs: &[PathBuf], limits: Limits) -> Self {
        let mut catalog = Self {
            limits,
            files: Vec::new(),
            stats: Stats::default(),
            bytes: HashMap::new(),
            visited: Vec::new(),
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
        self.visited.push((dir.to_path_buf(), dir_mtime(dir)));
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };

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

    fn resolve_many(
        &mut self,
        requests: &[(String, FaceTarget)],
    ) -> Vec<Result<(PathBuf, FaceChoice), String>> {
        type Want = (String, FaceTarget);
        let mut results: Vec<Option<Result<(PathBuf, FaceChoice), String>>> =
            vec![None; requests.len()];
        let mut wants: HashMap<Want, Vec<usize>> = HashMap::new();
        let mut prefix_hits: HashMap<Want, PathBuf> = HashMap::new();

        for (index, (request, target)) in requests.iter().enumerate() {
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
            } else if crate::is_generic_monospace_family(trimmed) {
                results[index] = Some(
                    crate::default_primary_identity()
                        .map(|face| (PathBuf::from(face.path), FaceChoice::At(face.index)))
                        .ok_or_else(|| format!("{trimmed:?}: no default face loads")),
                );
            } else if trimmed.contains(['/', '\\']) {
                results[index] = Some(Ok((PathBuf::from(trimmed), FaceChoice::Nearest(*target))));
            } else if crate::bundled::is_identity(trimmed)
                && let Some((id, _)) = crate::bundled::face_for_family(trimmed)
            {
                // only when nothing installed answers.
                results[index] = Some(Ok((PathBuf::from(id), FaceChoice::At(0))));
            } else {
                let normalized = normalize_family(trimmed);
                if normalized.is_empty() {
                    results[index] = Some(Err("empty normalized font family".to_string()));
                } else {
                    wants.entry((normalized, *target)).or_default().push(index);
                }
            }
        }

        for path in &self.files {
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let normalized = normalize_family(stem);
            for (want, indexes) in &wants {
                if normalized == want.0 {
                    for &index in indexes {
                        results[index]
                            .get_or_insert_with(|| Ok((path.clone(), FaceChoice::Nearest(want.1))));
                    }
                } else if normalized.starts_with(&want.0) {
                    prefix_hits
                        .entry(want.clone())
                        .or_insert_with(|| path.clone());
                }
            }
        }

        let mut unresolved: BTreeSet<Want> = wants
            .iter()
            .filter(|(_, indexes)| indexes.iter().any(|&index| results[index].is_none()))
            .map(|(want, _)| want.clone())
            .collect();

        let mut settling: HashMap<Want, ((u32, bool), PathBuf, u32)> = HashMap::new();
        let settle =
            |want: &Want,
             path: &Path,
             face: u32,
             results: &mut Vec<Option<Result<(PathBuf, FaceChoice), String>>>| {
                if let Some(indexes) = wants.get(want) {
                    for &index in indexes {
                        results[index] = Some(Ok((path.to_path_buf(), FaceChoice::At(face))));
                    }
                }
            };
        if !unresolved.is_empty() {
            for path in self.files.clone() {
                if unresolved.is_empty() && settling.is_empty() {
                    break;
                }
                if unresolved.is_empty()
                    && !settling
                        .values()
                        .any(|(_, best, _)| best.parent() == path.parent())
                {
                    continue;
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
                    let len = std::fs::metadata(&path).map_or(0, |meta| meta.len());
                    if len > read_cap as u64 && len <= super::NAME_SCAN_MAX_BYTES {
                        if len > name_remaining as u64 {
                            self.stats.exhausted.insert("name bytes");
                        }
                        if len > aggregate_remaining as u64 {
                            self.stats.exhausted.insert("aggregate bytes");
                        }
                    }
                    continue;
                };
                self.stats.name_files += 1;
                self.stats.name_bytes += raw.len();
                self.stats.aggregate_bytes += raw.len();
                let bytes = Arc::new(raw);
                let matches: Vec<(Want, u32, u32)> = unresolved
                    .iter()
                    .chain(
                        settling
                            .iter()
                            .filter(|(_, (_, best, _))| best.parent() == path.parent())
                            .map(|(want, _)| want),
                    )
                    .filter_map(|want| {
                        crate::best_face(&bytes, Some(&want.0), want.1)
                            .map(|(distance, face)| (want.clone(), distance, face))
                    })
                    .collect();
                if !matches.is_empty() {
                    self.bytes.insert(path.clone(), Arc::clone(&bytes));
                }
                for (want, distance, face) in matches {
                    unresolved.remove(&want);
                    let rank = super::name_match_rank(distance, &path);
                    let held =
                        settling
                            .entry(want)
                            .or_insert(((u32::MAX, true), path.clone(), face));
                    if rank < held.0 {
                        *held = (rank, path.clone(), face);
                    }
                }
                settling.retain(|want, (held, best, face)| {
                    let nearest = *held == super::NAME_MATCH_SETTLED;
                    if nearest {
                        settle(want, best, *face, &mut results);
                    }
                    !nearest
                });
            }
        }
        for (want, (_, best, face)) in settling {
            settle(&want, &best, face, &mut results);
        }

        let truncated = self.stats.truncated();
        for (want, indexes) in wants {
            for index in indexes {
                if results[index].is_none() {
                    let bundled = crate::bundled::face_for_family(requests[index].0.trim())
                        .map(|(id, _)| (PathBuf::from(id), FaceChoice::At(0)));
                    results[index] = prefix_hits
                        .get(&want)
                        .map(|path| (path.clone(), FaceChoice::Nearest(want.1)))
                        .or(bundled)
                        .map(Ok)
                        .or_else(|| {
                            Some(Err(if truncated {
                                format!(
                                    "{:?} was not found before bounded font discovery exhausted {:?}",
                                    requests[index].0, self.stats.exhausted
                                )
                            } else {
                                format!("{:?} does not resolve to a font file", requests[index].0)
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
                        requests[index].0
                    ))
                })
            })
            .collect()
    }

    fn admit(&mut self, path: &Path, choice: FaceChoice) -> Result<AdmittedFont, String> {
        if let Some(bytes) = self.bytes.get(path) {
            return Ok(AdmittedFont {
                path: path.to_string_lossy().into_owned(),
                index: choice.index(bytes),
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
            index: choice.index(&bytes),
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

/// Resolve and admit every config-authored family/path in one bounded system catalogue generation, each for the style it serves (`font_family` the regular face, `font_family_bold` the bold one: [`FaceTarget`]).
#[must_use]
pub fn resolve_and_admit(requests: &[(String, FaceTarget)]) -> Batch {
    resolve_and_admit_in_dirs(requests, &crate::font_search_dirs(), Limits::default())
}

fn resolve_and_admit_in_dirs(
    requests: &[(String, FaceTarget)],
    dirs: &[PathBuf],
    limits: Limits,
) -> Batch {
    let need_catalog = requests
        .iter()
        .take(limits.max_requests)
        .any(|(request, _)| {
            !request.trim().contains(['/', '\\'])
                && !crate::bundled::is_identity(request)
                && !crate::is_generic_monospace_family(request)
        });
    let mut catalog = if need_catalog {
        Catalog::scan(dirs, limits)
    } else {
        Catalog {
            limits,
            files: Vec::new(),
            stats: Stats::default(),
            bytes: HashMap::new(),
            visited: Vec::new(),
        }
    };
    let paths = catalog.resolve_many(requests);
    let entries = requests
        .iter()
        .map(|(requested, _)| requested.clone())
        .zip(paths)
        .map(|(requested, result)| {
            let result = match crate::display_family_refusal(&requested) {
                Some(refusal) => Err(refusal),
                None => result,
            };
            let result = result.and_then(|(path, choice)| {
                match path.to_str().and_then(crate::bundled::face_for_id) {
                    Some(bytes) => Ok(AdmittedFont {
                        path: path.to_string_lossy().into_owned(),
                        index: 0,
                        bytes: Arc::new(bytes.to_vec()),
                    }),
                    None => catalog.admit(&path, choice),
                }
            });
            Entry { requested, result }
        })
        .collect();
    Batch {
        entries,
        stats: catalog.stats,
    }
}

/// The modification time of a directory, `None` when it does not exist or
/// cannot be stat'ed — a value in its own right, so a root that appears later
/// invalidates a memo taken while it was absent.
fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).ok().and_then(|m| m.modified().ok())
}

/// One remembered walk: the roots it was asked for, every directory it
/// visited with the mtime seen then, and the files it found.
struct FontFilesMemo {
    roots: Vec<PathBuf>,
    visited: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    files: Vec<PathBuf>,
}

impl FontFilesMemo {
    /// Still describes the tree: every visited directory has the mtime it had.
    fn current(&self) -> bool {
        self.visited
            .iter()
            .all(|(dir, seen)| dir_mtime(dir) == *seen)
    }
}

/// Remembered walks, keyed by root list: one in production (`font_search_dirs()`); tests walk
/// private fixture roots and must not evict each other's or the real one.
static FONT_FILES_MEMOS: std::sync::Mutex<Vec<Arc<FontFilesMemo>>> =
    std::sync::Mutex::new(Vec::new());
const MAX_FONT_FILES_MEMOS: usize = 8;

/// One walk gate per root list ([`memoized_font_files_walked`]'s single-flight).
type WalkGate = std::sync::Arc<std::sync::Mutex<()>>;
static FONT_WALK_GATES: std::sync::Mutex<Vec<(Vec<PathBuf>, WalkGate)>> =
    std::sync::Mutex::new(Vec::new());

fn walk_gate(roots: &[PathBuf]) -> WalkGate {
    let mut gates = FONT_WALK_GATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, gate)) = gates.iter().find(|(r, _)| r == roots) {
        return gate.clone();
    }
    if gates.len() >= MAX_FONT_FILES_MEMOS {
        gates.clear();
    }
    let gate = WalkGate::default();
    gates.push((roots.to_vec(), gate.clone()));
    gate
}

/// [`system_font_files`] over an explicit root list, remembered per process.
fn memoized_font_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    memoized_font_files_walked(roots).0
}

/// [`memoized_font_files`] plus whether this call walked (`false`: answered from the memo).
fn memoized_font_files_walked(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let (inventory, walked) = memoized_font_inventory_walked(roots);
    (inventory.files.clone(), walked)
}

/// [`memoized_font_files_walked`] returning the memo itself. Every walk is a
/// new `Arc`, so an answer keyed on it ([`monospace_families`]) also sees a
/// file renamed over one of the same name, which moves its directory's mtime
/// but not the file list.
fn memoized_font_inventory_walked(roots: &[PathBuf]) -> (Arc<FontFilesMemo>, bool) {
    let remembered = || {
        let memos = FONT_FILES_MEMOS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memos
            .iter()
            .find(|m| m.roots == roots)
            .filter(|m| m.current())
            .map(Arc::clone)
    };
    if let Some(inventory) = remembered() {
        return (inventory, false);
    }
    let gate = walk_gate(roots);
    let _in_flight = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(inventory) = remembered() {
        return (inventory, false);
    }
    let catalog = Catalog::scan(roots, Limits::default());
    let mut memos = FONT_FILES_MEMOS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    memos.retain(|m| m.roots != roots);
    if memos.len() >= MAX_FONT_FILES_MEMOS {
        memos.clear();
    }
    let inventory = Arc::new(FontFilesMemo {
        roots: roots.to_vec(),
        visited: catalog.visited,
        files: catalog.files,
    });
    memos.push(Arc::clone(&inventory));
    (inventory, true)
}

/// Bounded font-file list, in directory/lexical order, for renderer diagnostics and runtime discovery.
pub(crate) fn system_font_files() -> Vec<PathBuf> {
    memoized_font_files(&crate::font_search_dirs())
}

/// What a font picker offers, every name one the resolvers find again: the face aterm draws when no family is set, the families compiled into aterm, and the installed faces sorted by what they are good for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonospaceFamilies {
    /// The family of the face aterm draws when `font_family` is unset.
    pub automatic: Option<String>,
    /// Whether that face has programming ligatures (`liga`/`calt`).
    pub automatic_has_ligatures: bool,
    /// Families compiled into aterm and chosen by name.
    pub bundled: Vec<String>,
    /// Installed fixed-pitch families with Latin letters, sorted.
    pub installed: Vec<String>,
    /// Installed fixed-pitch bold, italic and bold italic faces, by full name (`JetBrains Mono Bold`), each sorted.
    pub bold: Vec<String>,
    pub italic: Vec<String>,
    pub bold_italic: Vec<String>,
    /// Installed faces made for symbols and icons (Nerd Font symbols, `Apple Symbols`).
    pub symbol: Vec<String>,
    /// Installed colour emoji faces, by file name.
    pub emoji: Vec<String>,
    /// Whether each listed family's regular face has programming ligatures (`liga`/`calt`, the features the run shaper turns on), keyed by [`normalize_family`].
    pub ligatures: std::collections::BTreeMap<String, bool>,
}

impl MonospaceFamilies {
    /// Whether `family`, as a person writes it in `font_family`, draws programming ligatures: `Some(false)` for a listed family whose regular face has neither `liga` nor `calt` (Menlo, Monaco), `None` for a path or a name this list does not hold, which is not a guess to make.
    #[must_use]
    pub fn has_ligatures(&self, family: &str) -> Option<bool> {
        self.ligatures
            .get(&normalize_family(family.trim()))
            .copied()
    }

    /// How this list spells `family` (`menlo` is `Menlo`), when it lists it.
    #[must_use]
    pub fn listed_family(&self, family: &str) -> Option<&str> {
        let key = normalize_family(family.trim());
        self.installed
            .iter()
            .chain(&self.bundled)
            .find(|listed| normalize_family(listed) == key)
            .map(String::as_str)
    }
}

type MonospaceMemo = Option<(Arc<FontFilesMemo>, Vec<String>, Arc<MonospaceFamilies>)>;

/// [`MonospaceFamilies`] for this machine.
#[must_use]
pub fn monospace_families() -> Arc<MonospaceFamilies> {
    static MEMO: std::sync::Mutex<MonospaceMemo> = std::sync::Mutex::new(None);
    memoized_monospace_families(
        &crate::font_search_dirs(),
        &crate::primary_font_candidate_paths(),
        &MEMO,
    )
}

fn memoized_monospace_families(
    roots: &[PathBuf],
    automatic_candidates: &[String],
    cache: &std::sync::Mutex<MonospaceMemo>,
) -> Arc<MonospaceFamilies> {
    let (inventory, _) = memoized_font_inventory_walked(roots);
    let memo = |memo: &MonospaceMemo| {
        memo.as_ref()
            .filter(|(seen, automatic, _)| {
                Arc::ptr_eq(seen, &inventory) && automatic.as_slice() == automatic_candidates
            })
            .map(|(_, _, families)| Arc::clone(families))
    };
    if let Some(families) = memo(
        &cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    ) {
        return families;
    }
    let families = Arc::new(monospace_families_in(
        &inventory.files,
        automatic_candidates,
    ));
    *cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
        inventory,
        automatic_candidates.to_vec(),
        Arc::clone(&families),
    ));
    families
}

/// [`MonospaceFamilies`] over `files`, with the default face the first of `automatic_candidates` that loads.
#[must_use]
pub fn monospace_families_in(
    files: &[PathBuf],
    automatic_candidates: &[String],
) -> MonospaceFamilies {
    let bundled = crate::bundled::selectable_families()
        .iter()
        .map(|family| (*family).to_string())
        .collect::<Vec<_>>();
    monospace_families_with_bundled(files, automatic_candidates, &bundled)
}

fn monospace_families_with_bundled(
    files: &[PathBuf],
    automatic_candidates: &[String],
    bundled: &[String],
) -> MonospaceFamilies {
    use std::collections::BTreeMap;

    let mut installed: BTreeMap<String, String> = BTreeMap::new();
    let mut ligatures: BTreeMap<String, bool> = BTreeMap::new();
    let mut styled: [BTreeMap<String, String>; 3] = Default::default();
    let mut symbol: BTreeMap<String, String> = BTreeMap::new();
    let mut emoji: BTreeMap<String, String> = BTreeMap::new();
    let mut bytes = Vec::new();
    for path in files {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.to_ascii_lowercase().contains("emoji") {
            emoji.insert(normalize_family(stem), stem.to_string());
            continue;
        }
        if font_file::read_bounded_font_file_into(
            path,
            super::NAME_SCAN_MAX_BYTES as usize,
            &mut bytes,
        )
        .is_err()
        {
            continue;
        }
        let faces = aterm_font::fonts_in_collection(&bytes).unwrap_or(1).max(1);
        for index in 0..faces {
            let Ok(face) = aterm_font::Face::parse(&bytes, index) else {
                continue;
            };
            let Some(family) = face_family(&face) else {
                continue;
            };
            let key = normalize_family(&family);
            if face_is_terminal_text(&face) {
                let italic = face.is_italic() || face.is_oblique();
                let bold = face.is_bold() || face.weight().to_number() >= 600;
                if !bold && !italic {
                    ligatures.entry(key.clone()).or_insert_with(|| {
                        crate::ligature_shaping::face_has_ligature_features(&face)
                    });
                    installed.insert(key, family);
                } else if let Some(full) = face_full_name(&face) {
                    let slot = match (bold, italic) {
                        (true, false) => 0,
                        (false, true) => 1,
                        _ => 2,
                    };
                    styled[slot].insert(normalize_family(&full), full);
                }
            } else if face.glyph_index('\u{e0a0}').is_some()
                || family.to_ascii_lowercase().contains("symbol")
            {
                symbol.insert(key, family);
            }
        }
    }
    let bundled: Vec<String> = bundled
        .iter()
        .filter(|family| !installed.contains_key(&normalize_family(family)))
        .cloned()
        .collect();
    for family in &bundled {
        if let Some((_, bytes)) = crate::bundled::face_for_family(family) {
            ligatures.insert(
                normalize_family(family),
                crate::ligature_shaping::font_has_ligature_features(bytes, 0),
            );
        }
    }
    let installed = installed.into_values().collect();
    let (automatic, automatic_has_ligatures) = automatic_candidates
        .iter()
        .find_map(|candidate| automatic_face(candidate))
        .map_or((None, false), |(family, ligatures)| {
            (Some(family), ligatures)
        });
    let [bold, italic, bold_italic] = styled.map(|faces| faces.into_values().collect());
    MonospaceFamilies {
        automatic,
        automatic_has_ligatures,
        bundled,
        installed,
        bold,
        italic,
        bold_italic,
        symbol: symbol.into_values().collect(),
        emoji: emoji.into_values().collect(),
        ligatures,
    }
}

/// The family and ligature support of the default face at `candidate`, a
/// path or a bundled identity, when it loads: the regular face of a
/// collection.
fn automatic_face(candidate: &str) -> Option<(String, bool)> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    let bytes = crate::read_resolved_font(candidate).ok()?;
    let (_, index) = crate::best_face(&bytes, None, FaceTarget::REGULAR)?;
    let face = aterm_font::Face::parse(&bytes, index).ok()?;
    let family = match candidate.strip_prefix(crate::bundled::SCHEME) {
        Some(bundled) if crate::bundled::is_bundled_id(candidate) => bundled.to_string(),
        _ => face_name(&face, TYPOGRAPHIC_FAMILY).or_else(|| face_name(&face, FAMILY))?,
    };
    Some((
        crate::public_face_name(&family).into_owned(),
        crate::ligature_shaping::face_has_ligature_features(&face),
    ))
}

/// Fixed pitch with the Latin letters and digits terminal text needs, so a
/// symbols-only monospace face is not offered as a terminal font.
fn face_is_terminal_text(face: &aterm_font::Face<'_>) -> bool {
    let advance = |ch: char| {
        face.glyph_index(ch)
            .and_then(|glyph| face.glyph_hor_advance(glyph))
    };
    let letters = ['a', 'M', 'i', 'W', '0'];
    if letters.iter().any(|ch| face.glyph_index(*ch).is_none()) {
        return false;
    }
    face.is_monospaced() || {
        let first = advance('M');
        first.is_some_and(|width| width > 0) && letters.iter().all(|ch| advance(*ch) == first)
    }
}

/// The face's family name: the typographic family where it has one (the
/// whole family across weights), else the legacy family, under its public
/// spelling (`.SF NS Mono` is `SF Mono`, [`crate::public_face_name`]); `None`
/// for a hidden system face no public family stands for (`.LastResort`).
fn face_family(face: &aterm_font::Face<'_>) -> Option<String> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    face_name(face, TYPOGRAPHIC_FAMILY)
        .or_else(|| face_name(face, FAMILY))
        .filter(|family| !crate::is_hidden_face_name(family))
        .map(|family| crate::public_face_name(&family).into_owned())
}

/// The face's full name (`JetBrains Mono Bold`), else its PostScript name,
/// under its public spelling: both are names the resolvers find this face by.
fn face_full_name(face: &aterm_font::Face<'_>) -> Option<String> {
    use aterm_font::name_id::{FULL_NAME, POST_SCRIPT_NAME};
    face_name(face, FULL_NAME)
        .or_else(|| face_name(face, POST_SCRIPT_NAME))
        .filter(|name| !crate::is_hidden_face_name(name))
        .map(|name| crate::public_face_name(&name).into_owned())
}

/// One `name` record, English where the face has it.
pub(crate) fn face_name(face: &aterm_font::Face<'_>, id: u16) -> Option<String> {
    let mut any = None;
    for name in face.names() {
        if name.name_id != id {
            continue;
        }
        let Some(text) = crate::name_record_text(&name) else {
            continue;
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let english = match name.platform_id {
            aterm_font::PlatformId::Windows => name.language_id == 0x0409,
            aterm_font::PlatformId::Macintosh => name.language_id == 0,
            _ => false,
        };
        if english {
            return Some(text);
        }
        any.get_or_insert(text);
    }
    any
}

/// A `.ttc` collection of `faces`, in order: the TTC header, then each face's own file with its table offsets rebased to where it lands.
#[doc(hidden)]
#[must_use]
pub fn ttc_of(faces: &[&[u8]]) -> Vec<u8> {
    let header = 12 + 4 * faces.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(faces.len() as u32).to_be_bytes());
    let mut base = header.next_multiple_of(4);
    let mut bodies = Vec::new();
    for face in faces {
        out.extend_from_slice(&(base as u32).to_be_bytes());
        let mut body = face.to_vec();
        let tables = usize::from(u16::from_be_bytes([body[4], body[5]]));
        for record in (0..tables).map(|table| 12 + 16 * table) {
            let offset = u32::from_be_bytes(body[record + 8..record + 12].try_into().unwrap());
            body[record + 8..record + 12].copy_from_slice(&(offset + base as u32).to_be_bytes());
        }
        body.resize(body.len().next_multiple_of(4), 0);
        base += body.len();
        bodies.push(body);
    }
    out.resize(header.next_multiple_of(4), 0);
    for body in bodies {
        out.extend_from_slice(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk keeps no seen-set, so a search root nested inside another would list (and every
    /// full-read consumer read) its fonts twice.
    #[test]
    fn search_dirs_are_never_nested_so_the_walk_visits_a_file_once() {
        let dirs = crate::font_search_dirs();
        for (i, outer) in dirs.iter().enumerate() {
            for (j, inner) in dirs.iter().enumerate() {
                assert!(
                    i == j || !inner.starts_with(outer),
                    "font search dir {} lies inside {} — the recursive walk already                      visits it, and listing it as a root walks every file under it twice",
                    inner.display(),
                    outer.display()
                );
            }
        }
    }

    const JETBRAINS: &[u8] = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");

    /// The fixture face with its OS/2 weight class set to `weight`: a bold
    /// sibling that names the same family.
    fn with_weight(weight: u16) -> Vec<u8> {
        let mut bytes = JETBRAINS.to_vec();
        let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let record = (0..tables)
            .map(|index| 12 + 16 * index)
            .find(|record| &bytes[*record..*record + 4] == b"OS/2")
            .expect("the fixture has an OS/2 table");
        let offset =
            u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
        bytes[offset + 4..offset + 6].copy_from_slice(&weight.to_be_bytes());
        bytes
    }

    /// The picker's list comes from the same files and names the resolvers
    /// use: an installed monospace family, its bold sibling by full name, a
    /// colour emoji face by file name, and the default face's family.
    #[test]
    fn monospace_families_list_what_the_resolvers_find() {
        let root = fixture("families");
        std::fs::write(root.join("JetBrainsMono-Regular.ttf"), JETBRAINS).unwrap();
        std::fs::write(root.join("JetBrainsMono-Bold.ttf"), with_weight(700)).unwrap();
        std::fs::write(root.join("NotoColorEmoji.ttf"), b"not read").unwrap();
        std::fs::write(root.join("notes.txt"), b"not a font").unwrap();
        let files = Catalog::scan(std::slice::from_ref(&root), Limits::default()).files;
        let regular = root.join("JetBrainsMono-Regular.ttf");
        let families = monospace_families_in(&files, &[regular.to_string_lossy().into_owned()]);
        assert!(
            families
                .installed
                .iter()
                .any(|family| family == "JetBrains Mono"),
            "{families:?}"
        );
        assert_eq!(families.automatic.as_deref(), Some("JetBrains Mono"));
        assert!(families.automatic_has_ligatures);
        assert_eq!(families.emoji, ["NotoColorEmoji"]);
        assert_eq!(families.bold.len(), 1, "{families:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The fixture face renamed `name` wherever its `name` table says
    /// "JetBrains Mono" (family, full and typographic names), padded with
    /// spaces to the same length, which the name readers trim.
    fn with_family(name: &str) -> Vec<u8> {
        const FROM: &str = "JetBrains Mono";
        let padded = format!("{name:<14}");
        assert_eq!(padded.chars().count(), FROM.chars().count(), "{name:?}");
        let utf16 =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
        let mut bytes = JETBRAINS.to_vec();
        for (from, to) in [
            (utf16(FROM), utf16(&padded)),
            (FROM.as_bytes().to_vec(), padded.as_bytes().to_vec()),
        ] {
            let mut at = 0;
            while at + from.len() <= bytes.len() {
                if bytes[at..at + from.len()] == from[..] {
                    bytes[at..at + from.len()].copy_from_slice(&to);
                    at += from.len();
                } else {
                    at += 1;
                }
            }
        }
        bytes
    }

    /// A system face that hides its name (a leading dot) is offered only under
    /// the public family it stands for: `.SF NS Mono` is SF Mono.
    #[test]
    fn a_hidden_system_face_is_listed_only_under_a_public_family() {
        let root = fixture("hidden");
        std::fs::write(root.join("Placeholder.ttf"), with_family(".LastResort")).unwrap();
        std::fs::write(root.join("SFNSMono.ttf"), with_family(".SF NS Mono")).unwrap();
        let files = Catalog::scan(std::slice::from_ref(&root), Limits::default()).files;
        let families = monospace_families_in(&files, &[]);
        assert_eq!(families.installed, ["SF Mono"], "{families:?}");
        assert!(crate::is_hidden_face_name(".LastResort"));
        assert!(!crate::is_hidden_face_name(".SF NS Mono"));
        assert_eq!(crate::public_face_name(".SF NS Mono"), "SF Mono");
        assert_eq!(crate::public_face_name(".LastResort"), ".LastResort");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two files name the same family and the bold one sorts first: the
    /// family resolves to its regular face.
    #[test]
    fn a_family_resolves_to_its_regular_face_not_the_first_file() {
        let root = fixture("regular-face");
        std::fs::write(root.join("JetBrainsMono-Bold.ttf"), with_weight(700)).unwrap();
        std::fs::write(root.join("JetBrainsMono-Regular.ttf"), JETBRAINS).unwrap();
        let batch = resolve_and_admit_in_dirs(
            &[("JetBrains Mono".to_string(), FaceTarget::REGULAR)],
            std::slice::from_ref(&root),
            Limits::default(),
        );
        let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
        assert!(
            admitted.path.ends_with("JetBrainsMono-Regular.ttf"),
            "resolved {}",
            admitted.path
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A family installed as a variable face plus links that name its weights
    /// (Ubuntu's `fonts-ubuntu`: `UbuntuMono-B.ttf` and `-R.ttf` link to
    /// `UbuntuMono[wght].ttf`) resolves to the file itself: every one of them
    /// is the regular default instance, and the link that sorts first named a
    /// Bold file as the regular face.
    #[cfg(unix)]
    #[test]
    fn a_family_resolves_to_its_file_not_a_link_that_sorts_first() {
        let root = fixture("linked-weights");
        let file = root.join("JetBrainsMono[wght].ttf");
        std::fs::write(&file, JETBRAINS).unwrap();
        for alias in ["JetBrainsMono-B.ttf", "JetBrainsMono-R.ttf"] {
            std::os::unix::fs::symlink("JetBrainsMono[wght].ttf", root.join(alias)).unwrap();
        }
        let request = [("JetBrains Mono".to_string(), FaceTarget::REGULAR)];
        let batch =
            resolve_and_admit_in_dirs(&request, std::slice::from_ref(&root), Limits::default());
        let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
        assert_eq!(admitted.path, file.to_string_lossy());
        assert_eq!(
            super::super::resolve_by_name_table_in(
                Catalog::scan(std::slice::from_ref(&root), Limits::default()).files,
                "jetbrainsmono",
                FaceTarget::REGULAR,
            )
            .map(|face| face.path)
            .as_deref(),
            Some(&*file.to_string_lossy()),
            "the by-name resolver `show-face` reports agrees"
        );

        std::fs::rename(&file, root.join("elsewhere.bin")).unwrap();
        for alias in ["JetBrainsMono-B.ttf", "JetBrainsMono-R.ttf"] {
            std::fs::remove_file(root.join(alias)).unwrap();
            std::os::unix::fs::symlink("elsewhere.bin", root.join(alias)).unwrap();
        }
        let batch =
            resolve_and_admit_in_dirs(&request, std::slice::from_ref(&root), Limits::default());
        let admitted = batch
            .get(0)
            .unwrap()
            .as_ref()
            .expect("a link still resolves");
        assert!(
            admitted.path.ends_with("JetBrainsMono-B.ttf"),
            "{}",
            admitted.path
        );
        std::fs::remove_dir_all(&root).unwrap();

        let store = fixture("linked-weights-store");
        let (bold, regular) = (
            store.join("JetBrainsMono-Bold.ttf"),
            store.join("JetBrainsMono-Regular.ttf"),
        );
        std::fs::write(&bold, with_weight(700)).unwrap();
        std::fs::write(&regular, JETBRAINS).unwrap();
        let profile = fixture("linked-weights-profile");
        for link in ["a.ttf", "c.ttf", "d.ttf", "e.ttf"] {
            std::os::unix::fs::symlink(&bold, profile.join(link)).unwrap();
        }
        let link = profile.join("b.ttf");
        let stow = Path::new("..")
            .join(store.file_name().unwrap())
            .join("JetBrainsMono-Regular.ttf");
        for target in [regular.clone(), stow] {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let batch = resolve_and_admit_in_dirs(
                &request,
                std::slice::from_ref(&profile),
                Limits::default(),
            );
            let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
            assert_eq!(admitted.path, link.to_string_lossy(), "{target:?}");
            assert_eq!(
                batch.stats.name_files, 2,
                "a link to {target:?} is the regular face's only path here, so it \
                 ends the search; the fonts after it were read"
            );
            assert_eq!(
                super::super::resolve_by_name_table_in(
                    Catalog::scan(std::slice::from_ref(&profile), Limits::default()).files,
                    "jetbrainsmono",
                    FaceTarget::REGULAR,
                )
                .map(|face| face.path)
                .as_deref(),
                Some(&*link.to_string_lossy()),
                "{target:?}"
            );
        }
        std::fs::remove_dir_all(&profile).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn a_regular_face_after_a_nested_directory_still_wins_its_directory() {
        let user = fixture("regular-interleaved-user");
        let system = fixture("regular-interleaved-system");
        let bold = user.join("A-Bold.ttf");
        let regular = user.join("Z-Regular.ttf");
        let nested = user.join("M-vendor");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&bold, with_weight(700)).unwrap();
        std::fs::write(nested.join("Regular.ttf"), JETBRAINS).unwrap();
        std::fs::write(&regular, JETBRAINS).unwrap();
        std::fs::write(system.join("Regular.ttf"), JETBRAINS).unwrap();
        let roots = [user.clone(), system.clone()];
        let request = [("JetBrains Mono".to_string(), FaceTarget::REGULAR)];
        let winner = |batch: &Batch| batch.get(0).unwrap().as_ref().unwrap().path.clone();
        let batch = resolve_and_admit_in_dirs(&request, &roots, Limits::default());
        assert_eq!(
            winner(&batch),
            regular.to_string_lossy().into_owned(),
            "descending into another directory must not finalize the first bold face"
        );

        std::fs::remove_file(&regular).unwrap();
        let batch = resolve_and_admit_in_dirs(&request, &roots, Limits::default());
        assert_eq!(winner(&batch), bold.to_string_lossy().into_owned());
        std::fs::remove_dir_all(user).unwrap();
        std::fs::remove_dir_all(system).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_family_memo_observes_an_atomic_replacement_at_the_same_path() {
        let root = fixture("family-replacement");
        let path = root.join("Regular.ttf");
        let replacement = root.join(".replacement");
        std::fs::write(&path, JETBRAINS).unwrap();
        std::fs::write(&replacement, with_weight(700)).unwrap();
        std::fs::File::open(&root)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800),
                ),
            )
            .unwrap();
        let before_mtime = dir_mtime(&root).unwrap();
        let roots = [root.clone()];
        let automatic = [path.to_string_lossy().into_owned()];
        let memo = std::sync::Mutex::new(None);
        let first = memoized_monospace_families(&roots, &automatic, &memo);
        assert_eq!(first.automatic.as_deref(), Some("JetBrains Mono"));
        assert!(first.bold.is_empty());
        let again = memoized_monospace_families(&roots, &automatic, &memo);
        assert_eq!(
            *again, *first,
            "an unchanged inventory preserves its answer"
        );
        let before_files = memoized_font_files(&roots);

        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(dir_mtime(&root).unwrap(), before_mtime);
        let (after_files, walked) = memoized_font_files_walked(&roots);
        assert!(walked, "the existing directory freshness path re-walks");
        assert_eq!(before_files, after_files, "no font pathname changed");
        let refreshed = memoized_monospace_families(&roots, &automatic, &memo);
        assert_eq!(
            refreshed.bold.len(),
            1,
            "the replacement is now a bold face despite the identical file list"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    const JB_REGULAR: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
    const JB_BOLD: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Bold.ttf");
    const JB_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
    const JB_BOLD_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-BoldItalic.ttf");

    /// The face `request` resolves to among `roots`: its file name and index.
    fn resolved_face(roots: &[PathBuf], request: &str, target: FaceTarget) -> (String, u32) {
        let batch =
            resolve_and_admit_in_dirs(&[(request.to_string(), target)], roots, Limits::default());
        let admitted = batch
            .get(0)
            .unwrap()
            .as_ref()
            .unwrap_or_else(|error| panic!("{request}: {error}"));
        let name = Path::new(&admitted.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (name, admitted.index)
    }

    /// `PTMono.ttc`'s shape: Bold at face 0, Regular at face 1. A family is
    /// its Regular face, a face's own name is that face, and a family asked
    /// for its bold slot is its Bold — through the name pass (a file named
    /// unlike the family) and through the stem pass alike.
    #[test]
    fn a_collection_resolves_each_request_to_its_own_face() {
        let collection = ttc_of(&[JB_BOLD, JB_REGULAR]);
        let by_name = fixture("collection-by-name");
        std::fs::write(by_name.join("Mono.ttc"), &collection).unwrap();
        let by_stem = fixture("collection-by-stem");
        std::fs::write(by_stem.join("JetBrainsMono.ttc"), &collection).unwrap();
        for root in [&by_name, &by_stem] {
            let roots = std::slice::from_ref(root);
            assert_eq!(
                resolved_face(roots, "JetBrains Mono", FaceTarget::REGULAR).1,
                1,
                "the family is its Regular face ({})",
                root.display()
            );
            assert_eq!(
                resolved_face(roots, "JetBrains Mono", FaceTarget::BOLD).1,
                0,
                "the bold slot takes the family's Bold"
            );
            assert_eq!(
                resolved_face(roots, "JetBrains Mono Bold", FaceTarget::REGULAR).1,
                0,
                "a face's full name is that face, whatever the slot"
            );
            assert_eq!(
                resolved_face(roots, "JetBrainsMono-Regular", FaceTarget::BOLD).1,
                1,
                "a face's PostScript name is that face"
            );
        }
        let (_, index) = resolved_face(
            std::slice::from_ref(&by_name),
            "JetBrains Mono",
            FaceTarget::REGULAR,
        );
        let face = aterm_font::Face::parse(&collection, index).unwrap();
        assert_eq!(face.weight().to_number(), 400);
        let _ = std::fs::remove_dir_all(by_name);
        let _ = std::fs::remove_dir_all(by_stem);
    }

    /// `Menlo.ttc`'s shape: Regular, Bold, Italic, Bold Italic in one file.
    #[test]
    fn every_face_of_a_collection_is_listed_under_its_style_and_resolves_back() {
        let root = fixture("collection-listing");
        std::fs::write(
            root.join("Mono.ttc"),
            ttc_of(&[JB_REGULAR, JB_BOLD, JB_ITALIC, JB_BOLD_ITALIC]),
        )
        .unwrap();
        let files = Catalog::scan(std::slice::from_ref(&root), Limits::default()).files;
        let families = monospace_families_with_bundled(&files, &[], &[]);
        assert_eq!(families.installed, ["JetBrains Mono"], "{families:?}");
        assert_eq!(families.bold, ["JetBrains Mono Bold"], "{families:?}");
        assert_eq!(families.italic, ["JetBrains Mono Italic"], "{families:?}");
        assert_eq!(
            families.bold_italic,
            ["JetBrains Mono Bold Italic"],
            "{families:?}"
        );
        let roots = std::slice::from_ref(&root);
        for (slot, (name, index)) in [
            (&families.bold[0], 1),
            (&families.italic[0], 2),
            (&families.bold_italic[0], 3),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                resolved_face(roots, name, FaceTarget::for_slot(slot)),
                ("Mono.ttc".to_string(), index),
                "{name}"
            );
            assert_eq!(
                resolved_face(roots, "JetBrains Mono", FaceTarget::for_slot(slot)).1,
                index
            );
        }

        let bold_first = fixture("collection-bold-first");
        std::fs::write(bold_first.join("Mono.ttc"), ttc_of(&[JB_BOLD, JB_REGULAR])).unwrap();
        let files = Catalog::scan(std::slice::from_ref(&bold_first), Limits::default()).files;
        let families = monospace_families_with_bundled(&files, &[], &[]);
        assert_eq!(families.installed, ["JetBrains Mono"], "{families:?}");
        assert_eq!(families.bold, ["JetBrains Mono Bold"], "{families:?}");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(bold_first);
    }

    /// The fixture face with its `GSUB` table record renamed, so the face
    /// advertises no ligature feature (the record stays in tag order).
    fn without_gsub(bytes: &[u8]) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let record = (0..tables)
            .map(|index| 12 + 16 * index)
            .find(|record| &bytes[*record..*record + 4] == b"GSUB")
            .expect("the fixture has a GSUB table");
        bytes[record + 3] = b'c';
        bytes
    }

    /// Every listed family records whether its regular face has programming
    /// ligatures, so Settings can say the Ligatures switch does nothing for a
    /// chosen family without them — not only for the automatic face.
    #[test]
    fn every_listed_family_records_its_ligatures() {
        let with = fixture("ligatures-with");
        std::fs::write(with.join("Regular.ttf"), JETBRAINS).unwrap();
        let without = fixture("ligatures-without");
        std::fs::write(without.join("Regular.ttf"), without_gsub(JETBRAINS)).unwrap();
        let families_in = |root: &PathBuf, bundled: &[String]| {
            let files = Catalog::scan(std::slice::from_ref(root), Limits::default()).files;
            monospace_families_with_bundled(&files, &[], bundled)
        };
        let has = families_in(&with, &[]);
        assert_eq!(has.has_ligatures("JetBrains Mono"), Some(true));
        let lacks = families_in(&without, &[]);
        assert_eq!(lacks.has_ligatures("jetbrains-mono"), Some(false));
        assert_eq!(lacks.has_ligatures("Not A Listed Family"), None);
        if crate::bundled::face_for_family(crate::bundled::IOSEVKA_FAMILY).is_some() {
            let bundled = families_in(&without, &[crate::bundled::IOSEVKA_FAMILY.to_string()]);
            assert_eq!(
                bundled.has_ligatures(crate::bundled::IOSEVKA_FAMILY),
                Some(true)
            );
        }
        let _ = std::fs::remove_dir_all(with);
        let _ = std::fs::remove_dir_all(without);
    }

    /// A name that sits in a file the remaining name-read budget cannot hold
    /// is refused as "not found before discovery exhausted …": the generation
    /// could not check it.
    #[test]
    fn a_name_beyond_the_remaining_budget_says_the_budget_ran_out() {
        let root = fixture("name-budget");
        let first = without_gsub(JETBRAINS);
        let second = with_family("Budget Mono");
        assert!(crate::bundled::face_for_family("Budget Mono").is_none());
        std::fs::write(root.join("A.ttf"), &first).unwrap();
        std::fs::write(root.join("B.ttf"), &second).unwrap();
        let resolve = |max_name_bytes| {
            resolve_and_admit_in_dirs(
                &[("Budget Mono".to_string(), FaceTarget::REGULAR)],
                std::slice::from_ref(&root),
                Limits {
                    max_name_bytes,
                    ..Limits::default()
                },
            )
        };
        let enough = resolve(first.len() + second.len());
        let found = enough.get(0).unwrap().as_ref().unwrap();
        assert!(found.path.ends_with("B.ttf"), "{}", found.path);
        let batch = resolve(first.len() + second.len() - 1);
        let error = batch.get(0).unwrap().as_ref().unwrap_err();
        assert!(error.contains("exhausted"), "{error}");
        assert!(
            batch.stats.exhausted.contains("name bytes"),
            "{:?}",
            batch.stats
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_installed_family_remains_installed_when_it_also_is_bundled() {
        let root = fixture("installed-and-bundled");
        std::fs::write(root.join("Regular.ttf"), JETBRAINS).unwrap();
        let files = Catalog::scan(std::slice::from_ref(&root), Limits::default()).files;
        let families = monospace_families_with_bundled(
            &files,
            &[],
            &[
                "JetBrains Mono".to_string(),
                "Other Bundled Face".to_string(),
            ],
        );
        assert!(families.installed.iter().any(|f| f == "JetBrains Mono"));
        assert_eq!(families.bundled, ["Other Bundled Face"]);
        std::fs::remove_dir_all(root).unwrap();
    }

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

    /// An unchanged tree is answered from the memo; a font added in a nested vendor directory
    /// (the Linux layout, which a root-mtime key would miss) or an absent root appearing is a
    /// miss.
    #[test]
    fn the_font_files_memo_is_revalidated_by_the_visited_directories_mtimes() {
        let root = fixture("memo");
        let vendor = root.join("truetype").join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("a.ttf"), b"a").unwrap();
        let absent_root = fixture("memo-later");
        std::fs::remove_dir_all(&absent_root).unwrap();
        let roots = vec![root.clone(), absent_root.clone()];

        let (first, walked) = memoized_font_files_walked(&roots);
        assert_eq!(first, vec![vendor.join("a.ttf")]);
        assert!(walked, "a cold ask walks");

        let (again, walked) = memoized_font_files_walked(&roots);
        assert_eq!(again, first);
        assert!(!walked, "an unchanged tree is answered from the memo");

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(vendor.join("b.ttf"), b"b").unwrap();
        let (grown, walked) = memoized_font_files_walked(&roots);
        assert_eq!(grown, vec![vendor.join("a.ttf"), vendor.join("b.ttf")]);
        assert!(walked, "an entry added deep in the tree is a miss");

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir_all(&absent_root).unwrap();
        std::fs::write(absent_root.join("c.otf"), b"c").unwrap();
        let (with_root, walked) = memoized_font_files_walked(&roots);
        assert_eq!(
            with_root,
            vec![
                vendor.join("a.ttf"),
                vendor.join("b.ttf"),
                absent_root.join("c.otf")
            ]
        );
        assert!(walked, "a root appearing is a miss");

        let (remembered, walked) = memoized_font_files_walked(&roots);
        assert_eq!(remembered, with_root);
        assert!(!walked, "and the new state is remembered");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(absent_root).unwrap();
    }

    /// Concurrent asks for one root list: exactly one caller walks and the rest get its list.
    #[test]
    fn concurrent_asks_for_one_tree_walk_it_once() {
        let root = fixture("single-flight");
        for i in 0..64 {
            let dir = root.join(format!("d{i:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("f.ttf"), b"f").unwrap();
        }
        let roots = vec![root.clone()];
        let start = std::sync::Barrier::new(8);
        let answers: Vec<(Vec<PathBuf>, bool)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        memoized_font_files_walked(&roots)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let walks = answers.iter().filter(|(_, walked)| *walked).count();
        assert_eq!(walks, 1, "{walks} concurrent callers walked the same tree");
        assert!(answers.iter().all(|(files, _)| files.len() == 64));
        assert!(answers.windows(2).all(|w| w[0].0 == w[1].0));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_scan_resolves_duplicates_and_shares_admitted_bytes() {
        let root = fixture("batch");
        let path = root.join("Example-Mono.ttf");
        std::fs::write(&path, b"font bytes").unwrap();
        let requests = vec![
            ("Example Mono".to_string(), FaceTarget::REGULAR),
            ("ExampleMono".to_string(), FaceTarget::REGULAR),
        ];
        let batch =
            resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), Limits::default());
        let first = batch.get(0).unwrap().as_ref().unwrap();
        let second = batch.get(1).unwrap().as_ref().unwrap();
        assert_eq!(first.path, path.to_string_lossy());
        assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
        assert_eq!(batch.stats.dirs, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A bundled family name yields to an installed face of that name (so a variable
    /// `JetBrainsMono[wght].ttf` keeps its weight axis) and falls back to the compiled-in face
    /// only when nothing installed answers; the `bundled:` identity always means the
    /// compiled-in face.
    #[test]
    fn an_installed_face_wins_a_bundled_family_name_and_the_identity_never_yields() {
        if !crate::bundled::ACTIVE {
            return;
        }
        let root = fixture("bundled-name");
        let installed = root.join("JetBrainsMono-Regular.ttf");
        std::fs::write(&installed, b"installed jetbrains mono").unwrap();
        let requests = vec![
            ("JetBrains Mono".to_string(), FaceTarget::REGULAR),
            (
                crate::bundled::JETBRAINS_ID.to_string(),
                FaceTarget::REGULAR,
            ),
        ];
        let batch =
            resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), Limits::default());
        let by_name = batch.get(0).unwrap().as_ref().unwrap();
        assert_eq!(by_name.path, installed.to_string_lossy(), "installed wins");
        assert_eq!(&by_name.bytes[..], b"installed jetbrains mono");
        let by_id = batch.get(1).unwrap().as_ref().unwrap();
        assert_eq!(by_id.path, crate::bundled::JETBRAINS_ID);

        std::fs::remove_file(&installed).unwrap();
        let batch =
            resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), Limits::default());
        for entry in &batch.entries {
            let admitted = entry.result.as_ref().unwrap();
            assert_eq!(
                admitted.path,
                crate::bundled::JETBRAINS_ID,
                "{}",
                entry.requested
            );
            assert_eq!(
                &admitted.bytes[..],
                crate::bundled::face_for_id(crate::bundled::JETBRAINS_ID).unwrap()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Settings' "Automatic (…)" names a bundled default by the family it is
    /// selected by: on Windows `Iosevka Term Semibold`, whose own typographic
    /// family (name ID 16) is `Iosevka Term` — the name of the lighter set
    /// listed beside it — and which draws no ligatures.
    #[test]
    fn a_bundled_default_is_named_as_it_is_selected() {
        if crate::bundled::IOSEVKA_SEMIBOLD_ACTIVE {
            let id = crate::bundled::IOSEVKA_SEMIBOLD_ID;
            let face =
                aterm_font::Face::parse(crate::bundled::face_for_id(id).unwrap(), 0).unwrap();
            assert_eq!(
                face_name(&face, aterm_font::name_id::TYPOGRAPHIC_FAMILY).as_deref(),
                Some(crate::bundled::IOSEVKA_FAMILY),
                "CONTROL: the face's own typographic family"
            );
            assert_eq!(
                automatic_face(id),
                Some((crate::bundled::IOSEVKA_SEMIBOLD_FAMILY.to_string(), false))
            );
        }
        if crate::bundled::ACTIVE {
            assert_eq!(
                automatic_face(crate::bundled::JETBRAINS_ID),
                Some((crate::bundled::JETBRAINS_FAMILY.to_string(), true))
            );
        }
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
        let requests = (0..12)
            .map(|i| (format!("Missing {i}"), FaceTarget::REGULAR))
            .collect::<Vec<_>>();
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
            (a.to_string_lossy().into_owned(), FaceTarget::REGULAR),
            (b.to_string_lossy().into_owned(), FaceTarget::REGULAR),
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

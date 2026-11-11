// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, generation-local font discovery and admission.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::font_file;
use crate::{FaceRef, FaceTarget, NamedInstance};

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

/// One face admitted for a config generation ([`FaceRef`]).
#[derive(Clone, Debug)]
pub struct AdmittedFont {
    pub path: String,
    pub index: u32,
    /// The named instance the request named ([`FaceRef::instance`]).
    pub instance: Option<NamedInstance>,
    pub bytes: Arc<Vec<u8>>,
}

impl AdmittedFont {
    /// The face this admission resolved to.
    #[inline]
    #[must_use]
    pub fn face(&self) -> FaceRef {
        FaceRef::new(self.path.clone(), self.index).with_instance(self.instance.clone())
    }
}

/// Which face of a resolved file a request takes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FaceChoice {
    /// The face at this index, at the named instance the request named: a
    /// `name`-table match, or a compiled-in face.
    At(u32, Option<NamedInstance>),
    /// The face nearest this style, chosen from the admitted bytes: a file
    /// found by its file name or named by path (`crate::best_face`).
    Nearest(FaceTarget),
}

impl FaceChoice {
    fn face(self, bytes: &[u8]) -> (u32, Option<NamedInstance>) {
        match self {
            Self::At(index, instance) => (index, instance),
            Self::Nearest(target) => (
                crate::best_face(bytes, None, target).map_or(0, |(_, index)| index),
                None,
            ),
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
    /// Every directory the walk asked `read_dir` of ([`system_font_files`]).
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
                        .map(|face| (PathBuf::from(face.path), FaceChoice::At(face.index, None)))
                        .ok_or_else(|| format!("{trimmed:?}: no default face loads")),
                );
            } else if trimmed.contains(['/', '\\']) {
                results[index] = Some(Ok((PathBuf::from(trimmed), FaceChoice::Nearest(*target))));
            } else if crate::bundled::is_identity(trimmed)
                && let Some((id, _)) = crate::bundled::face_for_family(trimmed)
            {
                results[index] = Some(Ok((PathBuf::from(id), FaceChoice::At(0, None))));
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

        type Settling = ((u32, bool), PathBuf, u32, Option<NamedInstance>);
        let mut settling: HashMap<Want, Settling> = HashMap::new();
        let settle =
            |want: &Want,
             path: &Path,
             face: u32,
             instance: &Option<NamedInstance>,
             results: &mut Vec<Option<Result<(PathBuf, FaceChoice), String>>>| {
                if let Some(indexes) = wants.get(want) {
                    for &index in indexes {
                        results[index] = Some(Ok((
                            path.to_path_buf(),
                            FaceChoice::At(face, instance.clone()),
                        )));
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
                        .any(|(_, best, _, _)| best.parent() == path.parent())
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
                    let len = std::fs::symlink_metadata(&path)
                        .ok()
                        .filter(std::fs::Metadata::is_file)
                        .map_or(0, |meta| meta.len());
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
                let matches: Vec<(Want, u32, u32, Option<NamedInstance>)> = unresolved
                    .iter()
                    .chain(
                        settling
                            .iter()
                            .filter(|(_, (_, best, _, _))| best.parent() == path.parent())
                            .map(|(want, _)| want),
                    )
                    .filter_map(|want| {
                        crate::best_named_face(&bytes, Some(&want.0), want.1).map(
                            |(distance, face, instance)| (want.clone(), distance, face, instance),
                        )
                    })
                    .collect();
                if !matches.is_empty() {
                    self.bytes.insert(path.clone(), Arc::clone(&bytes));
                }
                for (want, distance, face, instance) in matches {
                    unresolved.remove(&want);
                    let rank = super::name_match_rank(distance, &path);
                    let held = settling.entry(want).or_insert((
                        (u32::MAX, true),
                        path.clone(),
                        face,
                        None,
                    ));
                    if rank < held.0 {
                        *held = (rank, path.clone(), face, instance);
                    }
                }
                settling.retain(|want, (held, best, face, instance)| {
                    let nearest = *held == super::NAME_MATCH_SETTLED;
                    if nearest {
                        settle(want, best, *face, instance, &mut results);
                    }
                    !nearest
                });
            }
        }
        for (want, (_, best, face, instance)) in settling {
            settle(&want, &best, face, &instance, &mut results);
        }

        let truncated = self.stats.truncated();
        for (want, indexes) in wants {
            for index in indexes {
                if results[index].is_none() {
                    let bundled = crate::bundled::face_for_family(requests[index].0.trim())
                        .map(|(id, _)| (PathBuf::from(id), FaceChoice::At(0, None)));
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
            let (index, instance) = choice.face(bytes);
            return Ok(AdmittedFont {
                path: path.to_string_lossy().into_owned(),
                index,
                instance,
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
        let (index, instance) = choice.face(&bytes);
        Ok(AdmittedFont {
            path: path.to_string_lossy().into_owned(),
            index,
            instance,
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

use crate::normalize_family;

/// Resolve and admit every config-authored family/path in one generation ([`FaceTarget`]).
#[must_use]
pub fn resolve_and_admit(requests: &[(String, FaceTarget)]) -> Batch {
    resolve_and_admit_in_dirs(requests, &crate::font_search_dirs(), Limits::default())
}

pub(crate) fn resolve_and_admit_in_dirs(
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
                        instance: None,
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

fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).ok().and_then(|m| m.modified().ok())
}

struct FontFilesMemo {
    roots: Vec<PathBuf>,
    visited: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    files: Vec<PathBuf>,
}

impl FontFilesMemo {
    fn current(&self) -> bool {
        self.visited
            .iter()
            .all(|(dir, seen)| dir_mtime(dir) == *seen)
    }
}

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

/// [`memoized_font_files_walked`] returning the memo itself ([`monospace_families`]).
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

/// Bounded font-file list.
pub(crate) fn system_font_files() -> Vec<PathBuf> {
    memoized_font_files(&crate::font_search_dirs())
}

/// What a font picker offers, every name one the resolvers find again.
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
    /// Whether each listed family's regular face has programming ligatures ([`normalize_family`]).
    pub ligatures: std::collections::BTreeMap<String, bool>,
}

impl MonospaceFamilies {
    /// Whether `family`, as a person writes it in `font_family`, draws programming ligatures.
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

/// [`MonospaceFamilies`] over `files`.
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
                let mut regular = false;
                let mut list = |slot: Option<usize>, full: String| match slot {
                    None => regular = true,
                    Some(slot) => {
                        styled[slot].insert(normalize_family(&full), full);
                    }
                };
                let roles = variable_roles(&face);
                if roles.is_empty() {
                    let (bold, italic) = listed_style(&face);
                    if let Some(full) = face_full_name(&face).filter(|_| bold || italic) {
                        list(style_slot(bold, italic), full);
                    } else if !bold && !italic {
                        list(None, family.clone());
                    }
                } else {
                    for (words, slot) in roles {
                        list(slot, format!("{family} {words}"));
                    }
                    for (full, slot) in named_instance_listings(&face, &family) {
                        list(slot, full);
                    }
                }
                if regular {
                    ligatures.entry(key.clone()).or_insert_with(|| {
                        crate::ligature_shaping::face_has_ligature_features(&face)
                    });
                    installed.insert(key, family);
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

/// The style a picker lists `face` under, `(bold, italic)`.
pub(crate) fn listed_style(face: &aterm_font::Face<'_>) -> (bool, bool) {
    let italic = face.is_italic() || face.is_oblique();
    let bold = face.is_bold() || face.weight().to_number() >= 600;
    (bold, italic)
}

/// The styled slot (0 bold, 1 italic, 2 bold italic) a `(bold, italic)` style is
/// listed in; `None` for the regular style, which a picker lists by family.
const fn style_slot(bold: bool, italic: bool) -> Option<usize> {
    match (bold, italic) {
        (false, false) => None,
        (true, false) => Some(0),
        (false, true) => Some(1),
        (true, true) => Some(2),
    }
}

/// THE ROLES A VARIABLE FACE SERVES, by the style words it is offered and found by
/// and the slot ([`style_slot`]) each is listed in: an upright face is its family's
/// regular and, where its `wght` axis reaches bold, its "Bold"; a slanted face is its
/// "Italic" and, likewise, its "Bold Italic". Drawn in a role, a face draws that role's
/// instance (`Renderer::family_face_instance`), so the name its table gives its `fvar`
/// default (`SF Mono Light Italic`) is not what any role draws, and `SF Mono Bold` is
/// what the bold role draws. Empty for a static face.
pub(crate) fn variable_roles(face: &aterm_font::Face<'_>) -> Vec<(&'static str, Option<usize>)> {
    let Some(probe) = crate::variation::probe_face(face) else {
        return Vec::new();
    };
    let (bold_default, italic) = listed_style(face);
    let (regular, bold) = match probe
        .axes
        .iter()
        .find(|axis| axis.tag == crate::variation::WGHT_TAG)
    {
        Some(wght) => (wght.min < 600.0, wght.max >= 600.0),
        None => (!bold_default, bold_default),
    };
    let mut roles = Vec::new();
    if regular {
        roles.push((
            if italic { "Italic" } else { "Regular" },
            style_slot(false, italic),
        ));
    }
    if bold {
        roles.push((
            if italic { "Bold Italic" } else { "Bold" },
            style_slot(true, italic),
        ));
    }
    roles
}

/// The named instances of variable face `face` a picker offers besides its roles
/// ([`variable_roles`]), each by its family and the instance's name (`SF Mono Light
/// Italic`) with the slot its own weight and slant list it in: every instance a name
/// chooses ([`crate::NamedInstance`]) and no role answers to, so no two names offered
/// draw the same. An upright instance below bold is a weight of the family, which a
/// picker lists by family, as it lists a static family's Light.
fn named_instance_listings(
    face: &aterm_font::Face<'_>,
    family: &str,
) -> Vec<(String, Option<usize>)> {
    let Some(probe) = crate::variation::probe_face(face) else {
        return Vec::new();
    };
    let role_words: Vec<String> = variable_roles(face)
        .into_iter()
        .map(|(words, _)| normalize_family(words))
        .collect();
    let italic = face.is_italic() || face.is_oblique();
    let wght = probe
        .axes
        .iter()
        .position(|axis| axis.tag == crate::variation::WGHT_TAG);
    let mut listed = Vec::new();
    for record in crate::variation::instance_records(face) {
        let Some(subfamily) = face_name(face, record.subfamily_name_id) else {
            continue;
        };
        if role_words.contains(&normalize_family(&subfamily))
            || crate::named_instance_of(face, &record.coords).is_none()
        {
            continue;
        }
        let bold = wght.map_or_else(
            || listed_style(face).0,
            |at| {
                record
                    .design_coords()
                    .get(at)
                    .is_some_and(|&weight| weight >= 600.0)
            },
        );
        if let Some(slot) = style_slot(bold, italic) {
            listed.push((format!("{family} {subfamily}"), Some(slot)));
        }
    }
    listed
}

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

/// The face's family name: the typographic family where it has one ([`crate::public_face_name`]).
fn face_family(face: &aterm_font::Face<'_>) -> Option<String> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    face_name(face, TYPOGRAPHIC_FAMILY)
        .or_else(|| face_name(face, FAMILY))
        .filter(|family| !crate::is_hidden_face_name(family))
        .map(|family| crate::public_face_name(&family).into_owned())
}

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

/// A `.ttc` collection of `faces`, in order.
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
#[path = "font_catalog_tests.rs"]
mod tests;

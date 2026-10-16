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
    /// Every directory the walk asked `read_dir` of — roots included, absent
    /// or unreadable ones too — with the mtime observed at that moment. This
    /// is the validity key of the [`system_font_files`] memo: a directory's
    /// mtime moves when an entry is added, removed or renamed in it, so a font
    /// installed anywhere in the tree, a vendor directory appearing under a
    /// nested Linux root, or a root coming into existence all show up in a
    /// stat of one of these, without walking the tree again.
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
            } else if crate::bundled::is_identity(trimmed)
                && let Some((id, _)) = crate::bundled::face_for_family(trimmed)
            {
                // A bundled IDENTITY is compiled-in (see
                // `resolve_and_admit_in_dirs`): never a name for the filename /
                // name-table passes to hunt for. A bundled family NAME is hunted
                // like any other and falls back to the bundled face below only
                // when nothing installed answers (`resolve_family_or_bundled`).
                results[index] = Some(Ok(PathBuf::from(id)));
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
        // once and compare it against every still-unresolved family. A matched
        // family keeps settling over the files of its first matching directory, so its
        // most regular face wins (`regular_face_score`); finding the regular
        // face itself ends that family's search, so the rest of a crowded
        // directory is not read for it.
        let mut settling: HashMap<String, (u32, PathBuf)> = HashMap::new();
        let settle =
            |want: &str, path: &Path, results: &mut Vec<Option<Result<PathBuf, String>>>| {
                if let Some(indexes) = wants.get(want) {
                    for &index in indexes {
                        results[index] = Some(Ok(path.to_path_buf()));
                    }
                }
            };
        if !unresolved.is_empty() {
            for path in self.files.clone() {
                if unresolved.is_empty() && settling.is_empty() {
                    break;
                }
                // The depth-first walk can leave a directory for a child and
                // then return to a later sibling file. Keep its candidate until
                // the regular face or the end, without reading other directories
                // solely for a family whose winning directory is already known.
                if unresolved.is_empty()
                    && !settling
                        .values()
                        .any(|(_, best)| best.parent() == path.parent())
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
                    continue;
                };
                self.stats.name_files += 1;
                self.stats.name_bytes += raw.len();
                self.stats.aggregate_bytes += raw.len();
                let bytes = Arc::new(raw);
                let matches: Vec<String> = unresolved
                    .iter()
                    .chain(
                        settling
                            .iter()
                            .filter(|(_, (_, best))| best.parent() == path.parent())
                            .map(|(want, _)| want),
                    )
                    .filter(|want| super::font_name_table_matches(&bytes, want))
                    .cloned()
                    .collect();
                if !matches.is_empty() {
                    self.bytes.insert(path.clone(), Arc::clone(&bytes));
                }
                let score = super::regular_face_score(&bytes);
                for want in matches {
                    unresolved.remove(&want);
                    let held = settling.entry(want).or_insert((u32::MAX, path.clone()));
                    if score < held.0 {
                        *held = (score, path.clone());
                    }
                }
                settling.retain(|want, (held, best)| {
                    let regular = *held == 0;
                    if regular {
                        settle(want, best, &mut results);
                    }
                    !regular
                });
            }
        }
        for (want, (_, best)) in settling {
            settle(&want, &best, &mut results);
        }

        let truncated = self.stats.truncated();
        for (want, indexes) in wants {
            for index in indexes {
                if results[index].is_none() {
                    let bundled = crate::bundled::face_for_family(requests[index].trim())
                        .map(|(id, _)| PathBuf::from(id));
                    results[index] = prefix_hits.get(&want).cloned().or(bundled).map(Ok).or_else(|| {
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
            && !crate::bundled::is_identity(request)
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
        .cloned()
        .zip(paths)
        .map(|(requested, result)| {
            // The `display:` scheme resolves to embedded bytes ahead of the system
            // catalogue — the exact interception `from_system_with_family`
            // performs at startup, so the two paths cannot disagree.
            //
            // A bundled face (`JetBrains Mono` on non-macOS, when nothing
            // installed answers the name) resolved to its virtual identity, and
            // is admitted from its compiled-in bytes under that identity so the
            // renderer built from this entry finds its Bold/Italic siblings.
            let result = match crate::display_face_for_family(requested.trim()) {
                Some(bytes) => Ok(AdmittedFont {
                    path: requested.trim().to_string(),
                    bytes: Arc::new(bytes.to_vec()),
                }),
                None => result.and_then(|path| {
                    match path.to_str().and_then(crate::bundled::face_for_id) {
                        Some(bytes) => Ok(AdmittedFont {
                            path: path.to_string_lossy().into_owned(),
                            bytes: Arc::new(bytes.to_vec()),
                        }),
                        None => catalog.admit(&path),
                    }
                }),
            };
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

/// The walks this process remembers, keyed by their root list. In production
/// there is exactly one root list (`font_search_dirs()`), so this is one
/// entry; tests walk private fixture roots and must not evict each other's
/// entries or the real one. Bounded so a pathological caller cannot grow it.
static FONT_FILES_MEMOS: std::sync::Mutex<Vec<Arc<FontFilesMemo>>> =
    std::sync::Mutex::new(Vec::new());
const MAX_FONT_FILES_MEMOS: usize = 8;

/// One walk gate per root list ([`memoized_font_files_walked`]'s single-flight).
/// Bounded like the memos; clearing it only forfeits single-flight for a walk
/// already in progress, never correctness.
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
///
/// A hit costs one `stat` per directory the last walk visited (a few dozen on
/// a Linux tree, a handful on macOS) instead of that many `read_dir`s plus a
/// `stat`/`file_type` per entry (~660 on a Mac) and a sort per directory. The
/// key is exact for the tree's SHAPE: adding, removing or renaming an entry in
/// any visited directory moves that directory's mtime. What it does not see is
/// a file rewritten in place under the same name — which changes no path in
/// the list anyway, and the list is all this function returns. The walk's
/// bounded-work caps apply to the walk, exactly as before; a capped walk is
/// remembered as what it found, like an uncapped one.
///
/// The backend worker resolves the configured family through this on every
/// launch, and `list-fonts` / a config reload / the coverage index used to
/// re-walk the same tree from scratch; now they share the worker's walk until
/// the tree changes.
fn memoized_font_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    memoized_font_files_walked(roots).0
}

/// [`memoized_font_files`] plus whether THIS call walked the tree (`false` =
/// answered from the memo). The proof observable is per call, not a
/// process-global counter: the lib tests run in parallel and any of them may
/// take the real tree's first walk inside another test's window, so a global
/// count is a flake, while this answer is exact for the root list asked about.
fn memoized_font_files_walked(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let (inventory, walked) = memoized_font_inventory_walked(roots);
    (inventory.files.clone(), walked)
}

/// The actual walk generation, shared with derived catalogues. A replacement
/// under an unchanged pathname still produces a new generation when its
/// directory observation changes.
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
    // SINGLE-FLIGHT per root list: the broad and symbol chain builders run on
    // two threads at once at every launch; the second caller waits on the
    // first walk's gate and is answered from its memo instead of walking the
    // same tree again. The walk itself still runs OUTSIDE the memo lock, so a
    // caller asking for a DIFFERENT root list (tests walk private fixture
    // roots) never waits behind it.
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

/// Bounded file list used by renderer diagnostics and runtime discovery. This
/// preserves the stable directory/lexical ordering while removing their former
/// unbounded recursion, entry collection, and file accumulation. Remembered
/// per process and revalidated by directory mtimes ([`memoized_font_files`]),
/// so the second and every later caller pays a few `stat`s, not a walk.
pub(crate) fn system_font_files() -> Vec<PathBuf> {
    memoized_font_files(&crate::font_search_dirs())
}

/// What a font picker offers, every name one the resolvers find again:
/// the face aterm draws when no family is set, the families compiled into
/// aterm, and the installed faces sorted by what they are good for.
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
    /// Installed fixed-pitch bold, italic and bold italic faces, by full
    /// name (`JetBrains Mono Bold`), each sorted.
    pub bold: Vec<String>,
    pub italic: Vec<String>,
    pub bold_italic: Vec<String>,
    /// Installed faces made for symbols and icons (Nerd Font symbols,
    /// `Apple Symbols`).
    pub symbol: Vec<String>,
    /// Installed colour emoji faces, by file name.
    pub emoji: Vec<String>,
}

type MonospaceMemo = Option<(Arc<FontFilesMemo>, Vec<String>, Arc<MonospaceFamilies>)>;

/// [`MonospaceFamilies`] for this machine. It reads every font file the
/// resolvers search (seconds cold), so callers run it off the UI thread;
/// the answer is remembered until a font directory changes.
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

/// [`MonospaceFamilies`] over `files`, with the default face the first of
/// `automatic_candidates` that loads. Only the first face of a collection
/// counts, because that is the face the renderer loads from it, and only a
/// file within the name-table read limit, because the resolvers find no
/// other by name.
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
    let mut styled: [BTreeMap<String, String>; 3] = Default::default();
    let mut symbol: BTreeMap<String, String> = BTreeMap::new();
    let mut emoji: BTreeMap<String, String> = BTreeMap::new();
    let mut bytes = Vec::new();
    for path in files {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        // Colour emoji faces are too large to read by name; their file
        // name is the name the resolvers find them by.
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
        let Ok(face) = aterm_font::Face::parse(&bytes, 0) else {
            continue;
        };
        let Some(family) = face_family(&face) else {
            continue;
        };
        let key = normalize_family(&family);
        if face_is_terminal_text(&face) {
            let italic = face.is_italic() || face.is_oblique();
            let bold = face.weight().to_number() >= 600;
            if !bold && !italic {
                installed.insert(key, family);
            } else if let Some(full) = face_full_name(&face, stem) {
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
    // A bundled family an installed one already answers is listed once,
    // where it is installed.
    let bundled: Vec<String> = bundled
        .iter()
        .filter(|family| !installed.contains_key(&normalize_family(family)))
        .cloned()
        .collect();
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
    }
}

/// The family and ligature support of the default face at `candidate`, a
/// path or a bundled identity, when it loads. A system face that hides its
/// name (`.SF NS Mono`) is called what people call it (`SF Mono`).
fn automatic_face(candidate: &str) -> Option<(String, bool)> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    let bytes = crate::read_resolved_font(candidate).ok()?;
    let face = aterm_font::Face::parse(&bytes, 0).ok()?;
    let family = face_name(&face, TYPOGRAPHIC_FAMILY).or_else(|| face_name(&face, FAMILY))?;
    let family = match family.strip_prefix('.') {
        Some(hidden) => hidden
            .strip_prefix("SF NS ")
            .map_or_else(|| hidden.to_string(), |rest| format!("SF {rest}")),
        None => family,
    };
    Some((
        family,
        crate::ligature_shaping::font_has_ligature_features(&bytes),
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
/// whole family across weights), else the legacy family. A hidden system
/// family (`.SF NS Mono`) is not one to offer.
fn face_family(face: &aterm_font::Face<'_>) -> Option<String> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    face_name(face, TYPOGRAPHIC_FAMILY)
        .or_else(|| face_name(face, FAMILY))
        .filter(|family| !family.starts_with('.'))
}

/// The face's full name (`JetBrains Mono Bold`) where the resolvers find it
/// again (it reads as the file's name or its PostScript name once spaces and
/// hyphens go), else its PostScript name.
fn face_full_name(face: &aterm_font::Face<'_>, stem: &str) -> Option<String> {
    use aterm_font::name_id::{FULL_NAME, POST_SCRIPT_NAME};
    let postscript = face_name(face, POST_SCRIPT_NAME)?;
    let full = face_name(face, FULL_NAME).filter(|full| {
        let full = normalize_family(full);
        full == normalize_family(&postscript) || full == normalize_family(stem)
    });
    Some(full.unwrap_or(postscript))
}

/// One `name` record, English where the face has it.
fn face_name(face: &aterm_font::Face<'_>, id: u16) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk is recursive and keeps no seen-set, so a search root that lies
    /// inside another root is walked twice and every font under it is listed
    /// (and, by every full-read consumer, read) twice. `FONT_DIRS` carried
    /// `/System/Library/Fonts/Supplemental` beside its parent for exactly that
    /// cost — 290 faces / 131 MB twice per walk on a Mac. Structural, so it is
    /// red on that list on every host, not only where the directory exists.
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

    /// The same property observed on THIS host's real font tree: the catalogue
    /// names each path once. (Raw paths, deliberately not canonical ones — a
    /// user whose `~/.fonts` is a symlink to `~/.local/share/fonts` legitimately
    /// sees the same files under two prefixes, and that is a property of their
    /// home directory, not of the walk.)
    #[test]
    fn system_font_files_lists_each_path_once() {
        let files = system_font_files();
        let mut seen = std::collections::HashSet::new();
        let duplicates: Vec<&PathBuf> = files.iter().filter(|p| !seen.insert(*p)).collect();
        assert!(
            duplicates.is_empty(),
            "{} of {} catalogued font paths are listed more than once, e.g. {}",
            duplicates.len(),
            files.len(),
            duplicates[0].display()
        );
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

    /// Two files name the same family and the bold one sorts first: the
    /// family resolves to its regular face.
    #[test]
    fn a_family_resolves_to_its_regular_face_not_the_first_file() {
        let root = fixture("regular-face");
        std::fs::write(root.join("JetBrainsMono-Bold.ttf"), with_weight(700)).unwrap();
        std::fs::write(root.join("JetBrainsMono-Regular.ttf"), JETBRAINS).unwrap();
        let batch = resolve_and_admit_in_dirs(
            &["JetBrains Mono".to_string()],
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
        let request = ["JetBrains Mono".to_string()];
        let winner = |batch: &Batch| batch.get(0).unwrap().as_ref().unwrap().path.clone();
        let batch = resolve_and_admit_in_dirs(&request, &roots, Limits::default());
        assert_eq!(
            winner(&batch),
            regular.to_string_lossy().into_owned(),
            "descending into another directory must not finalize the first bold face"
        );

        // With no regular face in the winning user directory, that user's
        // bold face still precedes both the nested and system regular faces.
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
        // Force a distinct directory observation before the first real walk:
        // atomic rename moves it from this old epoch to now, even on a
        // coarse filesystem. This is not a wait for a timestamp tick.
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

    /// The memo: a second ask over an unchanged tree is answered without a
    /// walk; a font installed anywhere in the tree — here in a NESTED vendor
    /// directory, the Linux layout, so a root-mtime key would miss it — is a
    /// miss, and the answer includes it; a root that did not exist when the
    /// memo was taken invalidates it by appearing. Private fixture roots, so
    /// no concurrent caller of the real `system_font_files()` shares this key.
    #[test]
    fn the_font_files_memo_is_revalidated_by_the_visited_directories_mtimes() {
        let root = fixture("memo");
        let vendor = root.join("truetype").join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("a.ttf"), b"a").unwrap();
        // A SIBLING of `root`, not a child: the walk is recursive and the
        // search dirs are never nested (`search_dirs_are_never_nested_...`).
        let absent_root = fixture("memo-later");
        std::fs::remove_dir_all(&absent_root).unwrap();
        let roots = vec![root.clone(), absent_root.clone()];

        let (first, walked) = memoized_font_files_walked(&roots);
        assert_eq!(first, vec![vendor.join("a.ttf")]);
        assert!(walked, "a cold ask walks");

        let (again, walked) = memoized_font_files_walked(&roots);
        assert_eq!(again, first);
        assert!(!walked, "an unchanged tree is answered from the memo");

        // A font lands in the NESTED directory: only `vendor/`'s mtime moves.
        // (A same-instant write can share the directory's mtime tick on a
        // coarse filesystem; nudge the clock past it before writing.)
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(vendor.join("b.ttf"), b"b").unwrap();
        let (grown, walked) = memoized_font_files_walked(&roots);
        assert_eq!(grown, vec![vendor.join("a.ttf"), vendor.join("b.ttf")]);
        assert!(walked, "an entry added deep in the tree is a miss");

        // The second root comes into existence with a font in it.
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

    /// SINGLE-FLIGHT: the broad and symbol chain builders ask for the same
    /// tree on two threads at the same instant at every launch. Before the
    /// walk gate both missed the memo and both walked (measured with `strace`:
    /// every font directory opened twice). Now exactly one concurrent caller
    /// walks and the rest are answered from its memo, with the same list.
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

    /// A bundled family NAME yields to an INSTALLED face of that name (so a
    /// variable `JetBrainsMono[wght].ttf` keeps its weight axis), and falls back
    /// to the compiled-in face only when nothing installed answers; the
    /// `bundled:` IDENTITY always means the compiled-in face.
    #[test]
    fn an_installed_face_wins_a_bundled_family_name_and_the_identity_never_yields() {
        if !crate::bundled::ACTIVE {
            return;
        }
        let root = fixture("bundled-name");
        let installed = root.join("JetBrainsMono-Regular.ttf");
        std::fs::write(&installed, b"installed jetbrains mono").unwrap();
        let requests = vec![
            "JetBrains Mono".to_string(),
            crate::bundled::JETBRAINS_ID.to_string(),
        ];
        let batch =
            resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&root), Limits::default());
        let by_name = batch.get(0).unwrap().as_ref().unwrap();
        assert_eq!(by_name.path, installed.to_string_lossy(), "installed wins");
        assert_eq!(&by_name.bytes[..], b"installed jetbrains mono");
        let by_id = batch.get(1).unwrap().as_ref().unwrap();
        assert_eq!(by_id.path, crate::bundled::JETBRAINS_ID);

        // Nothing installed: the NAME resolves to the compiled-in face, with its
        // bytes, under its identity (so its Bold/Italic siblings are found).
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

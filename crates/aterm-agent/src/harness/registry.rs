// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS REGISTRY (`docs/HARNESS-CONTRACT.md` §2): the harnesses aterm proves a claim from,
//! the words each one leads, the environment prefixes they add to every shell's strip list, and
//! their marks, composed from data.
//!
//! atpkg composes the rows ([`atpkg::harness_record::compose`]): a verified record of an
//! installed build, a compiled seed row for a harness whose index-pinned build carries no record
//! yet, or a development link under its rule, with one leading harness per word. This module
//! turns each into a claim [`Row`]: the agent whose screen reader reads its tab is the name
//! table's agent for its first led word, else for its id or tool ([`aterm_phase::program_of`]),
//! else none ([`Program::Generic`], [`agent_of`]; atpkg refuses a row leading a word the name
//! table reads as no agent, [`atpkg::harness_record::name_table_refusal`]), the scope words it
//! allows are what the built-in engine stands down from for its agent (`allows`: every duty
//! where HX-07 let the engine stand down from typing, else `relaunch` and `upgrade`), which
//! bound a seed id's contract 3 claim and a seed row's grant, and its origin decides a contract 4
//! claim's grant ([`super::contract::grant`]). A claimant's image is judged at each claim's proof
//! under the trust as it stands then ([`Registry::admits_image`]), whatever the registry cached;
//! a standing claim's re-proof keeps that verdict while the claimant, its executable file and the
//! registry's generation ([`current_at`]) stay as they were (aterm-gui's
//! `harness_claim::KeptImage`).

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use aterm_phase::Program;
use aterm_types::domain::{EnvStrip, EnvStripRefusal};
use aterm_types::mark::MarkSpec;

pub use atpkg::harness_record::{Composition, Image, Origin, Refusal, is_seed};

use atpkg::harness_record::Trust;

use super::contract::{Row, Scope, Source};

/// The scope words a seed id's contract 3 claim of a registry row running `program` may name
/// (`docs/HARNESS-CONTRACT.md` §2); contract 4 grants a seed row these and `state`, `accounts`
/// ([`super::contract::grant`]). A row allows only the duties aterm's built-in engine can stand
/// down from for its agent, so this is the engine's fact, not the harness's, and goes with the
/// engine (HX-28). Claude Code's lanes stand down from every duty a proven claim takes: the
/// relaunch and upgrade lanes (`SupervisorAuthority::builtin_keeps`,
/// `upgrade_drive::self_claimed_by_status`) and, since HX-07 measured that a tab's own process
/// types under `if-human=` without losing or reordering a person's key
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §5), the typing duties (`turn-end`,
/// `approvals`, `questions`, `walls`): the engine's own typing names its duty and is refused one a
/// proven claim takes (`SupervisorInputFence::authorize`). Every other agent's row allows
/// `relaunch` and `upgrade` until its harness takes the typing duties (HX-23 to HX-27).
const fn allows(program: Program) -> &'static [Scope] {
    match program {
        Program::Claude => &Scope::ALL,
        _ => &[Scope::Relaunch, Scope::Upgrade],
    }
}

/// The agent `declaration`'s harness runs, by the name table ([`aterm_phase::program_of`]): its
/// first led word's, else its id's, else its tool's. A row that leads nothing and whose id and
/// tool the table does not read runs no agent aterm knows ([`Program::Generic`]): its claim is
/// proven and granted as any row's is, the engine has no vendor to judge in its tab, and no
/// program's row ([`super::contract::row_for`]) or mark ([`Registry::mark_for`]) is it.
fn agent_of(declaration: &atpkg::harness_record::Declaration) -> Program {
    declaration
        .leads
        .first()
        .and_then(|w| aterm_phase::program_of(w))
        .or_else(|| aterm_phase::program_of(&declaration.id))
        .or_else(|| aterm_phase::program_of(&declaration.tool))
        .unwrap_or(Program::Generic)
}

/// Where `origin` puts a claim row, for its contract 4 grant.
const fn source_of(origin: &Origin) -> Source {
    match origin {
        Origin::Seed => Source::Seed,
        Origin::Signed { .. } => Source::Signed,
        Origin::Linked { .. } => Source::Linked,
    }
}

/// One registry row with the facts beside its claim row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The claim row.
    pub row: &'static Row,
    /// The binary the package installs.
    pub tool: String,
    /// The agent words it leads.
    pub leads: Vec<String>,
    /// The product name.
    pub name: String,
    /// Where it came from.
    pub origin: Origin,
    /// Its mark, if its record carries one.
    pub mark: Option<MarkSpec>,
    /// The environment prefixes its record adds.
    pub env_strip: Vec<String>,
    /// Where its claimant may run from.
    pub images: Vec<Image>,
}

/// The registry: its entries, ordered by id, what it refused and narrowed, the strip list it
/// makes, and the trust its records were verified under.
#[derive(Clone, Debug)]
pub struct Registry {
    entries: Vec<Entry>,
    rows: &'static [Row],
    refused: Vec<Refusal>,
    notes: Vec<Refusal>,
    env_strip: EnvStrip,
    env_refused: Vec<EnvStripRefusal>,
    trust: Trust,
}

impl Registry {
    /// The registry atpkg's `composition` makes under `trust`, with the person's
    /// `shell_env_strip` (`None`: unset). Each row's agent is [`agent_of`] its declaration.
    #[must_use]
    pub fn from_composition(
        composition: Composition,
        trust: Trust,
        person_env_strip: Option<&[String]>,
    ) -> Self {
        let refused = composition.refused;
        let mut entries = Vec::new();
        for row in composition.rows {
            let d = row.declaration;
            let program = agent_of(&d);
            let claim = Row {
                id: intern(&d.id),
                program,
                package: intern(&d.id),
                vendor: program.name(),
                allows: allows(program),
                source: source_of(&row.origin),
            };
            entries.push(Entry {
                row: intern_row(claim),
                tool: d.tool,
                leads: d.leads,
                name: d.name,
                origin: row.origin,
                mark: d.mark,
                env_strip: d.env_strip,
                images: row.images,
            });
        }
        let rows = intern_rows(entries.iter().map(|e| *e.row).collect());
        let (env_strip, env_refused) = EnvStrip::compose(
            person_env_strip,
            entries
                .iter()
                .flat_map(|e| e.env_strip.iter().map(|s| (e.row.id, s.as_str()))),
        );
        Self {
            entries,
            rows,
            refused,
            notes: composition.notes,
            env_strip,
            env_refused,
            trust,
        }
    }

    /// The registry of the configured store and aterm.toml, as they are now.
    #[must_use]
    pub fn load() -> Self {
        let (layout, config) = configured();
        Self::load_from(layout.as_ref(), &config)
    }

    /// The registry of `layout` (`None`: no store, the seeds alone) under `config`.
    #[must_use]
    pub fn load_from(
        layout: Option<&atpkg::store::Layout>,
        config: &atpkg::config::HarnessConfig,
    ) -> Self {
        let (composition, trust) = match layout {
            Some(layout) => {
                let trust = Trust::of_layout(layout);
                (
                    atpkg::harness_record::compose(layout, config, &trust),
                    trust,
                )
            }
            None => (seeds_only(), no_trust()),
        };
        let mut registry = Self::from_composition(composition, trust, config.env_strip.as_deref());
        if let Some(entry) = &config.env_strip_unread {
            registry.env_refused.insert(
                0,
                EnvStripRefusal {
                    origin: "aterm.toml".to_string(),
                    entry: entry.clone(),
                    reason: "shell_env_strip is a list of strings, so the default list applies",
                },
            );
        }
        registry
    }

    /// The seed rows alone, read from no store and no aterm.toml.
    #[must_use]
    pub fn seeds() -> Self {
        Self::from_composition(seeds_only(), no_trust(), None)
    }

    /// The claim rows.
    #[must_use]
    pub const fn rows(&self) -> &'static [Row] {
        self.rows
    }

    /// The entries, ordered by id.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry whose id is `id`.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.row.id == id)
    }

    /// The entry that leads `word`.
    #[must_use]
    pub fn leader_of(&self, word: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.leads.iter().any(|w| w == word))
    }

    /// The records refused, and why.
    #[must_use]
    pub fn refused(&self) -> &[Refusal] {
        &self.refused
    }

    /// The rows kept with less than they asked for, and why.
    #[must_use]
    pub fn notes(&self) -> &[Refusal] {
        &self.notes
    }

    /// The strip list: the person's or the default, plus each row's additions.
    #[must_use]
    pub const fn env_strip(&self) -> &EnvStrip {
        &self.env_strip
    }

    /// The strip-list entries refused, and why.
    #[must_use]
    pub fn env_refused(&self) -> &[EnvStripRefusal] {
        &self.env_refused
    }

    /// The mark a record gives `program`'s tabs: the first row for that agent whose record
    /// carries one. A generic tab takes none, so a row that runs no agent aterm knows
    /// ([`agent_of`]) marks no tab.
    #[must_use]
    pub fn mark_for(&self, program: Program) -> Option<&MarkSpec> {
        if program == Program::Generic {
            return None;
        }
        self.entries
            .iter()
            .filter(|e| e.row.program == program)
            .find_map(|e| e.mark.as_ref())
    }

    /// Whether the executable `exe` may run as `row`'s harness in `layout`, judged now: inside
    /// a store build of the row's package whose kept record verifies again at this proof (a
    /// signed row's, or a seed row's build that carries a record), any ready build of a seed
    /// row's package that carries none, or a development link's checkout under its rule. The
    /// record is verified under this registry's trust with what `layout`'s index cache says now
    /// ([`Trust::refreshed`]), so a roster that revokes its signer or an index that yanks its
    /// build refuses it here, as does a tree changed since. A row this registry does not hold
    /// admits nothing.
    #[must_use]
    pub fn admits_image(
        &self,
        layout: &atpkg::store::Layout,
        row: &Row,
        exe: &std::path::Path,
    ) -> bool {
        self.admits_image_under(layout, row, exe, &self.trust.refreshed(layout))
    }

    /// [`Self::admits_image`] under an explicit `trust`.
    #[must_use]
    pub fn admits_image_under(
        &self,
        layout: &atpkg::store::Layout,
        row: &Row,
        exe: &std::path::Path,
        trust: &Trust,
    ) -> bool {
        let Some(entry) = self.entry(row.id) else {
            return false;
        };
        let composed = atpkg::harness_record::Row {
            declaration: atpkg::harness_record::Declaration {
                id: entry.row.id.to_string(),
                tool: entry.tool.clone(),
                leads: entry.leads.clone(),
                contracts: Vec::new(),
                name: entry.name.clone(),
                mark: None,
                env_strip: Vec::new(),
            },
            origin: entry.origin.clone(),
            images: entry.images.clone(),
        };
        atpkg::harness_record::admits_image(layout, &composed, exe, trust)
    }
}

/// A trust that verifies nothing: no masters, no deny-list.
fn no_trust() -> Trust {
    Trust::of(atpkg::Anchor::of(Vec::new(), 0), Vec::new())
}

/// The seed rows alone: a process with no configured store.
fn seeds_only() -> Composition {
    let layout = atpkg::store::Layout {
        prefix: std::path::PathBuf::new(),
    };
    atpkg::harness_record::compose_from(
        &layout,
        &atpkg::harness_record::Holdings::default(),
        &atpkg::config::HarnessConfig::default(),
        &no_trust(),
    )
}

/// `text` as a `&'static str`, one allocation per distinct string for the process's life: a
/// claim row's fields are `'static`, and reading the registry again allocates nothing new.
fn intern(text: &str) -> &'static str {
    static STRINGS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut strings = STRINGS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = strings.iter().find(|s| **s == text) {
        return kept;
    }
    let kept: &'static str = Box::leak(text.to_string().into_boxed_str());
    strings.push(kept);
    kept
}

/// `row` as a `&'static Row`, interned as [`intern`] does.
fn intern_row(row: Row) -> &'static Row {
    static ROWS: Mutex<Vec<&'static Row>> = Mutex::new(Vec::new());
    let mut rows = ROWS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = rows.iter().find(|r| ***r == row) {
        return kept;
    }
    let kept: &'static Row = Box::leak(Box::new(row));
    rows.push(kept);
    kept
}

/// `rows` as a `&'static [Row]`, interned as [`intern`] does.
fn intern_rows(rows: Vec<Row>) -> &'static [Row] {
    static SLICES: Mutex<Vec<&'static [Row]>> = Mutex::new(Vec::new());
    let mut slices = SLICES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = slices.iter().find(|s| **s == rows.as_slice()) {
        return kept;
    }
    let kept: &'static [Row] = Box::leak(rows.into_boxed_slice());
    slices.push(kept);
    kept
}

/// A file's identity as the fingerprint keeps it: its name, and its metadata as the kernel
/// keeps it (length, inode, and change time, which no user call can set back).
type Stamp = (String, FileId);

/// What [`file_id`] reads of a file: length, device, inode, modification and change time.
type FileId = (u64, u64, u64, i128, i128);

/// `path`'s identity, not following a final link; zeros when it is not there.
fn file_id(path: &std::path::Path) -> FileId {
    let Ok(m) = std::fs::symlink_metadata(path) else {
        return (0, 0, 0, 0, 0);
    };
    let modified = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let changed = i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec());
        (m.len(), m.dev(), m.ino(), modified, changed)
    }
    #[cfg(not(unix))]
    {
        (m.len(), 0, 0, modified, 0)
    }
}

/// What the registry was last read from: the store's harness holdings and each one's kept
/// records and tool files, each link's checkout declaration, the index cache (its rosters'
/// revocations and its indexes' yanks), the `current` link of each harness program, and the
/// person's settings. Equal state reads the same registry. A claim's image is judged again at
/// its proof whatever this says ([`Registry::admits_image`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    holdings: Vec<(String, u64, Vec<Stamp>)>,
    linked: Vec<(String, std::path::PathBuf, FileId)>,
    cache: FileId,
    currents: Vec<(String, Option<std::path::PathBuf>)>,
    config: atpkg::config::HarnessConfig,
}

impl Fingerprint {
    /// What `layout` (`None`: no store) and `config` stand at now.
    fn of(layout: Option<&atpkg::store::Layout>, config: atpkg::config::HarnessConfig) -> Self {
        let Some(layout) = layout else {
            return Self {
                holdings: Vec::new(),
                linked: Vec::new(),
                cache: (0, 0, 0, 0, 0),
                currents: Vec::new(),
                config,
            };
        };
        let held = atpkg::harness_record::Holdings::of_layout(layout);
        let holdings = held
            .recorded
            .iter()
            .map(|(program, build)| {
                let dir = layout.build_dir(program, *build);
                let mut files: Vec<Stamp> = std::fs::read_dir(dir.join("bin"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            file_id(&e.path()),
                        )
                    })
                    .collect();
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                for suffix in [
                    atpkg::harness_record::PKG_SUFFIX,
                    atpkg::harness_record::PKG_SIG_SUFFIX,
                    atpkg::harness_record::ROSTER_SUFFIX,
                    atpkg::harness_record::ROSTER_SIG_SUFFIX,
                    atpkg::harness_record::RECORD_SUFFIX,
                    ".ready",
                ] {
                    let id = file_id(&dir.with_file_name(format!("{name}{suffix}")));
                    files.push((suffix.to_string(), id));
                }
                files.push(("bin/".to_string(), file_id(&dir.join("bin"))));
                files.push(("./".to_string(), file_id(&dir)));
                files.sort();
                (program.clone(), *build, files)
            })
            .collect();
        let linked = held
            .linked
            .into_iter()
            .map(|(program, checkout)| {
                let declared = file_id(&checkout.join(atpkg::harness_record::DECLARATION_FILE));
                (program, checkout, declared)
            })
            .collect();
        let mut currents: Vec<(String, Option<std::path::PathBuf>)> =
            std::fs::read_dir(layout.prefix.join("store"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|program| atpkg::harness::is_harness(program))
                .map(|program| {
                    let current = std::fs::read_link(layout.program_current(&program)).ok();
                    (program, current)
                })
                .collect();
        currents.sort();
        Self {
            holdings,
            linked,
            cache: file_id(&layout.prefix.join("index-cache.toml")),
            currents,
            config,
        }
    }
}

/// What a spawn stats to learn whether what [`Fingerprint`] reads may have moved since the
/// registry was read ([`install_shell_strip_list`]): aterm.toml; under the store, `store/`, each
/// harness program's folder in it (a build, its record or its `current` link coming or going
/// changes it), `bin/` (which build is live), `links/` and the index cache; and each link's
/// checkout declaration, at the identity the fingerprint read. A file changed in place under a
/// build folder is not glanced at: it can only take a row, and the strip entries its record
/// added, away, so until the next read the list strips more, never less, and a claim's proof
/// judges that build again ([`Registry::admits_image`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Glance(Vec<(std::path::PathBuf, FileId)>);

impl Glance {
    /// What `layout` (`None`: no store) shows now, before its fingerprint is read, so a change
    /// made while that read runs moves the next glance.
    fn of(layout: Option<&atpkg::store::Layout>) -> Self {
        let mut paths: Vec<std::path::PathBuf> = atpkg::config::config_path().into_iter().collect();
        if let Some(layout) = layout {
            let store = layout.prefix.join("store");
            let mut programs: Vec<std::path::PathBuf> = std::fs::read_dir(&store)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(atpkg::harness::is_harness)
                })
                .map(|e| e.path())
                .collect();
            programs.sort();
            paths.push(store);
            paths.extend(programs);
            paths.push(layout.bin_dir());
            paths.push(layout.links_dir());
            paths.push(layout.prefix.join("index-cache.toml"));
        }
        Self(
            paths
                .into_iter()
                .map(|path| {
                    let id = file_id(&path);
                    (path, id)
                })
                .collect(),
        )
    }

    /// This glance with each link's declaration at the identity `seen` read it.
    fn with_declarations(mut self, seen: &Fingerprint) -> Self {
        self.0.extend(seen.linked.iter().map(|(_, checkout, id)| {
            (checkout.join(atpkg::harness_record::DECLARATION_FILE), *id)
        }));
        self
    }

    /// Whether every file it names is as it was.
    fn holds(&self) -> bool {
        self.0.iter().all(|(path, id)| file_id(path) == *id)
    }
}

/// The store and aterm.toml this process reads its registry from. A test binary reads none, a
/// unit test of this crate or of any crate that reads the registry through it
/// (`aterm_types::dirs::test_root`): its registry, and the strip list it installs, are the
/// seeds', whatever the machine running it holds; a test that wants a store names it
/// ([`for_layout`], [`Registry::load_from`]).
fn configured() -> (Option<atpkg::store::Layout>, atpkg::config::HarnessConfig) {
    if reads_no_store() {
        return (None, atpkg::config::HarnessConfig::default());
    }
    (
        atpkg::store::resolve_configured(),
        atpkg::config::load_harness(),
    )
}

/// One read of the registry: what it was read from, what a spawn glances at to learn whether
/// that moved, the registry, and the generation it was installed as ([`read_into`]).
struct Read {
    seen: Fingerprint,
    glance: Glance,
    registry: Arc<Registry>,
    generation: u64,
}

/// Whether this process is a test binary, which reads no store and no aterm.toml ([`configured`]).
fn reads_no_store() -> bool {
    cfg!(test) || aterm_types::dirs::test_root().is_some()
}

/// The registry this process read last, with the generation it was installed as: a reader takes
/// the two together ([`current_at`], [`snapshot_at`]).
static CURRENT: RwLock<Option<Read>> = RwLock::new(None);

/// The registry as the store and aterm.toml stand now ([`current_at`]), for a reader that records
/// no generation.
#[must_use]
pub fn current() -> Arc<Registry> {
    current_at().0
}

/// THE REGISTRY AS THE STORE AND ATERM.TOML STAND NOW, WITH ITS GENERATION: read again when what it
/// is read from changed since the last read ([`Fingerprint`]), else the last one. The generation
/// counts this process's reads made anew, so a reader that saw one generation knows a later one
/// may hold other rows (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2, HX-06). The two come
/// back as one value, taken under the lock that installs a read with its generation
/// ([`read_into`]), so a pass that records the generation it proved against (the host's re-proof
/// of live claims, a claimant's kept image verdict) proved against that very registry. Read by two
/// calls, another thread's read landing between them would be recorded as proven while its rows
/// were never judged, and every later pass would skip it (HX-05's review, findings 5 and 11). Each
/// new read installs its strip list for this process's spawns
/// ([`aterm_types::domain::install_env_strip`]).
#[must_use]
pub fn current_at() -> (Arc<Registry>, u64) {
    let (layout, config) = configured();
    read_into(&CURRENT, layout.as_ref(), config, &|registry: &Registry| {
        aterm_types::domain::install_env_strip(registry.env_strip().clone());
    })
}

/// Read the registry of `layout` (`None`: no store) under `config` into `slot`: the read `slot`
/// holds while what it was read from has not moved ([`Fingerprint`]), else a new read, handed to
/// `installed` and then installed under `slot`'s lock with the generation after the one it
/// replaces (the first is 1). The registry and its generation come back from one look under that
/// lock.
fn read_into(
    slot: &RwLock<Option<Read>>,
    layout: Option<&atpkg::store::Layout>,
    config: atpkg::config::HarnessConfig,
    installed: &dyn Fn(&Registry),
) -> (Arc<Registry>, u64) {
    let glance = Glance::of(layout);
    let now = Fingerprint::of(layout, config);
    let glance = glance.with_declarations(&now);
    if let Some(read) = slot
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        && read.seen == now
    {
        read.glance = glance;
        return (Arc::clone(&read.registry), read.generation);
    }
    let registry = Arc::new(Registry::load_from(layout, &now.config));
    installed(&registry);
    let mut held = slot.write().unwrap_or_else(PoisonError::into_inner);
    let generation = held
        .as_ref()
        .map_or(0, |read| read.generation)
        .saturating_add(1);
    *held = Some(Read {
        seen: now,
        glance,
        registry: Arc::clone(&registry),
        generation,
    });
    (registry, generation)
}

/// Whether the strip list this process installed is still the one the store and aterm.toml
/// make, as a glance at what it is read from tells ([`Glance`]): a few file stats, no read.
/// `false` before the first read, and once anything glanced at moved, until the next read.
#[must_use]
pub fn shell_strip_list_holds() -> bool {
    CURRENT
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|read| read.glance.holds())
}

/// Install the strip list of the registry as it stands now for this process's spawns: what a
/// process that starts shells calls before it starts one. A glance tells whether anything it is
/// read from moved ([`shell_strip_list_holds`]); only then is the registry read ([`current`]).
pub fn install_shell_strip_list() {
    if !shell_strip_list_holds() {
        let _ = current();
    }
}

/// The registry of `layout`: [`current`] for this user's configured store, else `layout`'s own,
/// composed now under aterm.toml's `[harness]` (a store a caller names, as a test's).
#[must_use]
pub fn for_layout(layout: &atpkg::store::Layout) -> Arc<Registry> {
    if configured().0.as_ref() == Some(layout) {
        return current();
    }
    Arc::new(Registry::load_from(
        Some(layout),
        &atpkg::config::load_harness(),
    ))
}

/// The registry last read ([`current`]), with no file read: what a frame draws from. `None`
/// before the first read.
#[must_use]
pub fn snapshot() -> Option<Arc<Registry>> {
    snapshot_at().map(|(registry, _)| registry)
}

/// The registry last read and the generation it was installed as ([`current_at`]), with no file
/// read: one look under the lock that installed them, what a standing claim's re-proof reads.
/// `None` before the first read.
#[must_use]
pub fn snapshot_at() -> Option<(Arc<Registry>, u64)> {
    CURRENT
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|read| (Arc::clone(&read.registry), read.generation))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

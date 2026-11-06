// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness disk free <path>…`: the one way aterm's disk-free worker removes anything
//! (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.4). A path goes only when every fence holds: it
//! lies strictly under a cache root tools rebuild or download again ([`ROOTS`]), it is no link and
//! no component of it is one, it is on its root's volume with no mount point at or under it, no
//! process of the user's uses it, and aterm's own grant is on (`[disk] auto_free_gib > 0`). Each
//! removal is journalled in the disk ledger before it starts, and a removal the ledger cannot
//! record does not start.
//! Managed markers, package coordinators and complete registry custody take precedence over
//! this safelist. A bounded FD preflight and the guarded remover never follow nested links;
//! partial removal reports the walker's counted allocation, not a before/after size estimate.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::{Config, ROW_TEXT_CAP, discover, human_bytes, path_text, rfc3339_utc, truncate_bytes};

/// The ledger rows' `kind`.
pub const KIND: &str = "disk-free";

/// A child of a cache root that stays, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// This name.
    Name(&'static str),
    /// Every name that begins with this.
    Prefix(&'static str),
}

impl Kept {
    /// Whether `name` is this one, ASCII case folded: macOS's default volume names a folder alike
    /// in any case, so `Com.Apple.Safari` is `com.apple.Safari`.
    #[must_use]
    pub fn holds(self, name: &str) -> bool {
        match self {
            Kept::Name(n) => name.eq_ignore_ascii_case(n),
            Kept::Prefix(p) => name
                .get(..p.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(p)),
        }
    }
}

/// One cache root: where it lies under the home folder, and which of its children stay, each with
/// why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// The root, relative to the home folder, `/`-separated.
    pub rel: &'static str,
    /// The children nothing here removes, and why each stays.
    pub kept: &'static [(Kept, &'static str)],
}

impl Root {
    /// The root under `home`.
    #[must_use]
    pub fn under(&self, home: &Path) -> PathBuf {
        self.rel
            .split('/')
            .fold(home.to_path_buf(), |p, c| p.join(c))
    }

    /// Why the child of this root named `name` stays, or `None` when it may go.
    #[must_use]
    pub fn keeps(&self, name: &str) -> Option<&'static str> {
        self.kept
            .iter()
            .find(|(k, _)| k.holds(name))
            .map(|(_, why)| *why)
    }
}

const ATERM: (Kept, &str) = (Kept::Name("aterm"), "aterm's own");

/// The cache roots tools rebuild or download again, relative to the home folder (§8.4). The
/// macOS-only ones are simply absent elsewhere.
pub const ROOTS: &[Root] = &[
    Root {
        rel: "Library/Caches",
        kept: &[
            (Kept::Prefix("com.apple."), "macOS's own (`com.apple.*`)"),
            ATERM,
            (Kept::Prefix("com.aterm."), "aterm's own"),
            (Kept::Prefix("."), "a hidden entry"),
        ],
    },
    Root {
        rel: ".cache",
        kept: &[ATERM],
    },
    Root {
        rel: ".cargo/registry",
        kept: &[],
    },
    Root {
        rel: ".cargo/git",
        kept: &[],
    },
    Root {
        rel: ".npm",
        kept: &[],
    },
    Root {
        rel: "Library/Developer/Xcode/DerivedData",
        kept: &[],
    },
];

/// Why every path is refused while aterm's own grant is off.
pub const GRANT_OFF: &str = "aterm's own grant is off (`[disk] auto_free_gib = 0` in aterm.toml, \
     or a `[disk]` value it could not read): nothing is removed; `--dry-run` says what would go";

/// Why a path is refused when its intent row could not be written.
pub const UNJOURNALLED: &str =
    "the disk ledger could not be written, and nothing is removed that it does not record first";

const UNRECORDED_COMPLETION: &str =
    "the removal's completion could not be written to the disk ledger";

/// What one run's fences read, injected so a test drives them on scratch roots.
pub struct World<'a> {
    /// The owner's home folder, the base of every root.
    pub home: &'a Path,
    /// The running processes' uses of a path ([`discover::Processes`]; the product's is
    /// [`discover::scan_processes`]).
    pub processes: discover::Processes<'a>,
    /// The mount table, `None` where it could not be read ([`super::mounts_clear`]).
    pub mounts: Option<&'a aterm_sysprobe::volumes::Table>,
    /// The `[disk]` knobs as read: the grant is `auto_free_gib > 0`.
    pub config: Config,
    /// Unix seconds, for the ledger rows.
    pub now: i64,
    /// The aterm session it runs under, or empty.
    pub sid: &'a str,
}

impl World<'_> {
    /// Whether aterm's own grant is on, as for the window's pass.
    #[must_use]
    pub const fn grant(&self) -> bool {
        self.config.auto_free_gib > 0
    }
}

/// A path every fence admitted ([`judge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    /// The path, its own real path.
    pub path: PathBuf,
    /// The root it lies under, as resolved.
    pub root: PathBuf,
    /// The file allocation counted before removal ([`super::target::cache_bytes`]).
    pub bytes: u64,
    /// Whether the count stopped short, so `bytes` is a lower bound.
    pub partial: bool,
    /// Its identity when judged; a removal goes only while it is unchanged.
    pub id: aterm_sysprobe::volumes::FileId,
}

/// What became of one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Removed, accounting for `bytes` of file allocation (at least, when `partial`).
    Removed {
        /// The file allocation counted by the remover.
        bytes: u64,
        /// Whether the count stopped short.
        partial: bool,
    },
    /// Removed completely, but its completion row could not be written to the disk ledger.
    Unrecorded {
        /// The file allocation counted by the remover.
        bytes: u64,
    },
    /// A dry run's eligible file allocation; the removal grant is reported separately.
    Would {
        /// The bytes counted.
        bytes: u64,
        /// Whether the count stopped short.
        partial: bool,
    },
    /// Not removed, and why.
    Refused(String),
    /// Its removal started and failed, having counted `freed` bytes of removed file allocation.
    Failed {
        /// The removed file allocation counted before it failed.
        freed: u64,
        /// What failed.
        why: String,
    },
}

/// One path given, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The path as given.
    pub path: PathBuf,
    /// What became of it.
    pub verdict: Verdict,
}

impl Outcome {
    /// Whether the operation was refused, failed, or could not record its completion. Any such
    /// outcome makes the command's exit status nonzero, including a fully removed path.
    #[must_use]
    pub const fn refused(&self) -> bool {
        matches!(
            self.verdict,
            Verdict::Refused(_) | Verdict::Failed { .. } | Verdict::Unrecorded { .. }
        )
    }

    /// Counted file allocation removed, or estimated for a dry run. Shared storage means this
    /// is not a measurement of the change in available volume space.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        match &self.verdict {
            Verdict::Removed { bytes, .. }
            | Verdict::Would { bytes, .. }
            | Verdict::Unrecorded { bytes } => *bytes,
            Verdict::Failed { freed, .. } => *freed,
            Verdict::Refused(_) => 0,
        }
    }

    /// The line a person reads.
    #[must_use]
    pub fn line(&self) -> String {
        let at_least = |partial: bool| if partial { "at least " } else { "" };
        let path = self.path.display();
        match &self.verdict {
            Verdict::Removed { bytes, partial } => {
                format!(
                    "removed {path} ({}{} of file allocation)",
                    at_least(*partial),
                    human_bytes(*bytes)
                )
            }
            Verdict::Would { bytes, partial } => format!(
                "would remove {path} ({}{} of file allocation)",
                at_least(*partial),
                human_bytes(*bytes)
            ),
            Verdict::Unrecorded { bytes } => format!(
                "removed {path} ({} of file allocation), but {UNRECORDED_COMPLETION}",
                human_bytes(*bytes)
            ),
            Verdict::Refused(why) => format!("refused {path}: {why}"),
            Verdict::Failed { freed, why } => format!(
                "failed {path} after removing {} of file allocation: {why}",
                human_bytes(*freed)
            ),
        }
    }

    /// The JSON object `--json` prints for it: `path`, `removed`, `bytes`, `bytes_partial`,
    /// `refused` (the operation's failure reason, or `null`), `dry_run` and `grant`.
    /// An unrecorded completion keeps `removed=true` and its bytes, alongside its failure reason.
    #[must_use]
    pub fn to_json(&self, dry_run: bool, grant: bool) -> Value {
        let (removed, partial, refused) = match &self.verdict {
            Verdict::Removed { partial, .. } => (true, *partial, None),
            Verdict::Unrecorded { .. } => (true, false, Some(UNRECORDED_COMPLETION.to_owned())),
            Verdict::Would { partial, .. } => (false, *partial, None),
            Verdict::Refused(why) => (false, false, Some(why.clone())),
            Verdict::Failed { freed, why } => (
                false,
                false,
                Some(format!(
                    "the removal failed after removing {} of file allocation: {why}",
                    human_bytes(*freed)
                )),
            ),
        };
        let mut o = Map::new();
        o.insert(
            "path".to_owned(),
            Value::from(self.path.display().to_string()),
        );
        o.insert("removed".to_owned(), Value::from(removed));
        o.insert("bytes".to_owned(), Value::from(self.bytes()));
        o.insert("bytes_partial".to_owned(), Value::from(partial));
        o.insert(
            "refused".to_owned(),
            refused.map_or(Value::Null, Value::from),
        );
        o.insert("dry_run".to_owned(), Value::from(dry_run));
        o.insert("grant".to_owned(), Value::from(grant));
        Value::Object(o)
    }
}

/// One command's counted allocation and path outcomes, independent of its output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Paths removed and journalled completely, or listed as eligible by a dry run.
    pub successful: usize,
    /// Paths refused, whose removal failed, or whose completion could not be journalled.
    pub refused: usize,
    /// Counted file allocation, including the portion removed before a failure.
    pub bytes: u64,
    /// The count is a lower bound because an estimate was incomplete or its sum overflowed.
    pub bytes_partial: bool,
}

impl Summary {
    /// Count every outcome without wrapping a large allocation total or losing a lower bound.
    #[must_use]
    pub fn from_outcomes(outcomes: &[Outcome]) -> Self {
        let mut summary = Self::default();
        for outcome in outcomes {
            if outcome.refused() {
                summary.refused += 1;
            } else {
                summary.successful += 1;
            }
            let (bytes, overflow) = summary.bytes.overflowing_add(outcome.bytes());
            summary.bytes = if overflow { u64::MAX } else { bytes };
            summary.bytes_partial |= overflow
                || matches!(
                    outcome.verdict,
                    Verdict::Removed { partial: true, .. } | Verdict::Would { partial: true, .. }
                );
        }
        summary
    }

    /// The human summary: allocation accounting, separately from completed path counts.
    #[must_use]
    pub fn line(&self, dry_run: bool) -> String {
        let (action, state) = if dry_run {
            ("would remove", "matched")
        } else {
            ("removed", "completed")
        };
        let lower_bound = if self.bytes_partial { "at least " } else { "" };
        format!(
            "{action} {lower_bound}{} of file allocation; {} path(s) {state}; {} refused or failed",
            human_bytes(self.bytes),
            self.successful,
            self.refused
        )
    }
}

/// The file allocation session `sid`'s `removed` and `failed` ledger rows counted. Unlike a rise
/// in available volume space, this accounting belongs to that session; rows cut by the ledger's
/// bound are not included.
#[must_use]
pub fn removed_by(rows: &[String], sid: &str) -> u64 {
    rows.iter()
        .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
        .filter(|row| {
            let word = |key: &str| row.get(key).and_then(Value::as_str);
            word("kind") == Some(KIND)
                && word("sid") == Some(sid)
                && matches!(word("action"), Some("removed" | "failed"))
        })
        .filter_map(|row| row.get("bytes").and_then(Value::as_u64))
        .fold(0, u64::saturating_add)
}

/// One `disk-free` row of the disk ledger: `action` is `removing` (the intent, before the
/// removal starts), `removed`, `failed` or `refused`.
#[must_use]
pub fn row(
    now: i64,
    sid: &str,
    path: &Path,
    action: &str,
    bytes: Option<u64>,
    text: &str,
) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from(KIND.to_owned()));
    o.insert("action".to_owned(), Value::from(action.to_owned()));
    o.insert("path".to_owned(), Value::from(path_text(path)));
    o.insert("bytes".to_owned(), bytes.map_or(Value::Null, Value::from));
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(text, ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"disk-free\",\"reason\":\"unserializable\"}".to_owned())
}

/// The roots, as `~/<rel>`, for a refusal to name.
fn roots_named() -> String {
    ROOTS
        .iter()
        .map(|r| format!("~/{}", r.rel))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `p` resolved, every link followed, spelled plainly (no Windows verbatim prefix).
fn real_path(p: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(|r| aterm_sysprobe::paths::plain(&r).into_owned())
}

/// Whether `a` and `b` name one path (on Windows, by its comparison key).
fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let key = aterm_sysprobe::paths::windows_key;
        key(a).is_some_and(|k| Some(k) == key(b))
    } else {
        a == b
    }
}

/// Whether `p` lies under `dir` and is not `dir` itself.
fn strictly_under(p: &Path, dir: &Path) -> bool {
    aterm_sysprobe::paths::is_under(p, dir) == Some(true) && !same(p, dir)
}

/// Prove absence of filesystem custody without following links. An incomplete bounded read
/// is uncertainty, never permission to remove an ancestor of a managed cache.
fn custody_clear(path: &Path) -> Result<(), String> {
    super::target::unmanaged_custody(path, super::target::WalkBudget::SHIPPED)
}

/// The bytes removing the file `meta` describes releases: its blocks, unless another link keeps
/// them.
fn file_bytes(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() == 1 {
            meta.blocks().saturating_mul(512)
        } else {
            0
        }
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// The bytes removing `path` releases, and whether the count stopped short.
fn released(path: &Path, dev: u64) -> (u64, bool) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => super::target::cache_bytes(path, Some(dev)),
        Ok(meta) => (file_bytes(&meta), false),
        Err(_) => (0, false),
    }
}

/// The fences of §8.4 but the grant, read for `path`: the path admitted with what its removal
/// releases, or why it stays. Nothing under the path is read before the root's own name fence has
/// passed.
///
/// # Errors
///
/// Why it stays: it is not absolute; the home folder cannot be resolved; it lies under no root, or
/// is a root, or lies under a root that cannot be resolved, resolves outside the home folder, or is
/// reached through a link; its first component under the root is one the root keeps; it is not
/// there or cannot be read; it is a link, or its real path is not the path given; it is on another
/// volume than its root, or its volume cannot be read; the mount table was not read whole, or a
/// mount point lies at or under it; or a process uses it or cannot be ruled out.
pub fn judge(path: &Path, world: &World<'_>) -> Result<Admitted, String> {
    if !path.is_absolute() {
        return Err("it is not an absolute path: give its whole path".to_owned());
    }
    let home = real_path(world.home).map_err(|e| {
        format!(
            "the home folder {} cannot be resolved ({e}), so no cache root is placed",
            world.home.display()
        )
    })?;
    let (root, root_real) = root_of(path, world.home, &home)?;
    let rest = path
        .components()
        .nth(root_real.components().count())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(why) = root.keeps(&rest) {
        return Err(format!(
            "{rest} under {} is {why}: it stays",
            root_real.display()
        ));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "it is not there".to_owned()
        } else {
            format!("it cannot be read: {e}")
        }
    })?;
    if super::reserve::is_link(&meta) {
        return Err("it is a symbolic link: a link is never followed or removed".to_owned());
    }
    let real = real_path(path).map_err(|e| format!("it cannot be resolved: {e}"))?;
    if !same(&real, path) {
        return Err(format!(
            "its real path is {}: a component of the path given is a link, and nothing is \
             removed through one",
            real.display()
        ));
    }
    let id = aterm_sysprobe::volumes::file_id(path, false)
        .ok_or_else(|| "its volume cannot be read".to_owned())?;
    let root_dev = aterm_sysprobe::volumes::file_id(&root_real, true)
        .ok_or_else(|| format!("the volume of {} cannot be read", root_real.display()))?
        .dev;
    if id.dev != root_dev {
        return Err(format!(
            "it is on another volume than its cache root {}",
            root_real.display()
        ));
    }
    super::mounts_clear(path, world.mounts)?;
    discover::unused_under(&[path.to_path_buf()], world.processes)?;
    custody_clear(path)?;
    atpkg::build_cache::registered::ordinary_clearance(&home, path)?;
    let (bytes, partial) = released(path, id.dev);
    Ok(Admitted {
        path: path.to_path_buf(),
        root: root_real,
        bytes,
        partial,
        id,
    })
}

/// The root `path` lies strictly under, with that root resolved: one whose resolved path lies
/// under the resolved home folder `home` (`given` as the caller named it).
fn root_of(path: &Path, given: &Path, home: &Path) -> Result<(&'static Root, PathBuf), String> {
    for root in ROOTS {
        let named = root.under(given);
        let real = match real_path(&named) {
            Ok(real) => real,
            Err(e) => {
                if aterm_sysprobe::paths::is_under(path, &named) == Some(true) {
                    return Err(format!(
                        "the cache root {} cannot be resolved ({e})",
                        named.display()
                    ));
                }
                continue;
            }
        };
        if same(path, &real) || same(path, &named) {
            return Err(format!(
                "it is the cache root {} itself: only what lies under a root is removed",
                real.display()
            ));
        }
        let real_holds = strictly_under(path, &real);
        if !real_holds && !strictly_under(path, &named) {
            continue;
        }
        if !strictly_under(&real, home) {
            return Err(format!(
                "the cache root {} resolves to {}, outside the home folder {}",
                named.display(),
                real.display(),
                home.display()
            ));
        }
        if real_holds {
            return Ok((root, real));
        }
        return Err(format!(
            "the cache root {} is reached through a link (it is {}): nothing is removed through \
             one",
            named.display(),
            real.display()
        ));
    }
    Err(format!(
        "it lies under none of the cache roots tools rebuild or download again ({})",
        roots_named()
    ))
}

/// The most entries [`survey`] lists.
pub const SURVEY_MAX: usize = 24;

/// What `aterm harness disk free --dry-run` with no path lists: every child of every cache root
/// under `world.home` that the fences admit, judged as a dry run, largest first, at most
/// [`SURVEY_MAX`]; a child a root keeps by its name is never read. So the worker finds what the
/// roots hold without listing or sizing a folder under `~/Library` itself, which the secrets rule
/// refuses, and only what it may remove is named.
#[must_use]
pub fn survey(world: &World<'_>) -> Vec<Outcome> {
    let mut children: Vec<PathBuf> = ROOTS
        .iter()
        .flat_map(|root| {
            let dir = root.under(world.home);
            std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|name| root.keeps(name).is_none())
                })
                .map(|e| e.path())
                .collect::<Vec<_>>()
        })
        .collect();
    children.sort();
    let mut admitted: Vec<Outcome> = run(&children, true, world, &mut |_| true)
        .into_iter()
        .filter(|o| matches!(o.verdict, Verdict::Would { .. }))
        .collect();
    admitted.sort_by_key(|o| std::cmp::Reverse(o.bytes()));
    admitted.truncate(SURVEY_MAX);
    admitted
}

/// Free each of `paths` (§8.4): every fence read ([`judge`]), aterm's grant on, a `removing` row
/// written through `journal` (which says whether it wrote) before the removal starts, its identity
/// unchanged since it was judged, and a `removed` or `failed` row after it. A refused path gets a
/// `refused` row, written as it can be. A failed completion write makes the command fail while
/// preserving what the remover actually removed.
/// Under `dry_run` nothing is removed or journalled, whatever the grant.
pub fn run(
    paths: &[PathBuf],
    dry_run: bool,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
) -> Vec<Outcome> {
    paths
        .iter()
        .map(|path| {
            let verdict = one(path, dry_run, world, journal);
            if let (Verdict::Refused(why), false) = (&verdict, dry_run) {
                journal(&row(world.now, world.sid, path, "refused", None, why));
            }
            Outcome {
                path: path.clone(),
                verdict,
            }
        })
        .collect()
}

fn one(
    path: &Path,
    dry_run: bool,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
) -> Verdict {
    if !dry_run && !world.grant() {
        return Verdict::Refused(GRANT_OFF.to_owned());
    }
    let a = match judge(path, world) {
        Ok(a) => a,
        Err(why) => return Verdict::Refused(why),
    };
    if dry_run {
        return Verdict::Would {
            bytes: a.bytes,
            partial: a.partial,
        };
    }
    let why = format!(
        "a cache under {} that tools rebuild or download again: no link, on the root's volume, \
         no mount point and no process under it",
        a.root.display()
    );
    if !journal(&row(
        world.now,
        world.sid,
        &a.path,
        "removing",
        Some(a.bytes),
        &why,
    )) {
        return Verdict::Refused(UNJOURNALLED.to_owned());
    }
    if aterm_sysprobe::volumes::file_id(&a.path, false) != Some(a.id) {
        return Verdict::Refused(
            "it changed while its fences were read, so it is judged again by a later run"
                .to_owned(),
        );
    }
    if let Err(why) = custody_clear(&a.path) {
        return Verdict::Refused(why);
    }
    let clearance = match atpkg::build_cache::registered::ordinary_clearance(world.home, &a.path) {
        Ok(clearance) => clearance,
        Err(why) => return Verdict::Refused(why),
    };
    if let Err(why) = discover::unused_under(std::slice::from_ref(&a.path), world.processes) {
        return Verdict::Refused(why);
    }
    let mut permit = || {
        if clearance.current() {
            Ok(())
        } else {
            Err("registered-cache index changed during removal".to_owned())
        }
    };
    record_removal(
        &a,
        super::target::remove_unmanaged_tree(&a.path, a.id, &mut permit),
        world,
        journal,
        &why,
    )
}

fn record_removal(
    a: &Admitted,
    result: std::io::Result<super::Removed>,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
    why: &str,
) -> Verdict {
    match result {
        Ok(done)
            if done.units.iter().any(|path| path == &a.path)
                && done.trouble.is_empty()
                && done.skipped.is_empty() =>
        {
            if !journal(&row(
                world.now,
                world.sid,
                &a.path,
                "removed",
                Some(done.bytes),
                why,
            )) {
                return Verdict::Unrecorded { bytes: done.bytes };
            }
            Verdict::Removed {
                bytes: done.bytes,
                partial: false,
            }
        }
        Ok(done) => {
            let freed = done.bytes;
            let why = done
                .trouble
                .into_iter()
                .chain(done.skipped.into_iter().map(|(_, why)| why))
                .collect::<Vec<_>>()
                .join("; ");
            let mut why = if why.is_empty() {
                "removal retained part of the path".to_owned()
            } else {
                why
            };
            if !journal(&row(
                world.now,
                world.sid,
                &a.path,
                "failed",
                Some(freed),
                &why,
            )) {
                why.push_str("; ");
                why.push_str(UNRECORDED_COMPLETION);
            }
            Verdict::Failed { freed, why }
        }
        Err(error) => Verdict::Refused(error.to_string()),
    }
}

#[cfg(test)]
#[path = "disk_free_tests.rs"]
mod tests;

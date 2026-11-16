// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness disk free <path>…`: a named cache's removal, for a person or an agent who
//! names it. A path goes only when every fence holds: it lies strictly under a cache root tools
//! rebuild or download again ([`ROOTS`]), it is no link and no component of it is one, it is on
//! its root's volume with no mount point at or under it, no process of the user's uses it, and
//! aterm's own grant is on (`[disk] auto_free_gib > 0`). Each removal is journalled in the disk
//! ledger before it starts, and a removal the ledger cannot record does not start.
//! Managed markers, package coordinators and complete registry custody take precedence over
//! this safelist. A bounded FD preflight and the guarded remover never follow nested links;
//! partial removal reports the walker's counted allocation, not a before/after size estimate.
//!
//! The disk watch's own pass removes through the same fences ([`pass`]): once its tiers have run
//! out and the home volume is still below its floor, it takes the children of `~/.cache` and
//! `~/Library/Caches` ([`PASS_ROOTS`]) that are PROVEN CACHES ([`evidence`]: a build or package
//! tool's own cache by its name, [`PRODUCERS`], or, under `~/.cache`, a folder whose tool marked
//! it with a `CACHEDIR.TAG`), that no name of [`PASS_KEPT`] keeps, and that nothing wrote,
//! changed or read within `[disk] idle_build_days`, least recently used first, free space
//! measured before each. A folder with no such evidence stays whatever its age: a cache root also
//! holds logins, history and data no tool makes again. Each removal renames the folder aside
//! first ([`STAGE`]), so a removal cut short never leaves half a cache where its tool looks, and
//! the next pass finishes what a cut removal left ([`finish_staged`]). It chooses by what it
//! reads, so no session is asked for a path; every removal passes the fences of [`judge`] and is
//! journalled as the verb's are, and nothing is asked of anyone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};
use aterm_sysprobe::volumes::{FileId, Table};

use super::target::CacheUse;
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

const ATERM: (Kept, &str) = (Kept::Prefix("aterm"), "aterm's own");

/// What a pass removing a child renames it to first, before the child's inode number and name
/// (`.aterm-removing-<ino>-<name>`): hidden, so no tool takes it for its cache, and named by its
/// inode, so [`finish_staged`] finishes only what this pass set aside.
pub const STAGE: &str = ".aterm-removing-";

const STAGED: (Kept, &str) = (
    Kept::Prefix(STAGE),
    "aterm's own removal in progress, which the disk pass finishes",
);

/// The cache roots tools rebuild or download again, relative to the home folder. The
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
        kept: &[ATERM, STAGED],
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
    pub id: FileId,
    /// Unix seconds of the newest write or change under it, itself included, read in the walk
    /// that counted `bytes` ([`CacheUse::newest`]); `None` where no time was read.
    pub last_use: Option<i64>,
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
    std::fs::canonicalize(p).map(|r| aterm_path::plain(&r).into_owned())
}

/// Whether `a` and `b` name one path (on Windows, by its comparison key).
fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let key = aterm_path::windows_key;
        key(a).is_some_and(|k| Some(k) == key(b))
    } else {
        a == b
    }
}

/// Whether `p` lies under `dir` and is not `dir` itself.
fn strictly_under(p: &Path, dir: &Path) -> bool {
    aterm_path::is_under(p, dir) == Some(true) && !same(p, dir)
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

/// The bytes removing `path` releases, whether the count stopped short, and the newest write or
/// change under it, in one walk that stops, cut short, once `until` passes.
fn released(path: &Path, dev: u64, until: Option<Instant>) -> CacheUse {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => super::target::cache_use(path, Some(dev), until),
        Ok(meta) => CacheUse {
            bytes: file_bytes(&meta),
            partial: false,
            #[cfg(not(windows))]
            newest: super::target::written(&meta),
            #[cfg(windows)]
            newest: None,
        },
        Err(_) => CacheUse {
            bytes: 0,
            partial: false,
            newest: None,
        },
    }
}

/// A path its place admits ([`placed`]).
struct Placed {
    /// The root it lies under, as resolved.
    root: PathBuf,
    /// Its identity as read.
    id: FileId,
    /// The home folder, resolved.
    home: PathBuf,
}

/// The fences of [`judge`] that read nothing under `path`: it is absolute, strictly under a
/// cache root of `home` by a name the root does not keep, there, no link and reached through
/// none, on its root's volume, with `mounts` read whole and no mount point at or under it.
///
/// # Errors
///
/// Why it stays, in [`judge`]'s words for each of those fences.
fn placed(path: &Path, home: &Path, mounts: Option<&Table>) -> Result<Placed, String> {
    if !path.is_absolute() {
        return Err("it is not an absolute path: give its whole path".to_owned());
    }
    let given = home;
    let home = real_path(given).map_err(|e| {
        format!(
            "the home folder {} cannot be resolved ({e}), so no cache root is placed",
            given.display()
        )
    })?;
    let (root, root_real) = root_of(path, given, &home)?;
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
    super::mounts_clear(path, mounts)?;
    Ok(Placed {
        root: root_real,
        id,
        home,
    })
}

/// Every fence of [`run`] but the grant, read for `path`: the path admitted with what its removal
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
    let at = placed(path, world.home, world.mounts)?;
    discover::unused_under(&[path.to_path_buf()], world.processes)?;
    unmanaged(path, at)
}

/// The fences of [`judge`] that come after its place (`at`) and its processes: no managed
/// custody under `path` and none the registered caches claim, with what its removal releases.
///
/// # Errors
///
/// Why it stays, in [`judge`]'s words for each of those fences.
fn unmanaged(path: &Path, at: Placed) -> Result<Admitted, String> {
    custody_clear(path)?;
    atpkg::build_cache::registered::ordinary_clearance(&at.home, path)?;
    let used = released(path, at.id.dev, None);
    Ok(Admitted {
        path: path.to_path_buf(),
        root: at.root,
        bytes: used.bytes,
        partial: used.partial,
        id: at.id,
        last_use: used.newest,
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
                if aterm_path::is_under(path, &named) == Some(true) {
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

/// The children of `root`, which lies at `dir`, that the root does not keep by name. The
/// listing reads `dir` alone: no child is opened or sized.
fn children(root: &Root, dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|name| root.keeps(name).is_none())
        })
        .map(|e| e.path())
        .collect()
}

/// What `aterm harness disk free --dry-run` with no path lists: every child of every cache root
/// under `world.home` that the fences admit, judged as a dry run, largest first, at most
/// [`SURVEY_MAX`]; a child a root keeps by its name is never read. So an agent finds what the
/// roots hold without listing or sizing a folder under `~/Library` itself, which the secrets rule
/// refuses, and only what it may remove is named.
#[must_use]
pub fn survey(world: &World<'_>) -> Vec<Outcome> {
    let mut children: Vec<PathBuf> = ROOTS
        .iter()
        .flat_map(|root| children(root, &root.under(world.home)))
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

/// Free each of `paths`: every fence read ([`judge`]), aterm's grant on, a `removing` row
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
    remove(&a, world, journal, Removal::InPlace)
}

/// How [`remove`] takes the path it admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// Where it stands: the verb's, whose person or agent named the path.
    InPlace,
    /// Renamed aside in its root first ([`STAGE`], never over another entry), then removed: the
    /// pass's, so its tool never finds part of its cache, and what a cut removal leaves the next
    /// pass finishes ([`finish_staged`]).
    Staged,
}

/// The name `a`'s path is set aside under in its root: [`STAGE`], its inode number, `-`, its
/// name.
fn staged_name(path: &Path, id: FileId) -> Option<std::ffi::OsString> {
    let mut name = std::ffi::OsString::from(format!("{STAGE}{}-", id.ino));
    name.push(path.file_name()?);
    Some(name)
}

/// Rename `from` to `to` where nothing stands at `to`, replacing no entry.
#[cfg(unix)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    aterm_dirfd::renameat_with(
        aterm_dirfd::CWD,
        from,
        aterm_dirfd::CWD,
        to,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

/// Off Unix no pass runs ([`pass`]), and nothing is set aside.
#[cfg(not(unix))]
fn rename_noreplace(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Remove the path `a` admitted: its `removing` row first (none written, nothing removed), then
/// its identity, custody, clearance and process use read again, the removal (`how`: in place, or
/// set aside first), and its `removed` or `failed` row.
fn remove(
    a: &Admitted,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
    how: Removal,
) -> Verdict {
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
    let at = match how {
        Removal::InPlace => a.path.clone(),
        Removal::Staged => {
            let Some(parent) = a.path.parent() else {
                return Verdict::Refused("it has no parent folder to be set aside in".to_owned());
            };
            let Some(name) = staged_name(&a.path, a.id) else {
                return Verdict::Refused("it has no name to be set aside under".to_owned());
            };
            let to = parent.join(name);
            if let Err(e) = rename_noreplace(&a.path, &to) {
                return Verdict::Refused(format!(
                    "it could not be set aside as {} ({e}), so nothing of it was removed",
                    to.display()
                ));
            }
            to
        }
    };
    let result = super::target::remove_unmanaged_tree(&at, a.id, &mut permit);
    record_removal(a, &at, result, world, journal, &why)
}

/// FINISH WHAT A CUT REMOVAL LEFT: each entry of a [`PASS_ROOTS`] root under `world.home` named
/// [`STAGE`]`<ino>-…` whose inode is that number (so only what a pass set aside: a folder made
/// under that name otherwise stays), no link, on the root's volume (the root on the home's), with
/// the mount table read whole and no mount at or under it, no process using it and no custody in
/// it, is removed, its `removing` row first (none written, nothing removed) and its `removed` or
/// `failed` row after. What a fence refuses stays, with one `refused` row. Nothing is done with
/// the grant off or on Windows.
pub fn finish_staged(world: &World<'_>, journal: &mut dyn FnMut(&str) -> bool) -> Vec<Outcome> {
    let mut out = Vec::new();
    if cfg!(windows) || !world.grant() {
        return out;
    }
    let Ok(home) = real_path(world.home) else {
        return out;
    };
    let Some(home_dev) = aterm_sysprobe::volumes::file_id(&home, true).map(|id| id.dev) else {
        return out;
    };
    for (rel, _) in PASS_ROOTS {
        let Some(root) = ROOTS.iter().find(|root| root.rel == *rel) else {
            continue;
        };
        let dir = root.under(&home);
        if !real_path(&dir).is_ok_and(|real| same(&real, &dir))
            || aterm_sysprobe::volumes::file_id(&dir, false).map(|id| id.dev) != Some(home_dev)
        {
            continue;
        }
        let mut staged: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(STAGE))
            })
            .collect();
        staged.sort();
        for path in staged {
            let verdict = finish_one(&path, &home, home_dev, world, journal);
            if let Verdict::Refused(why) = &verdict {
                journal(&row(world.now, world.sid, &path, "refused", None, why));
            }
            out.push(Outcome { path, verdict });
        }
    }
    out
}

/// [`finish_staged`]'s fences and removal of one set-aside entry at `path`.
fn finish_one(
    path: &Path,
    home: &Path,
    home_dev: u64,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
) -> Verdict {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Verdict::Refused("it cannot be read".to_owned());
    };
    if super::reserve::is_link(&meta) {
        return Verdict::Refused(
            "it is a symbolic link: a link is never followed or removed".to_owned(),
        );
    }
    let Some(id) = aterm_sysprobe::volumes::file_id(path, false) else {
        return Verdict::Refused("its volume cannot be read".to_owned());
    };
    let named = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(STAGE))
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(ino, _)| ino.parse::<u128>().ok());
    if named != Some(id.ino) || id.dev != home_dev {
        return Verdict::Refused(
            "its name does not carry its own inode on the home's volume: no pass set it aside"
                .to_owned(),
        );
    }
    if let Err(why) = super::mounts_clear(path, world.mounts) {
        return Verdict::Refused(why);
    }
    if let Err(why) = discover::unused_under(&[path.to_path_buf()], world.processes) {
        return Verdict::Refused(why);
    }
    if let Err(why) = custody_clear(path) {
        return Verdict::Refused(why);
    }
    let clearance = match atpkg::build_cache::registered::ordinary_clearance(home, path) {
        Ok(clearance) => clearance,
        Err(why) => return Verdict::Refused(why),
    };
    let used = released(path, id.dev, None);
    let a = Admitted {
        path: path.to_path_buf(),
        root: path.parent().unwrap_or(home).to_path_buf(),
        bytes: used.bytes,
        partial: used.partial,
        id,
        last_use: used.newest,
    };
    let why = "what an earlier pass set aside and could not finish removing";
    if !journal(&row(
        world.now,
        world.sid,
        path,
        "removing",
        Some(a.bytes),
        why,
    )) {
        return Verdict::Refused(UNJOURNALLED.to_owned());
    }
    let mut permit = || {
        if clearance.current() {
            Ok(())
        } else {
            Err("registered-cache index changed during removal".to_owned())
        }
    };
    let result = super::target::remove_unmanaged_tree(path, id, &mut permit);
    record_removal(&a, path, result, world, journal, why)
}

/// The rows and verdict of `a`'s removal, made at `at` (its path, or where it was set aside).
fn record_removal(
    a: &Admitted,
    at: &Path,
    result: std::io::Result<super::Removed>,
    world: &World<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
    why: &str,
) -> Verdict {
    match result {
        Ok(done)
            if done.units.iter().any(|path| path == at)
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

/// The cache roots the disk watch's own pass reaches ([`pass`]), each a `rel` of [`ROOTS`], in
/// the order it reads them, and whether the first read of one of its children is guarded
/// ([`date_within`]): on macOS a read of another app's folder under `~/Library` can raise a
/// privacy dialog, and the reading thread waits until a person answers it. `.cache` comes
/// first: its children are read in place, so no guarded wait spends the pass's wall clock
/// before them. The other roots stay the verb's alone.
pub const PASS_ROOTS: &[(&str, bool)] = &[(".cache", false), ("Library/Caches", true)];

/// The children of a pass root the pass never takes, whatever marks them, each with why. The
/// verb (`aterm harness disk free <path>`) still may: a person or an agent names the path.
pub const PASS_KEPT: &[(Kept, &str)] = &[
    (Kept::Prefix("."), "a hidden entry"),
    (Kept::Prefix("aterm"), "aterm's own"),
    (Kept::Prefix("alab"), "ALab's own"),
    (Kept::Name("codex-alab"), "ALab's own"),
    (Kept::Name("build-slot"), "build admission's state"),
    (
        Kept::Name("trust-cg"),
        "Trust's proof store, which Trust evicts itself",
    ),
    (
        Kept::Name("huggingface"),
        "holds a login and models a download may not get again",
    ),
    (Kept::Name("rclone"), "holds writes not yet uploaded"),
    (Kept::Name("JetBrains"), "holds the IDEs' local history"),
    (Kept::Name("lm-studio"), "holds conversations"),
    (Kept::Name("pypoetry"), "holds virtual environments"),
];

/// The build and package tools' own caches the pass may take by name, each a pass root's `rel`
/// and a child's name: each tool makes again on its next run whatever of it is gone, with
/// nothing a person wrote in it.
pub const PRODUCERS: &[(&str, &str)] = &[
    (".cache", "pip"),
    (".cache", "uv"),
    (".cache", "go-build"),
    (".cache", "pre-commit"),
    (".cache", "node-gyp"),
    (".cache", "ccache"),
    (".cache", "sccache"),
    (".cache", "yarn"),
    ("Library/Caches", "pip"),
    ("Library/Caches", "go-build"),
    ("Library/Caches", "Homebrew"),
    ("Library/Caches", "Yarn"),
    ("Library/Caches", "node-gyp"),
    ("Library/Caches", "ccache"),
    ("Library/Caches", "Mozilla.sccache"),
];

/// The Cache Directory Tagging Specification's tag file: a tool that writes it says the folder
/// holds only what it makes again. Its content begins with a 43-byte signature.
pub const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_SIGNATURE_LEN: u64 = 43;

/// Why the child `path` of the pass root `rel` is a proven cache the pass may take, or `None`:
/// no name of [`PASS_KEPT`], and either a [`PRODUCERS`] name or, under `.cache` (whose children
/// are read in place, never through a guard), a folder, no link, with a `CACHEDIR.TAG` at its
/// top ([`tagged`]). A name alone is read for a guarded root's child, so its listing touches
/// nothing under it.
#[must_use]
pub fn evidence(rel: &str, path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if PASS_KEPT.iter().any(|(kept, _)| kept.holds(name)) {
        return None;
    }
    if PRODUCERS
        .iter()
        .any(|(under, producer)| *under == rel && Kept::Name(producer).holds(name))
    {
        return Some("a build or package tool's own cache, which it makes again");
    }
    (rel == ".cache" && tagged(path))
        .then_some("a folder its tool tagged a cache (`CACHEDIR.TAG`), which it makes again")
}

/// Whether `dir` is a folder, no link, holding a `CACHEDIR.TAG` that is a regular file, no
/// link, at least as long as the tag's signature. The tag is read by its metadata alone: a read
/// of its content would set its read time, and a read time is a use ([`super::target::CacheUse`]),
/// so the check itself would keep every tagged folder in use.
fn tagged(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir())
        && std::fs::symlink_metadata(dir.join(CACHEDIR_TAG))
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() >= CACHEDIR_SIGNATURE_LEN)
}

/// How long a look waits for the first read of a guarded child ([`date_within`]).
pub const TOUCH_WAIT: Duration = Duration::from_secs(2);

/// The wall clock one pass spends dating the outside caches, its first read of each:
/// [`super::HOLDERS_BUDGET`]'s figure. The judgments and removals that follow are bounded by
/// the pass's target and by [`PASS_KEPT_MAX`], not by this clock.
pub const PASS_WALL: Duration = Duration::from_secs(20);

/// How long, in seconds, a child the pass read and kept for another reason than its date is
/// not read again.
const RETRY_S: i64 = 3600;

/// The most children one pass judges and then keeps: each judgment is a custody scan and a
/// whole walk. A child the pass's one process scan finds in use is kept without a judgment
/// and is not counted.
const PASS_KEPT_MAX: usize = 8;

/// The least a child holds for the pass to take it. Less cannot move the free figure, and the
/// smallest children of a cache root are lock and slot files, which a removal would race.
const PASS_MIN_BYTES: u64 = 1 << 20;

/// The `refused` row of a guarded child whose first read did not return in time
/// ([`Dated::TimedOut`]). A record: it asks nothing.
pub const HELD_READ: &str = "its first read did not return within the look's wait: it is left \
     for now, and no folder beside it is read while that read is pending";

const CUT_UNMEASURED: &str =
    "free space could not be measured again, so the outside caches' step stopped there";
const CUT_WALL: &str =
    "the outside caches were not all read within the step's wall clock; the next look goes on";
const CUT_COUNTED: &str = "the outside caches removed cover the shortfall first measured, and \
     free space does not show it yet; the next look measures again";
const CUT_KEPT: &str = "the outside caches' step judged as many folders as it may and kept \
     them; the next look goes on";
const CUT_MOUNTS: &str = "the mount table was not read whole, so no outside cache was read; \
     the next look reads the table again";
const CUT_HOME: &str = "the home folder could not be resolved, so no outside cache was read";
const CUT_HELD: &str = "a first read under a guarded cache root did not return within the \
     look's wait, so the folders after it were not read; the next look goes on";
const CUT_PENDING: &str = "a first read under a guarded cache root has not returned yet, so \
     that root's folders were not read; the next look goes on";

/// What a read of one child is given ([`read`]).
#[derive(Debug, Clone)]
pub struct Reading {
    /// The home folder, the base of every root.
    pub home: PathBuf,
    /// The mount table, `None` where it could not be read ([`super::mounts_clear`]).
    pub mounts: Option<Table>,
    /// When the read stops, cut short.
    pub until: Instant,
    /// How long the look waits for a guarded child's first read ([`Reads::wait`]).
    pub wait: Duration,
}

/// What dating one guarded child came to ([`Dater`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dated {
    /// Its read returned: what it holds and its last use, or why it stays ([`read`]).
    At(Result<CacheUse, String>),
    /// Its read did not return within the wait and is left running; its answer is kept for a
    /// later pass. `said`: this process journalled a `refused` row of this child before, or
    /// there is nothing to say (the read never started).
    TimedOut {
        /// Whether no row is owed.
        said: bool,
    },
    /// An earlier read of it has not returned: none was made.
    StillHeld,
}

/// How a pass dates a guarded child ([`date_within`] is the product's).
pub type Dater<'a> = &'a dyn Fn(&Path, &Reading) -> Dated;

/// How a pass reads the outside caches, injected so a test drives it.
#[derive(Clone, Copy)]
pub struct Reads<'a> {
    /// Dates a guarded child ([`date_within`]).
    pub dater: Dater<'a>,
    /// The wall clock the pass spends dating ([`PASS_WALL`]).
    pub wall: Duration,
    /// How long the look waits for a guarded child's first read ([`TOUCH_WAIT`]).
    pub wait: Duration,
}

/// What one [`pass`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Passed {
    /// Each child whose removal was started, with what became of it.
    pub outcomes: Vec<Outcome>,
    /// The file allocation the removals counted ([`Outcome::bytes`]).
    pub bytes: u64,
    /// The guarded children whose first read did not return, each journalled `refused` once.
    pub held: Vec<PathBuf>,
    /// The guarded children whose first read a path fence refused, each with why, journalled
    /// `refused` once.
    pub refused: Vec<(PathBuf, String)>,
    /// Why the pass stopped before it had read and judged every child it may take (`None`: it
    /// reached them all, or free space is back at the target).
    pub cut: Option<String>,
}

/// The guarded children whose first read has not returned ([`date_off_thread`]), each with
/// when that read's own walk stops.
static HELD: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());
/// The guarded children a `refused` row was journalled of, once a process.
static SAID: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
/// Per child, the unix second before which no pass reads it again.
static LATER: Mutex<BTreeMap<PathBuf, i64>> = Mutex::new(BTreeMap::new());
/// The answers of the reads a look stopped waiting for, each kept for the next pass to take
/// in place of a second read.
static LATE: Mutex<BTreeMap<PathBuf, Result<CacheUse, String>>> = Mutex::new(BTreeMap::new());

/// `memo`, for one map operation: never held across a filesystem call or a journal write.
fn locked<T>(memo: &Mutex<T>) -> MutexGuard<'_, T> {
    memo.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A read on a thread of its own: dropped, its path leaves [`HELD`].
struct InFlight(PathBuf);

impl Drop for InFlight {
    fn drop(&mut self) {
        locked(&HELD).remove(&self.0);
    }
}

/// Where a read on a thread of its own leaves its answer ([`date_off_thread`]).
enum Slot {
    /// The look still waits for it.
    Awaited,
    /// The read returned while the look waited: its answer, for the look to take.
    Answered(Result<CacheUse, String>),
    /// The look stopped waiting: the answer goes to [`LATE`].
    Left,
}

/// What the child at `path` holds and when it was last used, read behind the fences that read
/// nothing under it ([`placed`]: its root's name fence, no link, its root's volume, no mount
/// point), the walk stopping at `r.until`.
///
/// # Errors
///
/// Why it stays, in [`judge`]'s words for each of those fences.
pub fn read(path: &Path, r: &Reading) -> Result<CacheUse, String> {
    let at = placed(path, &r.home, r.mounts.as_ref())?;
    Ok(released(path, at.id.dev, Some(r.until)))
}

/// `look` at `path` on a thread of its own, waited for `wait` and no longer: a read a privacy
/// dialog holds keeps that thread and nothing else. While it has not returned `path` is in
/// [`HELD`], with `until` (when the read's own walk stops), and no second read of it starts;
/// the thread takes it out once its answer is placed. An answer the look did not wait for is
/// kept in [`LATE`], so the walk a slow folder took is used by the next pass, not made again.
fn date_off_thread(
    path: &Path,
    wait: Duration,
    until: Instant,
    look: impl FnOnce(&Path) -> Result<CacheUse, String> + Send + 'static,
) -> Dated {
    {
        let mut held = locked(&HELD);
        if held.contains_key(path) {
            return Dated::StillHeld;
        }
        held.insert(path.to_path_buf(), until);
    }
    let flight = InFlight(path.to_path_buf());
    let slot = Arc::new(Mutex::new(Slot::Awaited));
    let theirs = Arc::clone(&slot);
    let (wake, woken) = std::sync::mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("aterm-harness-disk-cache-read".to_owned())
        .spawn(move || {
            let read = look(&flight.0);
            let left = {
                let mut slot = locked(&theirs);
                if matches!(*slot, Slot::Left) {
                    Some(read)
                } else {
                    *slot = Slot::Answered(read);
                    None
                }
            };
            if let Some(read) = left {
                locked(&LATE).insert(flight.0.clone(), read);
            }
            drop(flight);
            let _ = wake.send(());
        });
    if spawned.is_err() {
        // The closure went with the failed spawn, and its `InFlight` freed the path.
        return Dated::TimedOut { said: true };
    }
    let woke = woken.recv_timeout(wait);
    // The slot decides, under its lock: an answer placed before the look leaves is taken
    // here, and one that comes after goes to `LATE`.
    let answer = std::mem::replace(&mut *locked(&slot), Slot::Left);
    match (answer, woke) {
        (Slot::Answered(read), _) => Dated::At(read),
        // The read panicked: it is not pending, so nothing true could be said of a wait.
        (_, Err(RecvTimeoutError::Disconnected)) => Dated::TimedOut { said: true },
        _ => Dated::TimedOut {
            said: !locked(&SAID).insert(path.to_path_buf()),
        },
    }
}

/// The product's [`Dater`]: [`read`] on a thread of its own, waited for `r.wait`. The thread
/// makes the look's first touch of the child (its stat, its path fences and its whole walk),
/// so a privacy dialog nobody answers holds that thread alone, once.
#[must_use]
pub fn date_within(path: &Path, r: &Reading) -> Dated {
    let given = r.clone();
    date_off_thread(path, r.wait, r.until, move |path| read(path, &given))
}

/// One child the pass may read.
struct Candidate {
    path: PathBuf,
    /// Its first read goes through the [`Dater`].
    guarded: bool,
    /// Its pass root's `rel`, for its [`evidence`] read again at its judgment.
    rel: &'static str,
}

/// What a pass may read ([`candidates`]).
struct Listed {
    /// The children, in [`PASS_ROOTS`]' order and by path within a root.
    children: Vec<Candidate>,
    /// A guarded root went unlisted that a later look reads: a first read under it has not
    /// returned, and that read's own walk has not run out of time.
    pending: bool,
}

/// The children of [`PASS_ROOTS`] under the resolved `home` a pass at `now` may read, in root
/// order and by path within a root: proven caches alone ([`evidence`]), none a root keeps by
/// name, none that is or holds a place whose units atpkg's leftovers sweep judges (the campaign
/// scopes, whose receipts, evidence and sources stay), none read too recently to have changed its
/// answer ([`LATER`]), and no guarded child while a read of one under its root is held. A root
/// that is a link, or on another volume than the home folder, lists nothing.
fn candidates(home: &Path, now: i64) -> Listed {
    let mut out = Listed {
        children: Vec::new(),
        pending: false,
    };
    let Some(home_dev) = aterm_sysprobe::volumes::file_id(home, true).map(|id| id.dev) else {
        return out;
    };
    let places = atpkg::build_cache::leftovers::places(home);
    for (rel, guarded) in PASS_ROOTS {
        let Some(root) = ROOTS.iter().find(|root| root.rel == *rel) else {
            continue;
        };
        let dir = root.under(home);
        if !real_path(&dir).is_ok_and(|real| same(&real, &dir))
            || aterm_sysprobe::volumes::file_id(&dir, false).map(|id| id.dev) != Some(home_dev)
        {
            continue;
        }
        if *guarded {
            let stops = locked(&HELD)
                .iter()
                .filter(|(held, _)| held.starts_with(&dir))
                .map(|(_, until)| *until)
                .max();
            if let Some(until) = stops {
                // A read still held after its own walk ran out of time waits on what no look
                // can end (a privacy dialog nobody answers): its root is out of the pass's
                // reach, and a pass that leaves it has not stopped short.
                out.pending |= Instant::now() < until;
                continue;
            }
        }
        let mut listed = children(root, &dir);
        listed.sort();
        let gone = |path: &PathBuf| {
            path.parent() == Some(dir.as_path()) && listed.binary_search(path).is_err()
        };
        locked(&LATE).retain(|path, _| !gone(path));
        let fresh: Vec<PathBuf> = {
            let mut later = locked(&LATER);
            later.retain(|path, _| !gone(path));
            listed
                .iter()
                .filter(|path| later.get(*path).is_none_or(|at| *at <= now))
                .cloned()
                .collect()
        };
        out.children.extend(
            fresh
                .into_iter()
                .filter(|path| !places.iter().any(|place| place.root.starts_with(path)))
                .filter(|path| evidence(rel, path).is_some())
                .map(|path| Candidate {
                    path,
                    guarded: *guarded,
                    rel,
                }),
        );
    }
    out
}

/// `passed` stopped short for `why`; the first reason stands.
fn cut(passed: &mut Passed, why: &str) {
    if passed.cut.is_none() {
        passed.cut = Some(why.to_owned());
    }
}

/// THE WATCH'S OWN PASS OVER THE OUTSIDE CACHES: first finish what a removal cut short left set
/// aside ([`finish_staged`]), then free the children of [`PASS_ROOTS`] under `world.home` that
/// are proven caches ([`evidence`]) and that nothing wrote, changed or read within
/// `[disk] idle_build_days`, least recently used first, until `measure` reads free space at
/// `goal`. A child with no evidence is never read, dated or removed. It chooses by record: each
/// child is dated behind the fences that read nothing under it ([`read`]; a guarded root's
/// through `reads.dater`, on a thread of its own), and one whose turn comes passes every fence of
/// [`judge`] on the caller's thread, is dated and its evidence read again on that judgment, and
/// is removed as the verb removes ([`run`]), but renamed aside first ([`STAGE`]): its `removing`
/// row through `journal` first, none written, nothing removed. One scan of the running processes answers for every child
/// chosen, so a child a process uses stays without a judgment, and the removal scans again for
/// the one it takes. `measure` is read before every read and every removal (`None` stops the
/// pass there), and the pass also stops once what it removed covers the shortfall it first
/// measured, as a tier does while the free figure lags.
///
/// Its work is bounded: dating stops `reads.wall` after it starts, a child read fresh is not
/// read again until its window could have passed, one kept for another reason not for an hour,
/// and at most [`PASS_KEPT_MAX`] children are judged and kept. A partial walk, or a child no
/// time was read of, stays. A guarded child whose first read a path fence refuses is left with
/// one `refused` row. One whose first read does not return within `reads.wait` is left with
/// one `refused` row ([`HELD_READ`]), and its answer is taken by the next pass once the read
/// returns; no other guarded child is read this pass, and none under that root by any pass
/// while that read is pending.
///
/// A pass that could not look says so in [`Passed::cut`], so nothing reads it as a pass that
/// ran out: the mount table was not read whole, the home folder could not be resolved, a first
/// read timed out, or a guarded root was left because a read under it is still walking. A read
/// held past its own walk's deadline cuts nothing: only a person can end that wait, and what
/// lies behind it is out of the pass's reach.
///
/// With the grant off, with `[disk] idle_build_days` below 1 (no window, so nothing goes by
/// its idleness) and on Windows (whose walker dates nothing), it does nothing. A home with
/// nothing to read costs no `measure`.
pub fn pass(
    world: &World<'_>,
    goal: u64,
    reads: &Reads<'_>,
    measure: &mut super::Measure<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
) -> Passed {
    let mut passed = Passed::default();
    if cfg!(windows) || !world.grant() || world.config.idle_build_days < 1 {
        return passed;
    }
    for outcome in finish_staged(world, journal) {
        passed.bytes = passed.bytes.saturating_add(outcome.bytes());
        passed.outcomes.push(outcome);
    }
    sweep(world, goal, reads, measure, journal, &mut passed);
    for (path, why) in &passed.refused {
        journal(&row(world.now, world.sid, path, "refused", None, why));
    }
    for path in &passed.held {
        journal(&row(world.now, world.sid, path, "refused", None, HELD_READ));
    }
    passed
}

/// [`pass`]'s reads and removals, into `passed`.
fn sweep(
    world: &World<'_>,
    goal: u64,
    reads: &Reads<'_>,
    measure: &mut super::Measure<'_>,
    journal: &mut dyn FnMut(&str) -> bool,
    passed: &mut Passed,
) {
    let started = Instant::now();
    let window = world.config.idle_build_days.saturating_mul(super::DAY_S);
    let idle = |used: &CacheUse| {
        !used.partial
            && used
                .newest
                .is_some_and(|at| world.now.saturating_sub(at) >= window)
    };
    let Ok(home) = real_path(world.home) else {
        cut(passed, CUT_HOME);
        return;
    };
    let Listed {
        children: candidates,
        pending,
    } = candidates(&home, world.now);
    if pending {
        cut(passed, CUT_PENDING);
    }
    if candidates.is_empty() {
        return;
    }
    // No fence admits a path while the mount table is not read whole: nothing is read, and
    // nothing is put off for an hour on a table the next look may read.
    if !world.mounts.is_some_and(|table| table.complete) {
        cut(passed, CUT_MOUNTS);
        return;
    }
    let reading = Reading {
        home,
        mounts: world.mounts.cloned(),
        until: started + reads.wall,
        wait: reads.wait,
    };
    let later = |path: &Path, at: i64| {
        locked(&LATER).insert(path.to_path_buf(), at);
    };
    let retry = world.now.saturating_add(RETRY_S);
    // What free space was short of the goal at the pass's first measure.
    let mut short: Option<u64> = None;
    let mut unused: Vec<(i64, PathBuf, &'static str)> = Vec::new();
    let mut waited = false;
    for candidate in &candidates {
        if candidate.guarded && waited {
            continue;
        }
        if Instant::now() >= reading.until {
            cut(passed, CUT_WALL);
            break;
        }
        match measure() {
            None => {
                cut(passed, CUT_UNMEASURED);
                return;
            }
            Some(free) if free >= goal => return,
            Some(free) => short = short.or(Some(goal - free)),
        }
        aterm_uds::progress::tick();
        let path = candidate.path.as_path();
        let dated = if candidate.guarded {
            // An answer an earlier look stopped waiting for is taken before a second read.
            let late = locked(&LATE).remove(path);
            match late {
                Some(answer) => Dated::At(answer),
                None => (reads.dater)(path, &reading),
            }
        } else {
            Dated::At(read(path, &reading))
        };
        match dated {
            Dated::At(Ok(used)) => match used.newest {
                Some(newest) if idle(&used) => {
                    if used.bytes >= PASS_MIN_BYTES {
                        unused.push((newest, candidate.path.clone(), candidate.rel));
                    } else {
                        later(path, retry);
                    }
                }
                _ if used.partial && Instant::now() >= reading.until => {
                    // Put off like any child a read could not date, so the next pass reads the
                    // children after it first: one folder too large to walk in the wall clock
                    // never holds back the rest.
                    later(path, retry);
                    cut(passed, CUT_WALL);
                    break;
                }
                Some(newest) if !used.partial => later(path, newest.saturating_add(window)),
                _ => later(path, retry),
            },
            Dated::At(Err(why)) => {
                later(path, retry);
                if candidate.guarded && locked(&SAID).insert(candidate.path.clone()) {
                    passed.refused.push((candidate.path.clone(), why));
                }
            }
            Dated::TimedOut { said } => {
                waited = true;
                cut(passed, CUT_HELD);
                if !said {
                    passed.held.push(candidate.path.clone());
                }
            }
            Dated::StillHeld => {
                waited = true;
                cut(passed, CUT_PENDING);
            }
        }
    }
    unused.sort();
    let chosen: Vec<PathBuf> = unused.iter().map(|(_, path, _)| path.clone()).collect();
    // One scan answers for every folder chosen, made once a removal is due. It names each
    // process's first use only, so a folder it clears is scanned again, alone, by `remove`.
    let mut scanned: Option<std::io::Result<aterm_uds::holders::PathScan>> = None;
    let mut kept = 0usize;
    for (_, path, rel) in unused {
        if kept >= PASS_KEPT_MAX {
            cut(passed, CUT_KEPT);
            break;
        }
        match measure() {
            None => {
                cut(passed, CUT_UNMEASURED);
                return;
            }
            Some(free) if free >= goal => return,
            Some(free) => {
                if passed.bytes >= *short.get_or_insert(goal - free) {
                    cut(passed, CUT_COUNTED);
                    return;
                }
            }
        }
        aterm_uds::progress::tick();
        let scan = scanned
            .get_or_insert_with(|| (world.processes)(chosen.as_slice(), &[std::process::id()]));
        let in_use = discover::scan_says_unused(scan, |user| {
            aterm_path::is_under(&user.path, &path) == Some(true)
        })
        .is_err();
        if in_use {
            later(path.as_path(), retry);
            continue;
        }
        // The date and the evidence are read again on the judgment the `removing` row is
        // written from: a folder written or untagged since it was chosen stays.
        let admitted = placed(&path, world.home, world.mounts)
            .and_then(|at| unmanaged(&path, at))
            .ok()
            .filter(|a| {
                a.bytes >= PASS_MIN_BYTES
                    && evidence(rel, &path).is_some()
                    && idle(&CacheUse {
                        bytes: a.bytes,
                        partial: a.partial,
                        newest: a.last_use,
                    })
            });
        let Some(admitted) = admitted else {
            later(path.as_path(), retry);
            kept += 1;
            continue;
        };
        let verdict = remove(&admitted, world, journal, Removal::Staged);
        if let Verdict::Refused(why) = &verdict {
            journal(&row(world.now, world.sid, &path, "refused", None, why));
        }
        if matches!(verdict, Verdict::Refused(_) | Verdict::Failed { .. }) {
            later(path.as_path(), retry);
            kept += 1;
        }
        let outcome = Outcome { path, verdict };
        passed.bytes = passed.bytes.saturating_add(outcome.bytes());
        passed.outcomes.push(outcome);
    }
}

#[cfg(test)]
#[path = "disk_free_tests.rs"]
mod tests;

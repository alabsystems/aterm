// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What each check of a run reads beyond its tree, as `verify land`'s verdict records key it
//! (`docs/DESIGN-landing-economy.md` slice 4).
//!
//! Each planned check gets a key, `H(`[`KEY_SCHEMA`]`, check, T, part, seal)`, over T (the
//! tree of the commit the run judges), the check's planned part ([`crate::checks::part`]: its
//! stages, the base commit, the canonical scope, the changed paths and the flags it reads) and
//! its portable seal ([`seal`]: the content digests of what it reads outside T, which two Macs
//! with the same installs read alike). Beside the keys stands the run's local digest
//! ([`LOCAL`]: this checkout, the OS build, the model, the developer directory, the effective
//! environment less [`UNSEALED_ENV`], and the configuration files outside T), which a record
//! carries outside its key. A run that keeps an events file says them all in `run-planned`
//! before its first stage, and just before `run-finished` says in `run-resealed` what each field
//! reads then: a pass whose inputs moved while it ran was not sealed through.
//!
//! A field that cannot be read, and every field of a check whose inputs the seal does not cover
//! ([`sealed`]), takes this run's nonce, so its keys match no record; `run-planned`'s
//! `unsealed` says why. Off macOS the content fields are such fields.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Ctx;
use crate::checks::Check;

/// The key's schema, its first part.
pub const KEY_SCHEMA: &str = "aterm-land-check-v1";
/// The portable seal's schema.
pub const SEAL_SCHEMA: &str = "aterm-land-seal-v1";
/// The planned part's schema ([`crate::checks::part`]).
pub const PART_SCHEMA: &str = "aterm-land-part-v1";
/// The plan digest's schema: over the required checks' `(check, key)` pairs.
pub const PLAN_SCHEMA: &str = "aterm-land-plan-v1";
/// The local digest's schema.
pub const LOCAL_SCHEMA: &str = "aterm-land-local-v1";

/// The host platform every compile targets (`aterm_uds::host::host_platform`).
pub const TRIPLE: &str = "triple";
/// The selected Trust build by content: its `bin` and its host libraries.
pub const TOOLCHAIN: &str = "toolchain";
/// The macOS SDK, `/usr/bin`'s drivers and the Apple toolchain trees they resolve to, by content.
pub const APPLE: &str = "apple";
/// The Git every check's reads run (`git_provider::pin`).
pub const GIT: &str = "git";
/// The stock pin's toolchain and target libraries, for a check that needs `rustup`.
pub const STOCK: &str = "stock";
/// The prefix of a check's tool field: one per name in its `Spec.needs`.
pub const TOOLS: &str = "tools/";
/// The engine's forbidden-content baseline, as the run's one read of the engine holds it.
pub const ENGINE_BASELINE: &str = "external/baseline";
/// The engine's repository registry, as the run's one read of the engine holds it.
pub const ENGINE_REGISTRY: &str = "external/registry";
/// The installed release released-shim judges with: its store `targo` shim, the helper that
/// shim names and its app's `Info.plist`.
pub const RELEASE: &str = "release";
/// The two newest fetched public release tags in the candidate's ancestry, which decide whether
/// the installed release may judge.
pub const RELEASE_TAGS: &str = "release-tags";

/// The fields every check reads.
pub const COMMON: [&str; 4] = [TRIPLE, TOOLCHAIN, APPLE, GIT];

/// This checkout: the canonical worktree and its git common directory, digested.
pub const CHECKOUT: &str = "checkout";
/// `kern.osversion`.
pub const OS: &str = "os";
/// `hw.model`.
pub const MODEL: &str = "model";
/// `DEVELOPER_DIR`, else `xcode-select -p`, digested.
pub const DEVELOPER_DIR: &str = "developer_dir";
/// The verifier's own environment but [`UNSEALED_ENV`], `PATH` deduplicated in order.
pub const ENV: &str = "env";
/// The Cargo, clippy and rustfmt configuration outside T that the run's tools read.
pub const CONFIG: &str = "config";

/// The local attributes, outside every key.
pub const LOCAL: [&str; 6] = [CHECKOUT, OS, MODEL, DEVELOPER_DIR, ENV, CONFIG];

/// The variables [`ENV`] leaves out, each with why: per-Mac shaping, which decides how a run is
/// spread over the machine and never what it judges, and what each login or session sets anew.
/// An entry ending in `*` names a prefix. A variable missing here costs a reuse, never a false
/// one.
pub const UNSEALED_ENV: [(&str, &str); 23] = [
    ("CARGO_BUILD_JOBS", "shaping: the build's width"),
    (
        "BUILD_SLOT_HELD",
        "shaping: the build slot share a caller holds",
    ),
    ("RUST_TEST_THREADS", "shaping: the test binaries' width"),
    ("ATERM_RELEASE_POOL", "shaping: the release's CPU pool"),
    (
        "ATERM_DISK_COVERED",
        "shaping: the disk claim that covers the run",
    ),
    ("TMPDIR", "per login: the session's temporary directory"),
    (
        "XDG_RUNTIME_DIR",
        "per login: the session's runtime directory",
    ),
    ("SSH_*", "per session: the connection a shell came in by"),
    ("TERM", "per session: the terminal"),
    (
        "TERM_*",
        "per session: the terminal program and its session",
    ),
    ("COLORTERM", "per session: the terminal's colours"),
    ("SHLVL", "per session: the shell's nesting"),
    ("PWD", "per session: the shell's directory"),
    ("OLDPWD", "per session: the shell's previous directory"),
    ("_", "per session: the last command the shell ran"),
    ("ATERM_SESSION_ID", "per session: the aterm tab"),
    (
        "ATERM_CHILD",
        "per session: that a program runs under aterm",
    ),
    ("SECURITYSESSIONID", "per login: the security session"),
    ("LaunchInstanceID", "per login: launchd's instance"),
    (
        "__CF_USER_TEXT_ENCODING",
        "per login: CoreFoundation's text encoding",
    ),
    (
        "Apple_PubSub_Socket_Render",
        "per login: a legacy notification socket",
    ),
    ("XPC_SERVICE_NAME", "per login: the launching XPC service"),
    ("XPC_FLAGS", "per login: the launching XPC service's flags"),
];

/// Whether [`UNSEALED_ENV`] leaves `name` out.
#[must_use]
pub fn unsealed_env(name: &str) -> bool {
    UNSEALED_ENV
        .iter()
        .any(|(entry, _)| match entry.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == *entry,
        })
}

/// SHA-256 over NUL-terminated `parts`, lowercase hex: the key's encoding, as A19's
/// `check_record::key`.
#[must_use]
pub fn hash<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut h = aterm_digest::Sha256::new();
    for part in parts {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// [`hash`] as a digest string: `sha256:<hex>`.
#[must_use]
pub fn digest<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    format!("sha256:{}", hash(parts))
}

/// The prefix of a field this run could not read: its nonce follows, so nothing matches it.
pub const UNRESOLVED: &str = "unresolved:";

/// What a reseal says of a field whose reading changed, or could not be read again.
pub const MOVED: &str = "moved";

/// Whether the seal covers what `check` reads beyond its tree: the dev set and every check it
/// pulls. Another check (a bundle's `measure`, the network's `export`, a lab's) reads inputs the
/// fields do not name, so its key is unresolved.
#[must_use]
pub fn sealed(check: Check) -> bool {
    crate::checks::DEV.contains(&check)
        || matches!(check, Check::Forge | Check::Appkit | Check::Cells)
}

/// The portable fields `check` reads, sorted: [`COMMON`]; one per tool it needs; [`STOCK`] where
/// it needs the stock pin's `rustup`; the engine's two files export-scan reads and the baseline and
/// the public release census reads; and the installed release and the tags released-shim judges
/// by.
#[must_use]
pub fn fields_of(check: Check) -> Vec<String> {
    let spec = check.spec();
    let mut fields: Vec<String> = COMMON.iter().map(|f| (*f).to_owned()).collect();
    fields.extend(spec.needs.iter().map(|need| format!("{TOOLS}{need}")));
    if spec.needs.contains(&"rustup") {
        fields.push(STOCK.to_owned());
    }
    match check {
        Check::ExportScan => fields.extend([ENGINE_BASELINE, ENGINE_REGISTRY].map(String::from)),
        Check::Census => fields.push(ENGINE_BASELINE.to_owned()),
        Check::ReleasedShim => fields.extend([RELEASE, RELEASE_TAGS].map(String::from)),
        _ => {}
    }
    fields.sort_unstable();
    fields.dedup();
    fields
}

/// The external fields, read from the run's one read of the engine and of the public release
/// ([`Ctx::engine`], [`Ctx::harness`]), which every check of the run judges by: pinned for the
/// run, so never resealed.
fn external(name: &str) -> bool {
    name.starts_with("external/")
}

/// `check`'s seal over `fields` (each portable field's reading): `H(`[`SEAL_SCHEMA`]`, each of
/// its fields' name and digest, in name order)`.
#[must_use]
pub fn seal(check: Check, fields: &BTreeMap<String, String>) -> String {
    let own = fields_of(check);
    let mut parts: Vec<&str> = vec![SEAL_SCHEMA];
    for name in &own {
        parts.push(name);
        parts.push(fields.get(name).map_or(MOVED, String::as_str));
    }
    digest(parts)
}

/// The local digest over `local` (each local attribute's reading).
#[must_use]
pub fn local_digest(local: &BTreeMap<String, String>) -> String {
    let mut parts: Vec<&str> = vec![LOCAL_SCHEMA];
    for (name, value) in local {
        parts.push(name);
        parts.push(value);
    }
    digest(parts)
}

/// `check`'s key: `H(`[`KEY_SCHEMA`]`, check, tree, part, seal)`, bare hex.
#[must_use]
pub fn key(check: Check, tree: &str, part: &str, seal: &str) -> String {
    hash([KEY_SCHEMA, check.name(), tree, part, seal])
}

/// What a machine reads for a plan: each portable field but the external ones, and each local
/// attribute. The live one reads this Mac ([`Live`]); a test hands fixed readings.
pub trait Machine: Send {
    /// The field `name`'s reading now: a digest, or why it could not be read.
    ///
    /// # Errors
    /// The field could not be read.
    fn read(&mut self, ctx: &Ctx, name: &str) -> Result<String, String>;
    /// The field `name`'s reading after the run: the same digest while what it read stands (by
    /// status where it has files), else why not.
    ///
    /// # Errors
    /// What it read moved, or could not be read again.
    fn reseal(&mut self, ctx: &Ctx, name: &str) -> Result<String, String>;
}

/// What the run judges in its checkout: the commit, its tree, the base commit its cone is cut
/// from, the changed paths' digest, and whether the checkout is clean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// The commit judged: HEAD when the run's selection was read.
    pub commit: String,
    /// Its tree, T.
    pub tree: String,
    /// The merge-base of HEAD and the base ref: `Scope::Changed`'s base, else `origin/main`, as
    /// the changelog guard reads it.
    pub base: String,
    /// The digest of the changed paths since the base commit, in order.
    pub changed: String,
    /// `git status --porcelain=v1 --untracked-files=normal --ignore-submodules=none` printed
    /// nothing: what `verify land` holds a checkout to.
    pub clean: bool,
}

impl Tree {
    /// `ctx`'s checkout as the run reads it.
    ///
    /// # Errors
    /// A git read failed.
    pub fn read(ctx: &Ctx) -> Result<Self, String> {
        let git = |args: &[&str]| -> Result<String, String> {
            let out = crate::changed::git(&ctx.root, &ctx.path_env)
                .args(args)
                .output()
                .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
            if !out.status.success() {
                return Err(format!(
                    "git {}: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let commit = match &ctx.head_at_start {
            Some(head) => head.clone(),
            None => git(&["rev-parse", "--verify", "HEAD^{commit}"])?
                .trim()
                .to_owned(),
        };
        let tree = git(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?
            .trim()
            .to_owned();
        let base_ref = match &ctx.scope {
            crate::Scope::Changed(changed) => changed.base.as_str(),
            _ => "origin/main",
        };
        let (base, mut paths) = crate::changed::changed_paths(&ctx.root, &ctx.path_env, base_ref)?;
        paths.sort_unstable();
        let changed = digest(paths.iter().map(String::as_str));
        let status = git(&[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])?;
        Ok(Self {
            commit,
            tree,
            base,
            changed,
            clean: status.trim().is_empty(),
        })
    }
}

/// One required check of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Required {
    pub check: Check,
    /// Its planned part ([`crate::checks::part`]).
    pub part: String,
    /// Its key ([`key`]).
    pub key: String,
    /// Its portable seal ([`seal`]).
    pub seal: String,
}

/// A run's plan: what `run-planned` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// What the run judges ([`Tree`]); unresolved, each part is this run's nonce.
    pub tree: Tree,
    /// The full plan's checks, pulls included, in [`crate::checks::CHECKS`] order, each keyed.
    pub required: Vec<Required>,
    /// Every portable field a required check reads, by name.
    pub fields: BTreeMap<String, String>,
    /// Every local attribute, by name.
    pub local: BTreeMap<String, String>,
    /// [`local_digest`] of [`Plan::local`].
    pub local_digest: String,
    /// `H(`[`PLAN_SCHEMA`]`, each required check and its key)`.
    pub plan_digest: String,
    /// Each field (or check) this run could not seal, and why.
    pub unsealed: Vec<(String, String)>,
}

/// This run's nonce: what an unresolved field reads, so it matches no other run's.
fn nonce() -> String {
    aterm_uds::rand::hex_token::<16>()
        .unwrap_or_else(|_| format!("{}-{}", std::process::id(), atpkg::input_seal::now_ns()))
}

impl Plan {
    /// The plan of `ctx`'s checks, its fields read by `machine` and its tree read as `tree`.
    #[must_use]
    pub fn make(ctx: &Ctx, machine: &mut dyn Machine, tree: Result<Tree, String>) -> Self {
        let nonce = nonce();
        let unresolved = format!("{UNRESOLVED}{nonce}");
        let mut unsealed: Vec<(String, String)> = Vec::new();
        let tree = tree.unwrap_or_else(|why| {
            unsealed.push(("tree".to_owned(), why));
            Tree {
                commit: unresolved.clone(),
                tree: unresolved.clone(),
                base: unresolved.clone(),
                changed: unresolved.clone(),
                clean: false,
            }
        });
        let mut read = |name: &str, unsealed: &mut Vec<(String, String)>| {
            let reading = if external(name) {
                external_reading(ctx, name)
            } else {
                machine.read(ctx, name)
            };
            reading.unwrap_or_else(|why| {
                unsealed.push((name.to_owned(), why));
                unresolved.clone()
            })
        };
        let mut fields = BTreeMap::new();
        for check in &ctx.checks {
            for name in fields_of(*check) {
                if let std::collections::btree_map::Entry::Vacant(slot) = fields.entry(name) {
                    let value = read(slot.key(), &mut unsealed);
                    slot.insert(value);
                }
            }
        }
        let local: BTreeMap<String, String> = LOCAL
            .iter()
            .map(|name| ((*name).to_owned(), read(name, &mut unsealed)))
            .collect();
        let required: Vec<Required> = ctx
            .checks
            .iter()
            .map(|&check| {
                let part = crate::checks::part(check, ctx, &tree.base, &tree.changed);
                let seal = if sealed(check) {
                    seal(check, &fields)
                } else {
                    unsealed.push((
                        check.name().to_owned(),
                        "it reads inputs the land seal does not name".to_owned(),
                    ));
                    unresolved.clone()
                };
                Required {
                    check,
                    key: key(check, &tree.tree, &part, &seal),
                    part,
                    seal,
                }
            })
            .collect();
        let plan_digest = plan_digest(&required);
        Self {
            tree,
            required,
            fields,
            local_digest: local_digest(&local),
            local,
            plan_digest,
            unsealed,
        }
    }

    /// The required row of `check`, when the plan holds it.
    #[must_use]
    pub fn row(&self, check: Check) -> Option<&Required> {
        self.required.iter().find(|row| row.check == check)
    }

    /// Every field again after the run, by `machine` ([`Machine::reseal`]): the external fields
    /// and those this run never sealed read as they did; a field whose reading changed, or
    /// could not be read again, reads [`MOVED`].
    #[must_use]
    pub fn reseal(&self, ctx: &Ctx, machine: &mut dyn Machine) -> Resealed {
        let mut again = |name: &str, was: &String| {
            if external(name) || was.starts_with(UNRESOLVED) {
                return was.clone();
            }
            machine
                .reseal(ctx, name)
                .unwrap_or_else(|_| MOVED.to_owned())
        };
        let fields: BTreeMap<String, String> = self
            .fields
            .iter()
            .map(|(name, was)| (name.clone(), again(name, was)))
            .collect();
        let local: BTreeMap<String, String> = self
            .local
            .iter()
            .map(|(name, was)| (name.clone(), again(name, was)))
            .collect();
        let moved: Vec<String> = fields
            .iter()
            .chain(&local)
            .filter(|(name, now)| {
                self.fields.get(*name).or_else(|| self.local.get(*name)) != Some(now)
            })
            .map(|(name, _)| name.clone())
            .collect();
        let seals = self
            .required
            .iter()
            .map(|row| {
                let now = if row.seal.starts_with(UNRESOLVED) {
                    row.seal.clone()
                } else {
                    seal(row.check, &fields)
                };
                (row.check, now)
            })
            .collect();
        Resealed {
            sealed_through: moved.is_empty(),
            seals,
            local_digest: local_digest(&local),
            fields,
            local,
            moved,
        }
    }
}

/// `H(`[`PLAN_SCHEMA`]`, each check and its key)`.
fn plan_digest(required: &[Required]) -> String {
    let mut parts: Vec<&str> = vec![PLAN_SCHEMA];
    for row in required {
        parts.push(row.check.name());
        parts.push(&row.key);
    }
    digest(parts)
}

/// The external field `name`, from the run's one read of the engine or of the public release.
fn external_reading(ctx: &Ctx, name: &str) -> Result<String, String> {
    let file = |text: &Result<String, String>, label: &str| {
        text.as_deref()
            .map(|text| digest([label, text]))
            .map_err(Clone::clone)
    };
    match name {
        ENGINE_BASELINE => ctx
            .engine()
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|files| file(&files.baseline, "baseline")),
        ENGINE_REGISTRY => ctx
            .engine()
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|files| file(&files.registry, "registry")),
        _ => Err(format!("{name} is no external field")),
    }
}

/// What a run's fields read just before it ended: `run-resealed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resealed {
    /// Each required check's seal over [`Resealed::fields`], in plan order.
    pub seals: Vec<(Check, String)>,
    /// Each portable field again; [`MOVED`] where it moved.
    pub fields: BTreeMap<String, String>,
    /// Each local attribute again; [`MOVED`] where it moved.
    pub local: BTreeMap<String, String>,
    /// [`local_digest`] of [`Resealed::local`].
    pub local_digest: String,
    /// The fields whose reading differs from the plan's.
    pub moved: Vec<String>,
    /// Every field reads as the plan read it, so each check's seal and the local digest stood
    /// through the run.
    pub sealed_through: bool,
}

/// What the run's check rows say beside their words ([`crate::report::keyed_check_event`]): the
/// key, the crate scope of a crate-scoped check, and what an external check read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyed {
    pub key: String,
    /// The crates a crate-scoped check judged ([`Check::crate_scoped`]: lint, test, doc), and
    /// whether the selection was widened to the whole workspace.
    pub scope: Option<(Vec<String>, bool)>,
    /// export-scan: the engine commit and the digests of the two files it read; census: the
    /// baseline's digest and the public release's version and commit.
    pub external: Option<BTreeMap<String, String>>,
}

/// The plan of a run that keeps one, and the machine that reseals it.
pub struct Run {
    pub plan: Plan,
    machine: Mutex<Box<dyn Machine>>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl Run {
    /// `plan`, resealed by `machine` at the run's end.
    #[must_use]
    pub fn new(plan: Plan, machine: Box<dyn Machine>) -> Self {
        Self {
            plan,
            machine: Mutex::new(machine),
        }
    }

    /// [`Plan::reseal`] by the run's machine.
    #[must_use]
    pub fn reseal(&self, ctx: &Ctx) -> Resealed {
        let mut machine = self
            .machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.plan.reseal(ctx, machine.as_mut())
    }

    /// What `check`'s row says beside its word, when the plan holds it.
    #[must_use]
    pub fn keyed(&self, ctx: &Ctx, check: Check) -> Option<Keyed> {
        let row = self.plan.row(check)?;
        let scope = check
            .crate_scoped()
            .then(|| (ctx.scope.crates().to_vec(), ctx.scope.is_workspace()));
        let mut read = BTreeMap::new();
        if check == Check::ExportScan
            && let Ok(files) = ctx.engine()
        {
            read.insert("commit".to_owned(), files.sha.clone());
        }
        for name in fields_of(check).iter().filter(|name| external(name)) {
            if let Some(value) = self.plan.fields.get(name) {
                read.insert(name.clone(), value.clone());
            }
        }
        Some(Keyed {
            key: row.key.clone(),
            scope,
            external: (!read.is_empty()).then_some(read),
        })
    }
}

/// The walk schema and the kept seal's schema of the content fields: a field's walk is held to
/// `atpkg::input_seal`'s, as the release's build inputs are.
#[cfg(target_os = "macos")]
const WALK: atpkg::input_seal::Schema = atpkg::input_seal::Schema {
    walk: b"aterm-land-seal-walk-v1",
    seal: "aterm-land-seal-kept-v1",
};

/// What [`Live`] holds of a field to reseal it.
enum Held {
    /// A content walk ([`TOOLCHAIN`], [`APPLE`], [`STOCK`]): the seal this process holds over
    /// those roots, rechecked by status.
    #[cfg(target_os = "macos")]
    Walk {
        process: Box<atpkg::input_seal::Process>,
        identity: String,
        roots: Vec<atpkg::input_seal::Root>,
        digest: String,
    },
    /// Files read whole ([`GIT`], a tool, [`RELEASE`]): each one's status when it was read.
    Files {
        files: Vec<(PathBuf, aterm_toolchain::stat_digest::StatKey)>,
        digest: String,
    },
}

/// This Mac's readings. Each content field's seal is kept under the account's state, so the next
/// run adopts it after a recheck by status instead of hashing again.
#[derive(Default)]
pub struct Live {
    held: BTreeMap<String, Held>,
}

impl Live {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The field `name` read now; what to reseal it by is kept.
    fn take(&mut self, ctx: &Ctx, name: &str) -> Result<String, String> {
        match name {
            TRIPLE => Ok(digest([TRIPLE, &aterm_uds::host::host_platform()])),
            TOOLCHAIN | APPLE | STOCK => self.walk(ctx, name),
            GIT => {
                let git = crate::git_provider::pin().ok_or("no absolute executable Git on PATH")?;
                self.files(name, &[git.to_path_buf()], &[])
            }
            RELEASE => {
                let (files, why) = release_files(ctx)?;
                self.files(name, &files, &why)
            }
            RELEASE_TAGS => release_tags(ctx),
            CHECKOUT => checkout(ctx),
            OS => aterm_sysprobe::os_build().ok_or_else(|| "kern.osversion is unread".to_owned()),
            MODEL => aterm_sysprobe::machine_model().ok_or_else(|| "hw.model is unread".to_owned()),
            DEVELOPER_DIR => developer_dir(ctx),
            ENV => Ok(environment(std::env::vars_os())),
            CONFIG => config(ctx),
            tool if tool.starts_with(TOOLS) => {
                let need = &tool[TOOLS.len()..];
                match tool_file(ctx, need)? {
                    ToolFile::Hardware => Ok(digest([tool, "hardware"])),
                    ToolFile::Absent => Ok(digest([tool, "absent"])),
                    ToolFile::At(path) => self.files(name, &[path], &[]),
                }
            }
            _ => Err(format!("{name} is no field this machine reads")),
        }
    }

    /// The digest of `paths` read whole under their status, with `also` beside them; kept to
    /// reseal by status.
    fn files(&mut self, name: &str, paths: &[PathBuf], also: &[String]) -> Result<String, String> {
        let mut parts: Vec<String> = vec![name.to_owned()];
        let mut files = Vec::new();
        for path in paths {
            let (bytes, stat) = read_under_status(path)?;
            parts.push(bytes);
            files.push((path.clone(), stat));
        }
        parts.extend(also.iter().cloned());
        let digest = digest(parts.iter().map(String::as_str));
        self.held.insert(
            name.to_owned(),
            Held::Files {
                files,
                digest: digest.clone(),
            },
        );
        Ok(digest)
    }

    #[cfg(target_os = "macos")]
    fn walk(&mut self, ctx: &Ctx, name: &str) -> Result<String, String> {
        use atpkg::input_seal;
        let roots = match name {
            TOOLCHAIN => toolchain_roots(ctx)?,
            APPLE => input_seal::apple_roots(input_seal::macos_sdk()?)?,
            _ => stock_roots(ctx)?,
        };
        let mut process = input_seal::Process::unstarted(WALK);
        if let Some(state) = aterm_types::dirs::account_state_dir() {
            process.arm(state.join("verify").join("land-seal"));
        }
        let mut resolved = || Ok(roots.clone());
        let mut facts = input_seal::machine_facts;
        let mut open = |path: &Path| crate::open_regular(path);
        let mut now = input_seal::now_ns;
        let stopped = crate::stopping;
        let identity = process.identity(
            &mut input_seal::Sources {
                roots: &mut resolved,
                facts: &mut facts,
                open: &mut open,
                now: &mut now,
                stopped: &stopped,
                report: &mut |_| {},
            },
            false,
        )?;
        let Some(content) = process.digest() else {
            return Err("its inputs changed within the settle span of the take".to_owned());
        };
        let digest = digest([name, content]);
        self.held.insert(
            name.to_owned(),
            Held::Walk {
                process: Box::new(process),
                identity,
                roots,
                digest: digest.clone(),
            },
        );
        Ok(digest)
    }

    #[cfg(not(target_os = "macos"))]
    fn walk(&mut self, _: &Ctx, name: &str) -> Result<String, String> {
        self.held.remove(name);
        Err(format!("{name}: content seals are read on macOS only"))
    }
}

impl Machine for Live {
    fn read(&mut self, ctx: &Ctx, name: &str) -> Result<String, String> {
        self.take(ctx, name)
    }

    fn reseal(&mut self, ctx: &Ctx, name: &str) -> Result<String, String> {
        match self.held.get(name) {
            #[cfg(target_os = "macos")]
            Some(Held::Walk {
                process,
                identity,
                roots,
                digest,
            }) => {
                use atpkg::input_seal;
                let mut resolved = || Ok(roots.clone());
                let mut facts = input_seal::machine_facts;
                let mut open = |path: &Path| crate::open_regular(path);
                let mut now = input_seal::now_ns;
                let held = process.holds(
                    &mut input_seal::Sources {
                        roots: &mut resolved,
                        facts: &mut facts,
                        open: &mut open,
                        now: &mut now,
                        stopped: &|| false,
                        report: &mut |_| {},
                    },
                    identity,
                );
                if held {
                    Ok(digest.clone())
                } else {
                    Err(format!("{name}: a root's content or status changed"))
                }
            }
            Some(Held::Files { files, digest }) => {
                for (path, was) in files {
                    let now =
                        std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
                    if !was.unchanged(&aterm_toolchain::stat_digest::StatKey::of(&now)) {
                        return Err(format!("{} changed", path.display()));
                    }
                }
                Ok(digest.clone())
            }
            None => self.take(ctx, name),
        }
    }
}

/// `path`'s bytes' SHA-256 (hex) and its status, held to the status it had before the read.
fn read_under_status(
    path: &Path,
) -> Result<(String, aterm_toolchain::stat_digest::StatKey), String> {
    use aterm_toolchain::stat_digest::StatKey;
    use std::io::Read as _;
    let shown = || path.display().to_string();
    let mut file = crate::open_regular(path).map_err(|e| format!("{}: {e}", shown()))?;
    let before = StatKey::of(&file.metadata().map_err(|e| format!("{}: {e}", shown()))?);
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hash.update(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("{}: {e}", shown())),
        }
    }
    let after = StatKey::of(&file.metadata().map_err(|e| format!("{}: {e}", shown()))?);
    if !before.unchanged(&after) {
        return Err(format!("{} changed while it was read", shown()));
    }
    Ok((aterm_codec::hex::encode_lower(&hash.finalize()), before))
}

/// Where a check's tool `need` resolves.
enum ToolFile {
    /// A row of the hardware or the session, which no file names.
    Hardware,
    /// Found nowhere: the check is NOT RUN.
    Absent,
    /// The file that identifies it: a placed pin's stamp, else the program itself (canonical).
    At(PathBuf),
}

/// `need` as the run's tools resolve it ([`aterm_uds::host::Tool::locate_in`] over the run's
/// `PATH`): a provisioned pin is identified by its stamp, which names its bytes.
fn tool_file(ctx: &Ctx, need: &str) -> Result<ToolFile, String> {
    let row = aterm_uds::host::tool(need);
    if matches!(row.provider, aterm_uds::host::Provider::Hardware(_)) {
        return Ok(ToolFile::Hardware);
    }
    let lookup = aterm_uds::host::Lookup {
        path: Some(ctx.path_env.clone()),
        ..aterm_uds::host::Lookup::host(row)
    };
    let Ok(found) = row.locate_in(&lookup) else {
        return Ok(ToolFile::Absent);
    };
    if let Some(tooling) = lookup.tooling.as_deref()
        && row.placed_in(tooling).as_deref() == Some(found.as_path())
        && let Some(pinned) = row.pinned_root(tooling)
    {
        return Ok(ToolFile::At(pinned.join(aterm_uds::host::PIN_STAMP)));
    }
    found
        .canonicalize()
        .map(ToolFile::At)
        .map_err(|e| format!("{need} at {}: {e}", found.display()))
}

/// The selected Trust build's roots: its canonical `bin`, the sysroot's `bin` and its host
/// libraries ([`atpkg::input_seal::trust_roots`]).
#[cfg(target_os = "macos")]
fn toolchain_roots(ctx: &Ctx) -> Result<Vec<atpkg::input_seal::Root>, String> {
    use atpkg::input_seal;
    let bin = ctx
        .tools
        .stage2_dir
        .canonicalize()
        .map_err(|e| format!("Trust bin {}: {e}", ctx.tools.stage2_dir.display()))?;
    let sysroot = input_seal::trust_sysroot(&bin)?;
    let host = input_seal::trust_host(&bin)?;
    input_seal::trust_roots(bin, &sysroot, &host)
}

/// The stock pin's roots: its `bin` and the libraries of every target it holds
/// ([`crate::cells::stock_of`]).
#[cfg(target_os = "macos")]
fn stock_roots(ctx: &Ctx) -> Result<Vec<atpkg::input_seal::Root>, String> {
    let stock = crate::cells::stock_of(ctx).map_err(|e| e.why)?;
    let sysroot = stock
        .toolchain
        .rustc
        .parent()
        .and_then(Path::parent)
        .ok_or("the stock rustc has no sysroot")?;
    let targets: Vec<&str> = stock.targets.iter().map(String::as_str).collect();
    let mut roots = vec![("stock/bin".to_owned(), sysroot.join("bin"))];
    roots.extend(atpkg::input_seal::rust_library_roots(
        "stock", sysroot, &targets,
    )?);
    Ok(roots)
}

/// The installed release's files released-shim judges with: the store's `targo` shim, the helper
/// it names and that app's `Info.plist`; with a store whose shim names no helper, the shim alone
/// and why.
fn release_files(ctx: &Ctx) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let prefix = ctx.env.test_package_prefix.clone().unwrap_or_else(|| {
        crate::toolchain::atpkg_prefix(&ctx.env.home, ctx.env.xdg_config_home.as_deref())
    });
    let shim = prefix.join("bin/targo");
    let text = std::fs::read_to_string(&shim).map_err(|e| format!("{}: {e}", shim.display()))?;
    let Some(helper) = atpkg::build_cache::registered::shim_helper(&text) else {
        return Ok((vec![shim], vec!["the shim names no helper".to_owned()]));
    };
    let mut files = vec![shim, helper.clone()];
    if let Some(contents) = helper.parent().and_then(Path::parent) {
        files.push(contents.join("Info.plist"));
    }
    Ok((files, Vec::new()))
}

/// The two newest fetched public release tags in the candidate's ancestry, the ones
/// released-shim picks its judging release from ([`crate::released_shim::public_releases`]).
#[cfg(target_os = "macos")]
fn release_tags(ctx: &Ctx) -> Result<String, String> {
    let manifest = std::fs::read_to_string(ctx.root.join("Cargo.toml"))
        .map_err(|e| format!("Cargo.toml: {e}"))?;
    let manifest: aterm_toml::Value = aterm_toml::from_str(&manifest).map_err(|e| e.to_string())?;
    let current = manifest
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("version"))
        .and_then(aterm_toml::Value::as_str)
        .ok_or("the workspace has no version")?;
    let head = ctx
        .head_at_start
        .clone()
        .or_else(|| crate::head_commit(&ctx.root, &ctx.path_env))
        .ok_or("the candidate has no HEAD")?;
    let releases = crate::released_shim::public_releases(&ctx.root, &ctx.env, &head, current)?;
    let newest = releases.len().saturating_sub(2);
    let mut parts = vec![RELEASE_TAGS];
    parts.extend(releases[newest..].iter().map(String::as_str));
    Ok(digest(parts))
}

/// released-shim judges on macOS alone: elsewhere it reads no tags.
#[cfg(not(target_os = "macos"))]
fn release_tags(_: &Ctx) -> Result<String, String> {
    Err("released-shim reads its release tags on macOS alone".to_owned())
}

/// The canonical worktree and its canonical git common directory, digested: no path leaves the
/// Mac.
fn checkout(ctx: &Ctx) -> Result<String, String> {
    let out = crate::changed::git(&ctx.root, &ctx.path_env)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| format!("git rev-parse --git-common-dir: {e}"))?;
    if !out.status.success() {
        return Err("git rev-parse --git-common-dir failed".to_owned());
    }
    let common = ctx
        .root
        .join(String::from_utf8_lossy(&out.stdout).trim())
        .canonicalize()
        .map_err(|e| format!("the git common directory: {e}"))?;
    let root = ctx
        .root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", ctx.root.display()))?;
    Ok(digest([
        CHECKOUT,
        &root.to_string_lossy(),
        &common.to_string_lossy(),
    ]))
}

/// `DEVELOPER_DIR`, else what `xcode-select -p` prints, digested; off macOS, none.
fn developer_dir(ctx: &Ctx) -> Result<String, String> {
    if let Some(dir) = std::env::var_os("DEVELOPER_DIR") {
        return Ok(digest([
            DEVELOPER_DIR,
            "DEVELOPER_DIR",
            &dir.to_string_lossy(),
        ]));
    }
    if !cfg!(target_os = "macos") {
        return Ok(digest([DEVELOPER_DIR, "none"]));
    }
    let out = aterm_uds::spawnfd::command("/usr/bin/xcode-select")
        .arg("-p")
        .env("PATH", &ctx.path_env)
        .output()
        .map_err(|e| format!("xcode-select -p: {e}"))?;
    if !out.status.success() {
        return Err("xcode-select -p failed".to_owned());
    }
    Ok(digest([
        DEVELOPER_DIR,
        "xcode-select",
        String::from_utf8_lossy(&out.stdout).trim(),
    ]))
}

/// The digest of `vars`, every variable but [`UNSEALED_ENV`]'s in name order, `PATH` replaced by
/// its order-preserving dedupe, which resolves every lookup the same.
#[must_use]
pub fn environment(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> String {
    let mut kept: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for (name, value) in vars {
        let Some(text) = name.to_str() else {
            kept.insert(
                name.as_encoded_bytes().to_vec(),
                value.as_encoded_bytes().to_vec(),
            );
            continue;
        };
        if unsealed_env(text) {
            continue;
        }
        let value = if text == "PATH" {
            let mut seen: Vec<PathBuf> = Vec::new();
            for dir in std::env::split_paths(&value) {
                if !seen.contains(&dir) {
                    seen.push(dir);
                }
            }
            std::env::join_paths(seen).unwrap_or(value)
        } else {
            value
        };
        kept.insert(text.as_bytes().to_vec(), value.as_encoded_bytes().to_vec());
    }
    let mut h = aterm_digest::Sha256::new();
    h.update(ENV.as_bytes());
    for (name, value) in &kept {
        for part in [name, value] {
            h.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
            h.update(part);
        }
    }
    format!("sha256:{}", aterm_codec::hex::encode_lower(&h.finalize()))
}

/// The configuration outside T the run's tools read, digested: `$CARGO_HOME`'s `config.toml`
/// and `config` and each strict ancestor's `.cargo/config.toml` and `.cargo/config`
/// (`lint_content`'s Cargo reader, which refuses a configuration that selects a compiler tool
/// and follows the clippy directories one names), each strict ancestor's clippy configuration,
/// each one's `rustfmt.toml` and `.rustfmt.toml`, and `$XDG_CONFIG_HOME/rustfmt/`'s.
fn config(ctx: &Ctx) -> Result<String, String> {
    let mut hash = aterm_digest::Sha256::new();
    let allowed = std::collections::BTreeSet::new();
    let mut clippy = std::collections::BTreeSet::new();
    let cargo_home = ctx
        .env
        .cargo_home
        .clone()
        .unwrap_or_else(|| ctx.env.home.join(".cargo"));
    for name in ["config.toml", "config"] {
        crate::lint_content::hash_cargo_config(
            &mut hash,
            &cargo_home.join(name),
            &allowed,
            &mut clippy,
        )?;
    }
    let ancestors: Vec<&Path> = ctx.root.ancestors().skip(1).collect();
    for dir in &ancestors {
        for name in [".cargo/config.toml", ".cargo/config"] {
            crate::lint_content::hash_cargo_config(
                &mut hash,
                &dir.join(name),
                &allowed,
                &mut clippy,
            )?;
        }
    }
    clippy.extend(ancestors.iter().map(|dir| dir.to_path_buf()));
    for dir in &clippy {
        crate::lint_content::hash_clippy_directory(&mut hash, dir)?;
    }
    let xdg = ctx
        .env
        .xdg_config_home
        .clone()
        .unwrap_or_else(|| ctx.env.home.join(".config"))
        .join("rustfmt");
    for dir in ancestors.iter().copied().chain([xdg.as_path()]) {
        for name in ["rustfmt.toml", ".rustfmt.toml"] {
            let path = dir.join(name);
            hash.update(path.as_os_str().as_encoded_bytes());
            hash.update(b"\0");
            match std::fs::read(&path) {
                Ok(bytes) => {
                    hash.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
                    hash.update(&bytes);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.update(b"absent\0"),
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
    Ok(format!(
        "sha256:{}",
        aterm_codec::hex::encode_lower(&hash.finalize())
    ))
}

#[cfg(test)]
#[path = "land_seal_tests.rs"]
pub(crate) mod tests;

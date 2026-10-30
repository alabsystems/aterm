// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in disposable package targets, serialized by their producer's whole-build guardian.
//! This policy supplies authority to the existing registered-cache retirement transaction;
//! it never removes a tree itself or adopts an existing target into disposable custody.

use std::path::{Path, PathBuf};

use super::{Identity, Pending, Receipt};

pub const VERB: &str = "__build_cache_package";
pub const CAPABILITY: &str = "aterm-package-build-cache-v1";
pub const DEFAULT_BUDGET: u64 = 64 << 30;
#[cfg(unix)]
const DISK_FLOOR: u64 = 20 << 30;
#[cfg(unix)]
const COORDINATOR: &str = ".alab-package.lockf";
#[cfg(unix)]
const ATTEMPT_ENV: &str = "ALAB_PACKAGE_CACHE_ATTEMPT";

#[cfg(all(test, unix))]
#[path = "build_cache_package_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Declaration {
    cache: PathBuf,
    cache_identity: Identity,
    lock_identity: Identity,
    minimum_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Producer {
    pid: u32,
    birth: u64,
    group: u32,
    boot: String,
    namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Attempt {
    token: String,
    producer: Producer,
    workspace: PathBuf,
    /// Only this attempt began from a newly created empty generation. A later success in
    /// the same generation must not learn failed/warm accumulation as a cold baseline.
    fresh: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Policy {
    declaration: Declaration,
    learned_budget: Option<u64>,
    active: Option<Attempt>,
    last_settled: Option<String>,
    /// A requested compiler key cannot create another materialized generation until this
    /// settled one has passed the existing whole-root retirement transaction.
    #[serde(default)]
    superseded: bool,
}

fn token(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl Policy {
    pub(super) fn valid(&self) -> bool {
        self.declaration.cache.is_absolute()
            && self.declaration.minimum_bytes > 0
            && self
                .learned_budget
                .is_none_or(|bytes| bytes >= self.declaration.minimum_bytes)
            && self.last_settled.as_deref().is_none_or(token)
            && (!self.superseded || self.active.is_none())
            && self.active.as_ref().is_none_or(|attempt| {
                token(&attempt.token)
                    && attempt.workspace.is_absolute()
                    && (2..=i32::MAX as u32).contains(&attempt.producer.pid)
                    && (2..=i32::MAX as u32).contains(&attempt.producer.group)
                    && !attempt.producer.boot.is_empty()
                    && attempt.producer.boot.len() <= 128
                    && attempt
                        .producer
                        .namespace
                        .as_ref()
                        .is_none_or(|text| !text.is_empty() && text.len() <= 128)
            })
    }

    #[cfg(unix)]
    fn budget(&self) -> u64 {
        self.learned_budget
            .unwrap_or(self.declaration.minimum_bytes)
    }
}

pub(super) fn same_declaration(a: Option<&Policy>, b: Option<&Policy>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.declaration == b.declaration,
        _ => false,
    }
}

#[cfg(unix)]
pub(super) fn belongs(receipt: &Receipt, cache: &Path) -> bool {
    receipt
        .package
        .as_ref()
        .is_some_and(|policy| policy.declaration.cache == cache)
}

/// A durable authorization permits a partially removed payload, not a replaced directory
/// or a newly added source project/sibling in its private retirement stage.
pub(super) fn payload_safe(receipt: &Receipt, pending: &Pending) -> bool {
    if receipt.package.is_none() {
        return true;
    }
    let payload = pending.directory.join("cache.noindex");
    let Ok(entries) = std::fs::read_dir(&pending.directory) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else { return false };
        if entry.file_name() != "cache.noindex" {
            return false;
        }
    }
    match std::fs::symlink_metadata(&payload) {
        Err(error) => return pending.authorized && error.kind() == std::io::ErrorKind::NotFound,
        Ok(_) if super::identity(&payload).as_ref() != Some(&receipt.identity) => return false,
        Ok(_) => {}
    }
    if [".git", "Cargo.toml", "Targo.toml"].iter().any(|name| {
        !std::fs::symlink_metadata(payload.join(name))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    }) {
        return false;
    }
    let marker = payload.join(super::MARKER);
    match std::fs::symlink_metadata(&marker) {
        Err(error) => pending.authorized && error.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => super::read(&marker).is_some_and(|marker| {
            marker.identity == receipt.identity
                && marker.nonce == receipt.nonce
                && marker.schema == receipt.schema
                && same_declaration(marker.package.as_ref(), receipt.package.as_ref())
        }),
    }
}

/// Kernel reads have an inert fixture arm; unit tests never inspect the person's workload.
#[cfg(unix)]
pub(super) trait Kernel {
    fn capture(&self, pid: u32, group: u32) -> Option<Producer>;
    fn group_gone(&self, producer: &Producer) -> bool;
    fn retired(&self, producer: &Producer) -> bool;
    fn guardian_live(&self, producer: &Producer) -> bool;
    fn settlement_authority(&self, producer: &Producer) -> bool;
    fn free(&self, path: &Path) -> Option<u64>;
}

#[cfg(unix)]
pub(super) struct Live;

#[cfg(unix)]
pub(super) struct Context<'a> {
    pub cache: Option<&'a Path>,
    pub kernel: &'a dyn Kernel,
}

#[cfg(unix)]
impl Kernel for Live {
    fn capture(&self, pid: u32, group: u32) -> Option<Producer> {
        use aterm_uds::process;
        let identity = process::read_identity(i32::try_from(pid).ok()?)?;
        let own = process::read_lineage(i32::try_from(std::process::id()).ok()?)?;
        if group <= 1 || own.pgid != group {
            return None;
        }
        let namespace = process::pid_namespace();
        if cfg!(target_os = "linux") && namespace.is_none() {
            return None;
        }
        let producer = Producer {
            pid,
            birth: identity.birth.0,
            group,
            boot: process::boot_id()?,
            namespace,
        };
        #[cfg(unix)]
        return under_guard(&producer).then_some(producer);
        #[cfg(not(unix))]
        {
            let _ = producer;
            None
        }
    }

    fn guardian_live(&self, producer: &Producer) -> bool {
        use aterm_uds::process;
        process::boot_id().as_deref() == Some(producer.boot.as_str())
            && process::pid_namespace() == producer.namespace
            && process::read_identity(i32::try_from(producer.pid).unwrap_or(0))
                .is_some_and(|identity| identity.birth.0 == producer.birth)
    }

    fn settlement_authority(&self, producer: &Producer) -> bool {
        #[cfg(unix)]
        {
            self.guardian_live(producer)
                && under_guard(producer)
                && aterm_uds::process::read_lineage(i32::try_from(std::process::id()).unwrap_or(0))
                    .is_some_and(|lineage| lineage.pgid != producer.group)
        }
        #[cfg(not(unix))]
        {
            let _ = producer;
            false
        }
    }

    fn group_gone(&self, producer: &Producer) -> bool {
        use aterm_uds::process;
        let Some(boot) = process::boot_id() else {
            return false;
        };
        if boot != producer.boot {
            return true;
        }
        if process::pid_namespace() != producer.namespace {
            return false;
        }
        process::group_members(producer.group)
            .is_some_and(|members| members.complete && members.pids.is_empty())
    }

    fn retired(&self, producer: &Producer) -> bool {
        use aterm_uds::process;
        let Some(boot) = process::boot_id() else {
            return false;
        };
        if boot != producer.boot {
            return true;
        }
        if process::pid_namespace() != producer.namespace {
            return false;
        }
        let gone = match process::read_identity(i32::try_from(producer.pid).unwrap_or(0)) {
            Some(identity) => identity.birth.0 != producer.birth,
            None => !process::pid_alive(producer.pid),
        };
        gone && self.group_gone(producer)
    }

    fn free(&self, path: &Path) -> Option<u64> {
        crate::platform::volume_free_bytes(path)
    }
}

#[cfg(unix)]
fn file_identity(file: &std::fs::File) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let info = file.metadata().ok()?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let uid = unsafe { libc::geteuid() };
    (info.is_file() && info.uid() == uid && info.mode() & 0o022 == 0 && info.nlink() == 1)
        .then_some(Identity {
            device: info.dev(),
            inode: info.ino(),
        })
}

#[cfg(unix)]
fn coordinator(home: &Path, cache: &Path) -> Option<(Declaration, std::fs::File)> {
    if !super::scope(home, cache) || cache.canonicalize().ok().as_deref() != Some(cache) {
        return None;
    }
    let cache_identity = super::identity(cache)?;
    let path = cache.join(COORDINATOR);
    let file = crate::metadata_io::open_regular(&path).ok()?;
    let lock_identity = file_identity(&file)?;
    let again = crate::metadata_io::open_regular(&path).ok()?;
    if file_identity(&again).as_ref() != Some(&lock_identity) {
        return None;
    }
    Some((
        Declaration {
            cache: cache.to_path_buf(),
            cache_identity,
            lock_identity,
            minimum_bytes: DEFAULT_BUDGET,
        },
        file,
    ))
}

#[cfg(unix)]
fn matches_coordinator(home: &Path, declaration: &Declaration) -> bool {
    coordinator(home, &declaration.cache).is_some_and(|(mut current, _)| {
        current.minimum_bytes = declaration.minimum_bytes;
        current == *declaration
    })
}

#[cfg(unix)]
fn held_coordinator(home: &Path, declaration: &Declaration) -> bool {
    let Some((mut current, file)) = coordinator(home, &declaration.cache) else {
        return false;
    };
    current.minimum_bytes = declaration.minimum_bytes;
    current == *declaration
        && matches!(
            crate::lock::Flock::try_lock(file),
            Err(std::fs::TryLockError::WouldBlock)
        )
}

/// Existing shim publications must belong to the active guarded package attempt too. A
/// generic build outside that group cannot silently add a producer that ignores the policy.
#[cfg(unix)]
pub(super) fn producer_allows(home: &Path, policy: &Policy, workspace: &Path) -> bool {
    let Some(attempt) = &policy.active else {
        return false;
    };
    held_coordinator(home, &policy.declaration)
        && publication_matches(
            attempt,
            workspace,
            std::env::var_os(ATTEMPT_ENV).as_deref(),
            aterm_uds::process::read_lineage(i32::try_from(std::process::id()).unwrap_or(0))
                .map(|lineage| lineage.pgid),
        )
        && Live.guardian_live(&attempt.producer)
        && under_guard(&attempt.producer)
}

#[cfg(unix)]
fn publication_matches(
    attempt: &Attempt,
    workspace: &Path,
    token: Option<&std::ffi::OsStr>,
    group: Option<u32>,
) -> bool {
    attempt.workspace == workspace
        && token.is_some_and(|token| token == attempt.token.as_str())
        && group == Some(attempt.producer.group)
}

#[cfg(unix)]
fn under_guard(producer: &Producer) -> bool {
    let Ok(mut pid) = i32::try_from(std::process::id()) else {
        return false;
    };
    for _ in 0..64 {
        let Some(lineage) = aterm_uds::process::read_lineage(pid) else {
            return false;
        };
        if lineage.identity.pid == i32::try_from(producer.pid).unwrap_or(0) {
            return lineage.identity.birth.0 == producer.birth;
        }
        let Ok(parent) = i32::try_from(lineage.parent) else {
            return false;
        };
        if parent <= 1 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

/// Count exact allocated blocks without following links. An unreadable, foreign-owned or
/// mounted subtree is uncertainty, not a zero-byte baseline or permission to compact it.
#[cfg(unix)]
fn footprint(root: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let root_id = super::identity(root)?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(path) = stack.pop() {
        let info = std::fs::symlink_metadata(&path).ok()?;
        if info.uid() != uid || info.dev() != root_id.device {
            return None;
        }
        if !seen.insert((info.dev(), info.ino())) {
            continue;
        }
        bytes = bytes.checked_add(info.blocks().checked_mul(512)?)?;
        if info.is_dir() {
            for entry in std::fs::read_dir(&path).ok()? {
                stack.push(entry.ok()?.path());
            }
        } else if !info.is_file() && !info.file_type().is_symlink() {
            return None;
        }
    }
    Some(bytes)
}

/// Held before the receipt gate and through every pending-stage removal, including resume.
#[cfg(unix)]
pub(super) struct Coordination {
    declaration: Declaration,
    _held: crate::lock::Flock,
}

#[cfg(not(unix))]
pub(super) struct Coordination;

#[cfg(not(unix))]
pub(super) fn coordinate(_: &Path, receipt: &Receipt) -> Result<Option<Coordination>, ()> {
    if receipt.package.is_some() {
        Err(())
    } else {
        Ok(None)
    }
}

#[cfg(not(unix))]
impl Coordination {
    pub(super) fn current(&self, _: &Path, _: &Receipt) -> bool {
        false
    }
    pub(super) fn reason(&self, _: &Path, _: &Receipt) -> Option<super::super::Unused> {
        None
    }
}

#[cfg(unix)]
pub(super) fn coordinate(home: &Path, receipt: &Receipt) -> Result<Option<Coordination>, ()> {
    let Some(policy) = &receipt.package else {
        return Ok(None);
    };
    let (mut declaration, file) = coordinator(home, &policy.declaration.cache).ok_or(())?;
    declaration.minimum_bytes = policy.declaration.minimum_bytes;
    if declaration != policy.declaration
        || receipt.root.parent() != Some(declaration.cache.as_path())
    {
        return Err(());
    }
    let held = crate::lock::Flock::try_lock(file).map_err(|_| ())?;
    if !matches_coordinator(home, &declaration) {
        return Err(());
    }
    Ok(Some(Coordination {
        declaration,
        _held: held,
    }))
}

#[cfg(unix)]
impl Coordination {
    pub(super) fn current(&self, home: &Path, receipt: &Receipt) -> bool {
        receipt
            .package
            .as_ref()
            .is_some_and(|policy| policy.declaration == self.declaration && policy.active.is_none())
            && !receipt.owners_unknown
            && !receipt.owners.is_empty()
            && matches_coordinator(home, &self.declaration)
            && policies(home, &self.declaration.cache).is_ok_and(|records| {
                records
                    .iter()
                    .all(|(_, other)| other.root == receipt.root || other.pending.is_none())
            })
    }

    pub(super) fn recover_on(
        &self,
        home: &Path,
        path: &Path,
        receipt: &mut Receipt,
        fence: &super::super::fence::Fence<'_>,
        kernel: &dyn Kernel,
    ) -> bool {
        recover_active(home, path, receipt, &self.declaration, fence, kernel)
    }

    pub(super) fn reason(&self, home: &Path, receipt: &Receipt) -> Option<super::super::Unused> {
        self.reason_on(home, receipt, &Live)
    }

    pub(super) fn reason_on(
        &self,
        home: &Path,
        receipt: &Receipt,
        kernel: &dyn Kernel,
    ) -> Option<super::super::Unused> {
        if !self.current(home, receipt) {
            return None;
        }
        let policy = receipt.package.as_ref()?;
        let bytes = footprint(&receipt.root)?;
        if policy.superseded {
            return Some(super::super::Unused::PackageSuperseded);
        }
        let low_disk = kernel.free(&receipt.root)? < DISK_FLOOR;
        (bytes > policy.budget() || low_disk).then_some(super::super::Unused::PackageBudget {
            allocated: bytes,
            budget: policy.budget(),
            low_disk,
        })
    }
}

#[cfg(unix)]
fn recover_active(
    home: &Path,
    path: &Path,
    receipt: &mut Receipt,
    declaration: &Declaration,
    fence: &super::super::fence::Fence<'_>,
    kernel: &dyn Kernel,
) -> bool {
    if !matches_coordinator(home, declaration) || receipt.owners_unknown {
        return false;
    }
    let Some(policy) = &receipt.package else {
        return false;
    };
    if policy.declaration != *declaration {
        return false;
    }
    let Some(attempt) = policy.active.clone() else {
        return true;
    };
    if receipt.pending.is_some()
        || !kernel.retired(&attempt.producer)
        || !super::same(home, receipt)
    {
        return false;
    }
    let Ok(locks) = super::super::locks_under(&receipt.root) else {
        return false;
    };
    let Some(_held) = super::super::hold(&locks) else {
        return false;
    };
    if !fence.clear(&receipt.root, &receipt.root)
        || !matches_coordinator(home, declaration)
        || super::read(path).as_ref() != Some(receipt)
    {
        return false;
    }
    let Some(policy) = receipt.package.as_mut() else {
        return false;
    };
    policy.last_settled = Some(attempt.token);
    policy.active = None; // A crashed cohort never supplies a successful cold baseline.
    super::write(path, receipt).is_some()
}

#[cfg(unix)]
fn receipt_path(home: &Path, target: &Path) -> Option<PathBuf> {
    Some(
        home.join(super::INDEX)
            .join(format!("{}.json", super::key(target)?)),
    )
}

#[cfg(unix)]
fn policies(home: &Path, cache: &Path) -> Result<Vec<(PathBuf, Receipt)>, String> {
    let index = home.join(super::INDEX);
    match std::fs::symlink_metadata(&index) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("package cache index cannot be read".into()),
        Ok(_) => {}
    }
    if super::identity(&index).is_none() || index.canonicalize().ok().as_ref() != Some(&index) {
        return Err("package cache index identity is uncertain".into());
    }
    let mut result = Vec::new();
    let mut count = 0;
    for entry in std::fs::read_dir(index).map_err(|_| "package cache index cannot be listed")? {
        let path = entry
            .map_err(|_| "package cache index entry cannot be read")?
            .path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        count += 1;
        if count > super::MAX_ROOTS {
            return Err("package cache index exceeds its bound".into());
        }
        let receipt =
            super::read(&path).ok_or("package cache index contains an invalid receipt")?;
        if belongs(&receipt, cache) {
            result.push((path, receipt));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

#[cfg(unix)]
struct Admission<'a> {
    cache: &'a Path,
    target: &'a Path,
    manifest: &'a Path,
    budget: u64,
    attempt: &'a str,
    producer_pid: u32,
    producer_group: u32,
}

#[cfg(unix)]
fn legacy(target: &Path, created: bool) -> aterm_json::Value {
    aterm_json::json!({"path":target,"budgeted":false,"created":created,"attempt":null})
}

#[cfg(unix)]
fn admit_on(
    home: &Path,
    args: &Admission<'_>,
    compatible: bool,
    kernel: &dyn Kernel,
    fence: &super::super::fence::Fence<'_>,
) -> Result<aterm_json::Value, String> {
    if !token(args.attempt)
        || args.budget == 0
        || !args.target.is_absolute()
        || args.target.parent() != Some(args.cache)
        || !args.manifest.is_absolute()
        || [home, args.cache].contains(&args.target)
    {
        return Err(
            "package cache admission requires exact absolute paths, budget and attempt".into(),
        );
    }
    let (mut declaration, _) = coordinator(home, args.cache)
        .ok_or("package coordinator is not a private canonical lock")?;
    declaration.minimum_bytes = args.budget;
    if !held_coordinator(home, &declaration) {
        return Err("package coordinator is not held by the producer guardian".into());
    }
    let workspace =
        super::workspace(home, args.manifest).ok_or("package workspace cannot be established")?;
    let path = receipt_path(home, args.target).ok_or("package target cannot be addressed")?;
    let existing = super::read(&path);
    if !compatible {
        if existing
            .as_ref()
            .is_some_and(|receipt| receipt.package.is_some())
        {
            return Err(
                "existing policy cache needs a compatible actual compiler registrar".into(),
            );
        }
        let (_, created) = super::create_with_policy(home, args.target, &workspace, None)
            .map_err(|_| "legacy package cache owner publication refused")?;
        return Ok(legacy(args.target, created));
    }
    let producer = kernel
        .capture(args.producer_pid, args.producer_group)
        .ok_or("package producer birth or worker group cannot be established")?;
    let prior_records = policies(home, args.cache)?;
    if prior_records
        .iter()
        .any(|(_, receipt)| receipt.pending.is_some())
    {
        return Err("an unfinished retirement keeps the package generation".into());
    }
    let mut idempotent = None;
    let mut supersede = Vec::new();
    for (other_path, mut receipt) in prior_records {
        let before = receipt
            .package
            .as_ref()
            .ok_or("package policy disappeared")?;
        if before.declaration != declaration
            || receipt.owners_unknown
            || receipt.owners.is_empty()
            || receipt.root.parent() != Some(args.cache)
            || !super::scope(home, &receipt.root)
        {
            return Err("package policy coordinator or owner publication changed".into());
        }
        if receipt.pending.is_some() {
            return Err("an unfinished retirement keeps the package generation".into());
        }
        if let Some(active) = &before.active
            && active.token == args.attempt
        {
            if receipt.root != args.target
                || active.producer != producer
                || active.workspace != workspace
                || !super::same(home, &receipt)
                || !receipt.owners.contains(&workspace)
            {
                return Err("package attempt was reused for another producer or target".into());
            }
            idempotent = Some(
                aterm_json::json!({"path":args.target,"budgeted":true,"created":active.fresh,"attempt":args.attempt}),
            );
            continue;
        }
        let gate = super::gate_path(&other_path).ok_or("package receipt gate is unavailable")?;
        let file = crate::metadata_io::open_regular(&gate)
            .map_err(|_| "package receipt gate cannot be opened")?;
        let _held =
            crate::lock::Flock::try_lock(file).map_err(|_| "package receipt gate is busy")?;
        if super::read(&other_path).as_ref() != Some(&receipt)
            || !recover_active(home, &other_path, &mut receipt, &declaration, fence, kernel)
        {
            return Err("an unsettled producer still owns the package generation".into());
        }
        match std::fs::symlink_metadata(&receipt.root) {
            Ok(_) if !super::same(home, &receipt) => {
                return Err("a package generation was replaced or contains source work".into());
            }
            Ok(_) if receipt.root != args.target => supersede.push((other_path, receipt)),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err("a package generation cannot be inspected".into());
            }
            _ => {}
        }
    }
    if let Some(result) = idempotent {
        return supersede
            .is_empty()
            .then_some(result)
            .ok_or_else(|| "another materialized package generation must retire first".into());
    }
    if !supersede.is_empty() {
        // The producer already holds the coordinator. Request retirement under each exact
        // receipt gate, but leave removal to the outside compactor after this guard returns.
        for (other_path, mut receipt) in supersede {
            let gate = super::gate_path(&other_path).ok_or("package receipt gate unavailable")?;
            let file = crate::metadata_io::open_regular(&gate)
                .map_err(|_| "package receipt gate cannot be opened")?;
            let _held =
                crate::lock::Flock::try_lock(file).map_err(|_| "package receipt gate is busy")?;
            if super::read(&other_path).as_ref() != Some(&receipt)
                || !super::same(home, &receipt)
                || !held_coordinator(home, &declaration)
            {
                return Err("package generation changed before retirement request".into());
            }
            let policy = receipt
                .package
                .as_mut()
                .ok_or("package policy disappeared")?;
            if policy.active.is_some() || receipt.pending.is_some() {
                return Err("package generation remains unsettled".into());
            }
            policy.superseded = true;
            super::write(&other_path, &receipt).ok_or("package retirement request failed")?;
        }
        return Err(
            "a previous package compiler generation must retire before the next poll".into(),
        );
    }
    let prior = super::read(&path).and_then(|receipt| receipt.package);
    if prior.as_ref().is_some_and(|policy| policy.superseded)
        && std::fs::symlink_metadata(args.target).is_ok()
    {
        return Err("the superseded package generation must retire first".into());
    }
    if prior
        .as_ref()
        .is_some_and(|policy| policy.last_settled.as_deref() == Some(args.attempt))
    {
        return Err("package attempt already settled".into());
    }
    let policy = Policy {
        declaration: declaration.clone(),
        learned_budget: prior.as_ref().and_then(|policy| policy.learned_budget),
        last_settled: prior
            .as_ref()
            .and_then(|policy| policy.last_settled.clone()),
        active: Some(Attempt {
            token: args.attempt.into(),
            producer,
            workspace: workspace.clone(),
            fresh: false,
        }),
        superseded: false,
    };
    let (recorded, created) =
        super::create_with_policy(home, args.target, &workspace, Some(policy.clone()))
            .map_err(|_| "package cache owner publication refused")?;
    if !recorded {
        return Ok(legacy(args.target, false));
    }
    let file = crate::metadata_io::open_regular(
        &super::gate_path(&path).ok_or("package receipt gate is unavailable")?,
    )
    .map_err(|_| "package receipt gate cannot be opened")?;
    let _held = crate::lock::Flock::try_lock(file).map_err(|_| "package receipt gate is busy")?;
    let mut receipt = super::read(&path).ok_or("package receipt cannot be read")?;
    let Some(current) = &receipt.package else {
        return Ok(legacy(args.target, created));
    };
    if current.declaration != declaration
        || current
            .active
            .as_ref()
            .is_some_and(|attempt| attempt.token != args.attempt)
        || receipt.pending.is_some()
        || receipt.owners_unknown
        || !receipt.owners.contains(&workspace)
        || !super::same(home, &receipt)
        || !held_coordinator(home, &declaration)
    {
        return Err("package cache changed during owner publication".into());
    }
    let mut policy = policy;
    if let Some(active) = policy.active.as_mut() {
        active.fresh = created;
    }
    receipt.package = Some(policy);
    super::write(&path, &receipt).ok_or("package attempt publication failed")?;
    Ok(
        aterm_json::json!({"path":args.target,"budgeted":true,"created":created,"attempt":args.attempt}),
    )
}

#[cfg(unix)]
fn settled_on(
    home: &Path,
    cache: &Path,
    attempt: &str,
    success: bool,
    kernel: &dyn Kernel,
    fence: &super::super::fence::Fence<'_>,
) -> Result<aterm_json::Value, String> {
    if !token(attempt) {
        return Err("package settlement needs an exact attempt".into());
    }
    let found = policies(home, cache)?;
    if found.is_empty() {
        return Ok(aterm_json::json!({"status":"legacy","budgeted":false}));
    }
    for (path, mut receipt) in found {
        let policy = receipt
            .package
            .as_ref()
            .ok_or("package policy disappeared")?;
        if !held_coordinator(home, &policy.declaration) {
            return Err("package settlement must remain inside its producer guardian".into());
        }
        let Some(active) = policy.active.clone() else {
            if policy.last_settled.as_deref() == Some(attempt) {
                return Ok(
                    aterm_json::json!({"status":"settled","budgeted":true,"attempt":attempt,"budget_bytes":policy.budget()}),
                );
            }
            continue;
        };
        if active.token != attempt {
            continue;
        }
        let file = crate::metadata_io::open_regular(
            &super::gate_path(&path).ok_or("package receipt gate unavailable")?,
        )
        .map_err(|_| "package receipt gate cannot be read")?;
        let _held = crate::lock::Flock::try_lock(file).map_err(|_| "package receipt gate busy")?;
        if super::read(&path).as_ref() != Some(&receipt)
            || receipt.pending.is_some()
            || receipt.owners_unknown
            || !super::same(home, &receipt)
            || !kernel.settlement_authority(&active.producer)
            || !kernel.group_gone(&active.producer)
        {
            return Err("package child retirement or source ownership remains unsettled".into());
        }
        let locks = super::super::locks_under(&receipt.root)
            .map_err(|_| "package Cargo locks cannot be listed")?;
        let _cargo = super::super::hold(&locks).ok_or("package Cargo locks remain held")?;
        if !fence.clear(&receipt.root, &receipt.root)
            || !held_coordinator(home, &policy.declaration)
            || super::read(&path).as_ref() != Some(&receipt)
        {
            return Err("package target remains in use or was replaced".into());
        }
        let measured = (success && active.fresh && policy.learned_budget.is_none())
            .then(|| footprint(&receipt.root))
            .flatten();
        let policy = receipt
            .package
            .as_mut()
            .ok_or("package policy disappeared")?;
        if let Some(bytes) = measured {
            policy.learned_budget = Some(
                policy
                    .declaration
                    .minimum_bytes
                    .max(bytes.saturating_mul(2)),
            );
        }
        policy.active = None;
        policy.last_settled = Some(attempt.into());
        let budget = policy.budget();
        super::write(&path, &receipt).ok_or("package settlement publication failed")?;
        return Ok(
            aterm_json::json!({"status":"settled","budgeted":true,"attempt":attempt,"budget_bytes":budget,"learned":measured.is_some()}),
        );
    }
    Err("package settlement names no current attempt".into())
}

#[cfg(unix)]
fn executable_stamp(path: &Path) -> Option<[u64; 6]> {
    use std::os::unix::fs::MetadataExt as _;
    let file = crate::metadata_io::open_regular(path).ok()?;
    let info = file.metadata().ok()?;
    (info.mode() & 0o111 != 0 && info.mode() & 0o022 == 0).then_some([
        info.dev(),
        info.ino(),
        info.len(),
        info.mtime() as u64,
        info.mtime_nsec() as u64,
        info.ctime() as u64,
    ])
}

#[cfg(unix)]
fn capability(path: &Path, pkg: bool) -> bool {
    let Ok(physical) = path.canonicalize() else {
        return false;
    };
    let Some(before) = executable_stamp(&physical) else {
        return false;
    };
    let mut command = aterm_uds::spawnfd::command(path);
    command.setsid(true);
    if pkg {
        command.arg("pkg");
    }
    command.args([VERB, "--capabilities"]);
    let output =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(10));
    output.is_ok_and(|output| {
        output.status.success()
            && output.stdout == format!("{CAPABILITY}\n").as_bytes()
            && output.stderr.is_empty()
    }) && path.canonicalize().ok().as_ref() == Some(&physical)
        && executable_stamp(&physical).as_ref() == Some(&before)
}

/// A text shim's stamped registrar always wins, even when the API host is newer. Raw
/// admitted Trust drivers have no callback; only their explicit, same-image API host may
/// provide pre-registration. Ambiguity keeps a fresh target on the legacy v1 flow.
#[cfg(unix)]
fn compatible(home: &Path, driver: &Path, registrar: Option<&Path>) -> bool {
    use std::io::Read as _;
    if !driver.is_absolute() {
        return false;
    }
    let Ok(driver) = driver.canonicalize() else {
        return false;
    };
    let Some(before) = executable_stamp(&driver) else {
        return false;
    };
    let Ok(file) = crate::metadata_io::open_regular(&driver) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take((64 << 10) + 1).read_to_end(&mut head).is_err() {
        return false;
    }
    let raw = head.starts_with(b"\x7fELF")
        || [
            b"\xcf\xfa\xed\xfe",
            b"\xfe\xed\xfa\xcf",
            b"\xca\xfe\xba\xbe",
        ]
        .iter()
        .any(|magic| head.starts_with(*magic));
    let supported = if raw {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let discovered = crate::toolchain::Toolchain::discover(None, home, &path, Some("trust"));
        let Some(registrar) = registrar.filter(|path| path.is_absolute()) else {
            return false;
        };
        let Ok(registrar) = registrar.canonicalize() else {
            return false;
        };
        let Ok(current) = std::env::current_exe().and_then(std::fs::canonicalize) else {
            return false;
        };
        // The same shipped image serves both `aterm pkg` and its `atpkg` alias. Probe
        // this process's proven working entry, preserving argv0 dispatch after binding
        // its physical identity to the explicit registrar and current executable.
        let Some(entry) = std::env::args_os()
            .next()
            .map(PathBuf::from)
            .filter(|entry| entry.is_absolute())
        else {
            return false;
        };
        let Ok(physical_entry) = entry.canonicalize() else {
            return false;
        };
        let Some(stamp) = executable_stamp(&current) else {
            return false;
        };
        discovered.refused.is_none()
            && discovered.targo.canonicalize().ok().as_ref() == Some(&driver)
            && executable_stamp(&registrar).as_ref() == Some(&stamp)
            && executable_stamp(&physical_entry).as_ref() == Some(&stamp)
            && capability(
                &entry,
                std::env::args_os().nth(1).is_some_and(|word| word == "pkg"),
            )
            && executable_stamp(&registrar).as_ref() == Some(&stamp)
    } else {
        let Ok(text) = std::str::from_utf8(&head) else {
            return false;
        };
        head.len() <= 64 << 10
            && text
                .lines()
                .filter(|line| line.starts_with("# atpkg cache helper "))
                .count()
                == 1
            && super::shim_helper(text).is_some_and(|helper| capability(&helper, false))
    };
    supported && executable_stamp(&driver).as_ref() == Some(&before)
}

#[cfg(unix)]
fn compact(
    home: &Path,
    cache: &Path,
    fence: &super::super::fence::Fence<'_>,
) -> Result<aterm_json::Value, String> {
    let before = policies(home, cache)?;
    if before.is_empty() {
        return Ok(aterm_json::json!({"status":"legacy","budgeted":false}));
    }
    let removed = super::reclaim_selected_on(
        home,
        std::time::SystemTime::now(),
        super::super::RECLAIM_GONE,
        None,
        &mut |_, _| true,
        fence,
        Some(Context {
            cache: Some(cache),
            kernel: &Live,
        }),
    );
    let after = policies(home, cache)?;
    let pending = after
        .iter()
        .filter(|(_, receipt)| receipt.pending.is_some())
        .count();
    let unsettled = after.iter().any(|(_, receipt)| {
        receipt
            .package
            .as_ref()
            .is_some_and(|policy| policy.active.is_some())
    });
    let busy = after.iter().any(|(_, receipt)| {
        coordinate(home, receipt).is_err()
            || receipt.package.as_ref().is_some_and(|policy| {
                !matches_coordinator(home, &policy.declaration)
                    || (!std::fs::symlink_metadata(&receipt.root)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                        && (!super::same(home, receipt)
                            || policy.superseded
                            || footprint(&receipt.root)
                                .zip(Live.free(&receipt.root))
                                .is_none_or(|(bytes, free)| {
                                    bytes > policy.budget() || free < DISK_FLOOR
                                })))
            })
    });
    let status = if !removed.is_empty() && pending == 0 {
        "retired"
    } else if pending > 0 || unsettled || busy {
        "deferred"
    } else {
        "ready"
    };
    Ok(
        aterm_json::json!({"status":status,"budgeted":true,"pending":pending,"removed":removed.len()}),
    )
}

#[cfg(unix)]
fn invoke(args: &[std::ffi::OsString]) -> Result<aterm_json::Value, String> {
    let Some(verb) = args.first().and_then(|word| word.to_str()) else {
        return Err("package cache verb missing".into());
    };
    let mut values = std::collections::BTreeMap::new();
    let mut success = false;
    let mut words = args[1..].iter();
    while let Some(key) = words.next() {
        let key = key.to_str().ok_or("package cache option must be UTF-8")?;
        if key == "--success" && verb == "settled" && !success {
            success = true;
            continue;
        }
        let allowed = match verb {
            "admit" => matches!(
                key,
                "--cache"
                    | "--target-dir"
                    | "--manifest-path"
                    | "--budget-bytes"
                    | "--attempt"
                    | "--producer-pid"
                    | "--producer-group"
                    | "--compiler-driver"
                    | "--registrar"
            ),
            "settled" => matches!(key, "--cache" | "--attempt"),
            "compact" => key == "--cache",
            _ => false,
        };
        if !allowed || values.contains_key(key) {
            return Err("unknown or duplicate package cache option".into());
        }
        values.insert(
            key,
            words.next().ok_or("package cache option value missing")?,
        );
    }
    let value = |key| {
        values
            .get(key)
            .copied()
            .ok_or("required package cache option missing")
    };
    let path = |key| value(key).map(PathBuf::from);
    let text = |key| {
        value(key)?
            .to_str()
            .ok_or("package cache value must be UTF-8")
    };
    let cache = path("--cache")?;
    let home = aterm_types::dirs::home_dir().ok_or("package cache needs a home")?;
    if !cache.is_absolute()
        || !super::scope(&home, &cache)
        || cache.canonicalize().ok().as_ref() != Some(&cache)
    {
        return Err("package cache needs a canonical private cache below home".into());
    }
    let fence = super::super::fence::Fence::new(&super::super::fence::live);
    match verb {
        "compact" => Ok(compact(&home, &cache, &fence)
            .unwrap_or_else(|reason| aterm_json::json!({"status":"deferred","reason":reason}))),
        "settled" => settled_on(&home, &cache, text("--attempt")?, success, &Live, &fence),
        "admit" => {
            let target = path("--target-dir")?;
            let manifest = path("--manifest-path")?;
            let driver = path("--compiler-driver")?;
            let registrar = values.get("--registrar").map(|value| PathBuf::from(*value));
            let args = Admission {
                cache: &cache,
                target: &target,
                manifest: &manifest,
                budget: text("--budget-bytes")?
                    .parse()
                    .map_err(|_| "package budget is invalid")?,
                attempt: text("--attempt")?,
                producer_pid: text("--producer-pid")?
                    .parse()
                    .map_err(|_| "package producer PID is invalid")?,
                producer_group: text("--producer-group")?
                    .parse()
                    .map_err(|_| "package producer group is invalid")?,
            };
            admit_on(
                &home,
                &args,
                compatible(&home, &driver, registrar.as_deref()),
                &Live,
                &fence,
            )
        }
        _ => Err("unknown package cache verb".into()),
    }
}

/// Internal producer protocol. This never runs global machine maintenance or chooses a
/// different target. Busy compaction preserves the cache and returns a small deferred reply.
pub fn run_hidden(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    #[cfg(unix)]
    {
        if args.len() == 1 && args[0] == "--capabilities" {
            println!("{CAPABILITY}");
            return std::process::ExitCode::SUCCESS;
        }
        match invoke(args) {
            Ok(value) => {
                if let Ok(text) = aterm_json::to_string(&value) {
                    println!("{text}");
                    return std::process::ExitCode::SUCCESS;
                }
            }
            Err(error) => eprintln!("atpkg: package cache: {error}"),
        }
        std::process::ExitCode::from(75)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        std::process::ExitCode::from(2)
    }
}

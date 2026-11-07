// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{Limits, Pack, Pair, PairState, Plan};
use aterm_dirfd::{AtFlags, Dir, FileType, Mode, OFlags, openat};
use ring::digest::{Context, SHA256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::time::Instant;

const DIRECTORY: OFlags = OFlags::from_bits(
    OFlags::RDONLY.bits()
        | OFlags::DIRECTORY.bits()
        | OFlags::NOFOLLOW.bits()
        | OFlags::NONBLOCK.bits()
        | OFlags::CLOEXEC.bits(),
);
const REGULAR: OFlags = OFlags::from_bits(
    OFlags::RDONLY.bits()
        | OFlags::NOFOLLOW.bits()
        | OFlags::NONBLOCK.bits()
        | OFlags::CLOEXEC.bits(),
);

fn refusal(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

/// Read-only identity observation, including type/permissions and mutation times.
/// No atime: reading a file may legitimately update it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    bytes: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            links: meta.nlink(),
            uid: meta.uid(),
            gid: meta.gid(),
            bytes: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

struct Directory {
    path: String,
    stamp: Stamp,
    names: Vec<String>,
}

struct ObservedPack {
    row: Pack,
    stamp: Stamp,
}

struct Census<'a> {
    root: File,
    root_stamp: Stamp,
    root_path: &'a Path,
    limits: &'a Limits,
    start: Instant,
    entries: usize,
    directories: Vec<Directory>,
    packs: Vec<ObservedPack>,
}

/// Internal deterministic mutation points; public production entry supplies no hook.
#[derive(Clone, Copy)]
enum Point {
    ReadChunk,
    BeforeFinal,
}

fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && !name.contains('/')
        && !name.chars().any(char::is_control)
        && !name.contains('\u{fffd}')
}

fn check_elapsed(start: Instant, limits: &Limits) -> io::Result<()> {
    if start.elapsed() >= limits.max_elapsed {
        Err(refusal("cooperative elapsed-work limit reached"))
    } else {
        Ok(())
    }
}

fn check_directory_depth(path: &str, limits: &Limits) -> io::Result<()> {
    if path.split('/').filter(|p| !p.is_empty()).count() > limits.max_depth {
        Err(refusal("directory depth limit reached"))
    } else {
        Ok(())
    }
}

fn absolute_directory(path: &Path, start: Instant, limits: &Limits) -> io::Result<File> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(refusal(
            "git common directory must be an absolute UTF-8 path",
        ));
    }
    // Admission has a fixed lexical cap before opening its first component;
    // relative store traversal has the separate reported max_depth.
    if path.to_str().unwrap().len() > crate::MAX_ROOT_PATH_BYTES
        || path.components().count() > crate::MAX_ROOT_COMPONENTS
    {
        return Err(refusal(
            "common-directory root path/component limit reached",
        ));
    }
    check_elapsed(start, limits)?;
    let mut directory = File::from(openat(aterm_dirfd::CWD, "/", DIRECTORY, Mode::empty())?);
    for component in path.components() {
        check_elapsed(start, limits)?;
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .filter(|n| valid_component(n))
                    .ok_or_else(|| refusal("invalid common-directory component"))?;
                directory = File::from(openat(&directory, name, DIRECTORY, Mode::empty())?);
            }
            _ => {
                return Err(refusal(
                    "common directory must not contain parent or prefix components",
                ));
            }
        }
    }
    Ok(directory)
}

impl Census<'_> {
    fn check_time(&self) -> io::Result<()> {
        check_elapsed(self.start, self.limits)
    }

    fn open(&self, relative: &str, flags: OFlags) -> io::Result<File> {
        self.check_time()?;
        // A regular leaf does not add a directory level. Admission applies to
        // the complete directory chain before reopening any component.
        let directory_path = if flags.bits() & OFlags::DIRECTORY.bits() != 0 {
            relative
        } else {
            relative.rsplit_once('/').map_or("", |(parent, _)| parent)
        };
        check_directory_depth(directory_path, self.limits)?;
        if relative.is_empty() {
            return self.root.try_clone();
        }
        let mut parent = self.root.try_clone()?;
        let mut names = relative.split('/').peekable();
        while let Some(name) = names.next() {
            self.check_time()?;
            if !valid_component(name) {
                return Err(refusal("invalid recorded path component"));
            }
            parent = File::from(openat(
                &parent,
                name,
                if names.peek().is_some() {
                    DIRECTORY
                } else {
                    flags
                },
                Mode::empty(),
            )?);
        }
        Ok(parent)
    }

    fn child_directory(&self, parent: &File, parent_path: &str, name: &str) -> io::Result<File> {
        check_directory_depth(&join(parent_path, name), self.limits)?;
        self.check_time()?;
        Ok(File::from(openat(parent, name, DIRECTORY, Mode::empty())?))
    }

    fn optional_directory(
        &self,
        parent: &File,
        parent_path: &str,
        name: &str,
    ) -> io::Result<Option<File>> {
        match self.child_directory(parent, parent_path, name) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn names(&mut self, directory: &File, final_max: Option<usize>) -> io::Result<Vec<String>> {
        self.check_time()?;
        let mut names = Vec::new();
        for entry in Dir::read_from(directory)? {
            self.check_time()?;
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .ok()
                .filter(|n| valid_component(n))
                .ok_or_else(|| refusal("unrepresentable or unsafe directory entry"))?;
            if let Some(max) = final_max {
                if names.len() >= max {
                    return Err(refusal("directory membership changed"));
                }
            } else {
                if self.entries >= self.limits.max_entries {
                    return Err(refusal("directory-entry limit reached"));
                }
                self.entries += 1;
            }
            names.push(name.to_owned());
        }
        names.sort_unstable();
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(refusal("duplicate directory entry during observation"));
        }
        Ok(names)
    }

    fn observe_directory(&mut self, directory: &File, path: &str) -> io::Result<Vec<String>> {
        check_directory_depth(path, self.limits)?;
        let before = Stamp::of(&directory.metadata()?);
        let names = self.names(directory, None)?;
        if before != Stamp::of(&directory.metadata()?) {
            return Err(refusal(format!("directory changed during listing: {path}")));
        }
        self.directories.push(Directory {
            path: path.into(),
            stamp: before,
            names: names.clone(),
        });
        Ok(names)
    }

    fn collect_pack_directory(&mut self, objects: &File, objects_path: &str) -> io::Result<()> {
        // Record optional pack membership through its parent too, including absence.
        self.observe_directory(objects, objects_path)?;
        let Some(directory) = self.optional_directory(objects, objects_path, "pack")? else {
            return Ok(());
        };
        let path = join(objects_path, "pack");
        let names = self.observe_directory(&directory, &path)?;
        for name in names {
            self.check_time()?;
            if !name.ends_with(".pack") {
                continue;
            }
            if !pack_name(&name) {
                return Err(refusal(format!("invalid pack filename: {path}/{name}")));
            }
            if self.packs.len() >= self.limits.max_packs {
                return Err(refusal("pack-count limit reached"));
            }
            let file = File::from(openat(&directory, &name, REGULAR, Mode::empty())?);
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(refusal(format!(
                    "pack is not a regular file: {path}/{name}"
                )));
            }
            if meta.len() > self.limits.max_pack_bytes {
                return Err(refusal(format!("pack-size limit reached: {path}/{name}")));
            }
            let stamp = Stamp::of(&meta);
            // Compare named identity with the opened handle without following a link.
            let named = aterm_dirfd::fstatat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
            if named.st_dev != stamp.dev || named.st_ino != stamp.ino || named.st_mode != stamp.mode
            {
                return Err(refusal("pack path changed while opening"));
            }
            self.packs.push(ObservedPack {
                row: Pack {
                    relative_path: join(&path, &name),
                    bytes: stamp.bytes,
                    device: stamp.dev,
                    inode: stamp.ino,
                    links: stamp.links,
                },
                stamp,
            });
        }
        Ok(())
    }

    fn modules(&mut self, directory: &File, path: &str) -> io::Result<()> {
        let names = self.observe_directory(directory, path)?;
        // A Git store is recognized structurally, not declared valid Git data.
        // Once inside one, only its objects and nested modules are traversed.
        if names.iter().any(|n| n == "objects") && names.iter().any(|n| n == "HEAD") {
            let objects = self.child_directory(directory, path, "objects")?;
            self.collect_pack_directory(&objects, &join(path, "objects"))?;
            if let Some(nested) = self.optional_directory(directory, path, "modules")? {
                self.modules(&nested, &join(path, "modules"))?;
            }
            return Ok(());
        }
        for name in names {
            self.check_time()?;
            let stat = aterm_dirfd::fstatat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind.is_symlink() {
                return Err(refusal(format!(
                    "module namespace contains a symlink: {path}/{name}"
                )));
            }
            if kind.is_dir() {
                let child = self.child_directory(directory, path, &name)?;
                self.modules(&child, &join(path, &name))?;
            } else if !kind.is_file() {
                return Err(refusal(format!(
                    "module namespace contains a special file: {path}/{name}"
                )));
            }
        }
        Ok(())
    }

    fn scan(&mut self) -> io::Result<()> {
        let root = self.root.try_clone()?;
        self.observe_directory(&root, "")?;
        let objects = self.child_directory(&root, "", "objects")?;
        self.collect_pack_directory(&objects, "objects")?;
        if let Some(modules) = self.optional_directory(&root, "", "modules")? {
            self.modules(&modules, "modules")?;
        }
        if let Some(worktrees) = self.optional_directory(&root, "", "worktrees")? {
            let names = self.observe_directory(&worktrees, "worktrees")?;
            for name in names {
                self.check_time()?;
                let worktree = self.child_directory(&worktrees, "worktrees", &name)?;
                let path = join("worktrees", &name);
                self.observe_directory(&worktree, &path)?;
                if let Some(modules) = self.optional_directory(&worktree, &path, "modules")? {
                    self.modules(&modules, &join(&path, "modules"))?;
                }
            }
        }
        Ok(())
    }

    fn current_pack(&self, pack: &ObservedPack) -> io::Result<File> {
        let file = self.open(&pack.row.relative_path, REGULAR)?;
        if !file.metadata()?.is_file() || Stamp::of(&file.metadata()?) != pack.stamp {
            return Err(refusal(format!(
                "pack identity or metadata changed: {}",
                pack.row.relative_path
            )));
        }
        Ok(file)
    }

    fn final_check(&mut self) -> io::Result<()> {
        self.check_time()?;
        if self.root_stamp
            != Stamp::of(&absolute_directory(self.root_path, self.start, self.limits)?.metadata()?)
            || self.root_stamp != Stamp::of(&self.root.metadata()?)
        {
            return Err(refusal(
                "advertised common-directory identity or metadata changed",
            ));
        }
        for index in 0..self.directories.len() {
            let directory = self.open(&self.directories[index].path, DIRECTORY)?;
            let before = Stamp::of(&directory.metadata()?);
            if before != self.directories[index].stamp {
                return Err(refusal(format!(
                    "directory metadata changed: {}",
                    self.directories[index].path
                )));
            }
            let names = self.names(&directory, Some(self.directories[index].names.len() + 1))?;
            if names != self.directories[index].names || before != Stamp::of(&directory.metadata()?)
            {
                return Err(refusal(format!(
                    "directory membership changed: {}",
                    self.directories[index].path
                )));
            }
        }
        for pack in &self.packs {
            self.current_pack(pack)?;
        }
        Ok(())
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

fn pack_name(name: &str) -> bool {
    name.strip_prefix("pack-")
        .and_then(|n| n.strip_suffix(".pack"))
        .is_some_and(|hash| {
            matches!(hash.len(), 40 | 64)
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn read_counted(
    census: &Census<'_>,
    file: &mut File,
    mut buffer: &mut [u8],
    io_bytes: &mut u64,
) -> io::Result<()> {
    while !buffer.is_empty() {
        census.check_time()?;
        let count = file.read(buffer)?;
        *io_bytes += count as u64;
        if count == 0 {
            return Err(refusal("pack shrank during comparison"));
        }
        buffer = &mut buffer[count..];
    }
    Ok(())
}

fn verify(
    census: &Census<'_>,
    left: usize,
    right: usize,
    io_bytes: &mut u64,
    hook: &mut impl FnMut(Point),
) -> io::Result<(bool, String)> {
    let a = &census.packs[left];
    let b = &census.packs[right];
    let mut left_file = census.current_pack(a)?;
    let mut right_file = census.current_pack(b)?;
    let mut left_hash = Context::new(&SHA256);
    let mut right_hash = Context::new(&SHA256);
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    let mut remaining = a.stamp.bytes;
    let mut equal = true;
    while remaining != 0 {
        census.check_time()?;
        let count = usize::try_from(remaining.min(left_buffer.len() as u64)).unwrap();
        // The whole pair plus lookaheads was reserved before this function.
        // A read cannot exceed its slice, even if the file grows. Count partial
        // successful reads too, so an eventual error cannot hide consumed I/O.
        read_counted(census, &mut left_file, &mut left_buffer[..count], io_bytes)?;
        read_counted(
            census,
            &mut right_file,
            &mut right_buffer[..count],
            io_bytes,
        )?;
        equal &= left_buffer[..count] == right_buffer[..count];
        left_hash.update(&left_buffer[..count]);
        right_hash.update(&right_buffer[..count]);
        remaining -= count as u64;
        hook(Point::ReadChunk);
    }
    let mut extra = [0u8; 1];
    census.check_time()?;
    let left_extra = left_file.read(&mut extra)?;
    *io_bytes += left_extra as u64;
    census.check_time()?;
    let right_extra = right_file.read(&mut extra)?;
    *io_bytes += right_extra as u64;
    if left_extra != 0 || right_extra != 0 {
        return Err(refusal("pack grew during comparison"));
    }
    if Stamp::of(&left_file.metadata()?) != a.stamp || Stamp::of(&right_file.metadata()?) != b.stamp
    {
        return Err(refusal("pack changed during comparison"));
    }
    census.current_pack(a)?;
    census.current_pack(b)?;
    let left_digest = left_hash.finish();
    let right_digest = right_hash.finish();
    if equal && left_digest.as_ref() != right_digest.as_ref() {
        return Err(refusal("exact comparison and digest disagree"));
    }
    let digest = left_digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((equal, digest))
}

pub(super) fn plan(path: &Path, limits: &Limits) -> io::Result<Plan> {
    plan_with_hook(path, limits, &mut |_| {})
}

#[derive(Default)]
struct PackGroup {
    inodes: BTreeSet<usize>,
    paths: Vec<usize>,
}

/// Count independent inode connections, not path edges. Aliases can connect
/// overlapping filename groups; parallel edges and cycles add no duplicate bytes.
struct InodeForest {
    parents: Vec<usize>,
    ranks: Vec<usize>,
}

impl InodeForest {
    fn new(nodes: usize) -> Self {
        Self {
            parents: (0..nodes).collect(),
            ranks: vec![0; nodes],
        }
    }

    fn root(&mut self, mut node: usize) -> usize {
        while self.parents[node] != node {
            self.parents[node] = self.parents[self.parents[node]];
            node = self.parents[node];
        }
        node
    }

    fn connect(&mut self, left: usize, right: usize) -> bool {
        let mut left = self.root(left);
        let mut right = self.root(right);
        if left == right {
            return false;
        }
        if self.ranks[left] < self.ranks[right] {
            std::mem::swap(&mut left, &mut right);
        }
        self.parents[right] = left;
        if self.ranks[left] == self.ranks[right] {
            self.ranks[left] += 1;
        }
        true
    }
}

fn plan_with_hook(path: &Path, limits: &Limits, hook: &mut impl FnMut(Point)) -> io::Result<Plan> {
    let start = Instant::now();
    let root = absolute_directory(path, start, limits)?;
    check_elapsed(start, limits)?;
    let head = File::from(openat(&root, "HEAD", REGULAR, Mode::empty())?);
    if !head.metadata()?.is_file() || head.metadata()?.len() > 64 * 1024 {
        return Err(refusal(
            "common-directory HEAD must be a bounded regular file",
        ));
    }
    // Missing primary objects refuses root admission rather than an empty census.
    check_directory_depth("objects", limits)?;
    check_elapsed(start, limits)?;
    openat(&root, "objects", DIRECTORY, Mode::empty())?;
    check_directory_depth("refs", limits)?;
    check_elapsed(start, limits)?;
    openat(&root, "refs", DIRECTORY, Mode::empty())?;
    let root_stamp = Stamp::of(&root.metadata()?);
    let mut census = Census {
        root,
        root_stamp,
        root_path: path,
        limits,
        start,
        entries: 0,
        directories: Vec::new(),
        packs: Vec::new(),
    };
    let mut report = Plan {
        schema: "aterm.repo-storage-plan/v1",
        git_common_dir: path.to_str().unwrap().into(),
        observation_complete: true,
        limits: limits.into(),
        observation_scope: "Sequential read-only metadata and optional byte observations, with final path/membership rechecks; no atomic snapshot or writer exclusion. Elapsed limit cannot preempt filesystem calls.",
        mutation_available: false,
        mutation_reason: "writer custody and durable replacement/recovery are not implemented; equal bytes never authorize mutation",
        physical_savings_bytes: None,
        entries_observed: 0,
        unique_pack_inodes: 0,
        hardlink_alias_paths: 0,
        unique_logical_bytes: 0,
        candidate_duplicate_logical_bytes: 0,
        byte_equal_duplicate_logical_bytes: 0,
        verification_io_bytes: 0,
        packs: Vec::new(),
        pairs: Vec::new(),
        issues: Vec::new(),
    };
    if let Err(error) = census.scan() {
        report.observation_complete = false;
        report.issues.push(error.to_string());
    }
    let mut inodes = BTreeMap::new();
    let mut pack_inodes = Vec::with_capacity(census.packs.len());
    let mut groups: BTreeMap<(String, u64), PackGroup> = BTreeMap::new();
    for (index, pack) in census.packs.iter().enumerate() {
        let inode = *inodes
            .entry((pack.stamp.dev, pack.stamp.ino))
            .or_insert(index);
        pack_inodes.push(inode);
        if inode == index {
            report.unique_logical_bytes =
                report
                    .unique_logical_bytes
                    .checked_add(pack.row.bytes)
                    .ok_or_else(|| refusal("logical byte accounting overflow"))?;
        } else {
            report.hardlink_alias_paths += 1;
        }
        // Every basename gets its own inode membership. The global first path
        // may have a different basename and cannot replace this group's witness.
        let group = groups
            .entry((
                pack.row.relative_path.rsplit('/').next().unwrap().into(),
                pack.row.bytes,
            ))
            .or_default();
        if group.inodes.insert(inode) {
            group.paths.push(index);
        }
    }
    report.unique_pack_inodes = inodes.len();
    let mut candidates = InodeForest::new(census.packs.len());
    let mut pair_inodes = Vec::new();
    for group in groups.values() {
        let source = group.paths[0];
        for &target in &group.paths[1..] {
            let bytes = census.packs[target].row.bytes;
            let edge = (pack_inodes[source], pack_inodes[target]);
            if candidates.connect(edge.0, edge.1) {
                report.candidate_duplicate_logical_bytes = report
                    .candidate_duplicate_logical_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| refusal("candidate byte accounting overflow"))?;
            }
            let mut pair = Pair {
                source: census.packs[source].row.relative_path.clone(),
                target: census.packs[target].row.relative_path.clone(),
                bytes,
                state: PairState::Candidate,
                reason: "filename and length only; no content read".into(),
                sha256: None,
            };
            // Reserve both complete streams and one lookahead byte each before
            // reading. Actual successful read bytes, not attempted EOF, are reported.
            let required = bytes.checked_add(1).and_then(|b| b.checked_mul(2));
            if limits.verify_io_bytes != 0 {
                if required.is_none_or(|need| {
                    need > limits
                        .verify_io_bytes
                        .saturating_sub(report.verification_io_bytes)
                }) {
                    pair.state = PairState::Deferred;
                    pair.reason =
                        "paired comparison exceeds remaining verification I/O budget".into();
                } else {
                    match verify(
                        &census,
                        source,
                        target,
                        &mut report.verification_io_bytes,
                        hook,
                    ) {
                        Ok((true, digest)) => {
                            pair.state = PairState::ByteEqual;
                            pair.reason = "complete exact comparison and matching streamed SHA-256; advisory only".into();
                            pair.sha256 = Some(digest);
                        }
                        Ok((false, _)) => {
                            pair.state = PairState::ByteMismatch;
                            pair.reason = "same filename and length have different bytes".into();
                        }
                        Err(error) => {
                            pair.state = PairState::Refused;
                            pair.reason = error.to_string();
                            report.observation_complete = false;
                        }
                    }
                }
            }
            pair_inodes.push(edge);
            report.pairs.push(pair);
        }
    }
    hook(Point::BeforeFinal);
    if let Err(error) = census.final_check() {
        report.observation_complete = false;
        report.issues.push(error.to_string());
        for pair in &mut report.pairs {
            if pair.state == PairState::ByteEqual {
                pair.state = PairState::Refused;
                pair.reason = "final namespace/metadata validation failed after comparison".into();
                pair.sha256 = None;
            }
        }
    }
    // Build equality components only after final validation has revoked any
    // stale successes. Deferred, mismatched and refused edges add no equality.
    let mut equalities = InodeForest::new(census.packs.len());
    for (pair, (source, target)) in report.pairs.iter().zip(pair_inodes) {
        if pair.state == PairState::ByteEqual && equalities.connect(source, target) {
            report.byte_equal_duplicate_logical_bytes = report
                .byte_equal_duplicate_logical_bytes
                .checked_add(pair.bytes)
                .ok_or_else(|| refusal("verified byte accounting overflow"))?;
        }
    }
    report.entries_observed = census.entries;
    report.packs = census.packs.into_iter().map(|p| p.row).collect();
    Ok(report)
}

#[cfg(test)]
mod tests;

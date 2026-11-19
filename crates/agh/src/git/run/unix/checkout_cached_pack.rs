// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional exact blob-pack DATA from the explicitly admitted checkout cache.
//!
//! Index membership only selects a candidate. Native Git must independently
//! validate the owned cloned pair without any alternate objects before reuse.

use super::{
    Children, Error, GitRun, Kind, Result, check_intake_file, check_object_intake_directories,
    failed, hold_intake_file, hold_physical_directories, intake_file_stamp, os, unsent,
};
use std::fs::{File, Metadata};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

const MAX_CANDIDATES: usize = 256;
const MAX_DIRECTORY_ENTRIES: usize = 1024;
const MAX_OBJECTS: usize = 16_384;
const HEADER: usize = 8 + 256 * 4;
// Every successful native type row is exactly one SHA-1, " blob" and LF.
const _: () = assert!(MAX_OBJECTS * 46 < super::ChildOut::OUTPUT_KEEP);

/// Never let pathname-only temporary cleanup remove a replacement directory.
pub(super) struct Quarantine {
    directory: Option<aterm_tempfile::TempDir>,
    held: Vec<(PathBuf, File)>,
}

impl Quarantine {
    pub(super) fn create(parent: &Path) -> Result<Self> {
        let directory = aterm_tempfile::Builder::new()
            .prefix(".agh-checkout-")
            .suffix(".noindex")
            .tempdir_in(parent)
            .map_err(|e| Error::local(format!("checkout pack quarantine: {e}")))?;
        let held = match hold_physical_directories(&[directory.path()]) {
            Ok(held) => held,
            Err(error) => {
                let _ = directory.keep();
                return Err(error);
            }
        };
        Ok(Self {
            directory: Some(directory),
            held,
        })
    }

    pub(super) fn path(&self) -> &Path {
        self.directory.as_ref().expect("live quarantine").path()
    }

    pub(super) fn check(&self) -> Result<()> {
        check_object_intake_directories(&self.held)
    }
}

impl Drop for Quarantine {
    fn drop(&mut self) {
        if self.check().is_err()
            && let Some(directory) = self.directory.take()
        {
            let _ = directory.keep();
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Advice {
    Declined,
    Ready { hash: String },
    Failed { exit: u8, why: String },
}

fn local_decision(
    children: &Children<'_>,
    question: &str,
    binding: &str,
    observe: &mut dyn FnMut() -> Result<Option<String>>,
) -> Result<Option<String>> {
    let answer = children.gh.fx().local_data(question, binding, &mut || {
        let advice = match observe() {
            Ok(Some(hash)) => Advice::Ready { hash },
            Ok(None) => Advice::Declined,
            Err(error) => Advice::Failed {
                exit: error.kind.exit(),
                why: error.why.clone(),
            },
        };
        aterm_json::to_string(&advice).expect("a finite cache decision")
    })?;
    let advice: Advice = aterm_json::from_str(&answer)
        .map_err(|_| Error::protocol("invalid retained checkout pack decision"))?;
    match advice {
        Advice::Declined => Ok(None),
        Advice::Ready { hash } if crate::path::is_sha(&hash) => Ok(Some(hash)),
        Advice::Ready { .. } => Err(Error::protocol("invalid retained checkout pack hash")),
        Advice::Failed { exit, why } => {
            let kind = Kind::ALL
                .into_iter()
                .find(|kind| kind.exit() == exit)
                .ok_or_else(|| Error::protocol("invalid retained checkout pack error"))?;
            Err(Error::new(kind, why))
        }
    }
}

fn local_check(
    children: &Children<'_>,
    question: &str,
    binding: &str,
    observe: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    if local_decision(children, question, binding, &mut || {
        observe()?;
        Ok(None)
    })?
    .is_some()
    {
        return Err(Error::protocol("a checkout custody check returned a pack"));
    }
    Ok(())
}

struct Held {
    path: PathBuf,
    file: File,
    before: Metadata,
}

impl Held {
    fn open(path: PathBuf) -> Result<Self> {
        let (file, before) = hold_intake_file(&path)?;
        Ok(Self { path, file, before })
    }

    fn check(&self) -> Result<()> {
        check_intake_file(&self.path, &self.file, &self.before)
    }
}

struct Candidate {
    hash: String,
    sources: Vec<Held>,
    owned: Vec<Held>,
    index: Vec<u8>,
    view: Option<PrivateView>,
}

impl Candidate {
    fn check(&self) -> Result<()> {
        if let Some(view) = &self.view {
            view.check()?;
        }
        self.sources
            .iter()
            .chain(&self.owned)
            .try_for_each(Held::check)
    }

    fn linked(&mut self, directory: &Path) -> Result<()> {
        self.sources.iter().try_for_each(Held::check)?;
        for held in &mut self.owned {
            let named = directory.join(held.path.file_name().expect("a pack file"));
            let installed = std::fs::symlink_metadata(named)
                .map_err(|e| Error::local(format!("installed checkout pair: {e}")))?;
            if (installed.dev(), installed.ino()) == (held.before.dev(), held.before.ino()) {
                let after = held
                    .file
                    .metadata()
                    .map_err(|e| Error::local(e.to_string()))?;
                let stable = |m: &Metadata| {
                    (
                        m.dev(),
                        m.ino(),
                        m.mode(),
                        m.uid(),
                        m.len(),
                        m.mtime(),
                        m.mtime_nsec(),
                    )
                };
                if stable(&after) != stable(&held.before)
                    || after.nlink() != held.before.nlink() + 1
                {
                    return Err(Error::refused(
                        "checkout clone changed during its owned link",
                    ));
                }
                // Only our successful hard-link transition accounts for ctime
                // and link count. The installed pair is natively checked next.
                held.before = after;
            }
            held.check()?;
        }
        // Bind the post-link native verification to the SAME exact index
        // membership, including the pack checksum, before publishing any ref.
        let index = self
            .owned
            .iter_mut()
            .find(|f| f.path.extension().is_some_and(|e| e == "idx"))
            .expect("the owned index");
        index
            .file
            .rewind()
            .map_err(|e| Error::local(e.to_string()))?;
        let mut bytes = vec![0; self.index.len()];
        index
            .file
            .read_exact(&mut bytes)
            .map_err(|e| Error::local(e.to_string()))?;
        if bytes != self.index {
            return Err(Error::refused(
                "checkout index changed during its owned link",
            ));
        }
        index.check()
    }
}

/// A minimal owned bare repository, never an existing repository reinitialized
/// by Git. Its only inputs are the exact pair, fixed config and fixed HEAD.
/// All child-directory names and stamps remain frozen across native children.
struct PrivateView {
    parents: Vec<(PathBuf, File)>,
    directories: Vec<(PathBuf, File, Metadata)>,
    files: Vec<Held>,
}

impl PrivateView {
    fn create(repository: &Path) -> Result<Self> {
        let parent = repository
            .parent()
            .ok_or_else(|| Error::local("checkout view has no owner"))?;
        let parents = hold_physical_directories(&[parent])?;
        let mut directories: Vec<(PathBuf, File, Metadata)> = Vec::new();
        // Create-new, rather than create_dir_all: no appeared path may become
        // part of the native authority, including through an ancestor symlink.
        for suffix in [
            "",
            "objects",
            "objects/info",
            "objects/pack",
            "refs",
            "refs/heads",
            "refs/tags",
        ] {
            check_object_intake_directories(&parents)?;
            for (path, file, before) in &directories {
                check_directory_identity(path, file, before)?;
            }
            let path = repository.join(suffix);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|e| Error::local(format!("create owned checkout view: {e}")))?;
            let mut held = hold_physical_directories(&[&path])?;
            let (path, file) = held.pop().expect("one held directory");
            let before = file.metadata().map_err(|e| Error::local(e.to_string()))?;
            directories.push((path, file, before));
        }
        let mut files = Vec::new();
        for (name, bytes) in [
            (
                "config",
                b"[core]\n\trepositoryformatversion = 0\n\tbare = true\n".as_slice(),
            ),
            ("HEAD", b"ref: refs/heads/agh\n".as_slice()),
        ] {
            check_object_intake_directories(&parents)?;
            for (path, file, before) in &directories {
                check_directory_identity(path, file, before)?;
            }
            let path = repository.join(name);
            let mut file = aterm_uds::kept_out(|| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)
            })
            .map_err(|e| Error::local(format!("create owned checkout config: {e}")))?;
            file.write_all(bytes)
                .map_err(|e| Error::local(e.to_string()))?;
            let before = file.metadata().map_err(|e| Error::local(e.to_string()))?;
            file.rewind().map_err(|e| Error::local(e.to_string()))?;
            let mut observed = vec![0; bytes.len()];
            file.read_exact(&mut observed)
                .map_err(|e| Error::local(e.to_string()))?;
            if observed != bytes || before.len() != bytes.len() as u64 {
                return Err(Error::refused("owned checkout config differs"));
            }
            let held = Held { path, file, before };
            held.check()?;
            files.push(held);
        }
        Ok(Self {
            parents,
            directories,
            files,
        })
    }

    fn seal(&mut self, hash: &str) -> Result<()> {
        check_object_intake_directories(&self.parents)?;
        self.files.iter().try_for_each(Held::check)?;
        let root = self.directories[0].0.clone();
        for (path, file, before) in &mut self.directories {
            let named =
                std::fs::symlink_metadata(&*path).map_err(|e| Error::local(e.to_string()))?;
            let held = file.metadata().map_err(|e| Error::local(e.to_string()))?;
            if !named.is_dir()
                || (named.dev(), named.ino(), named.mode(), named.uid())
                    != (before.dev(), before.ino(), before.mode(), before.uid())
                || (held.dev(), held.ino(), held.mode(), held.uid())
                    != (before.dev(), before.ino(), before.mode(), before.uid())
            {
                return Err(Error::refused("owned checkout view directory changed"));
            }
            let relative = path.strip_prefix(&root).expect("owned view descendant");
            let mut expected: Vec<std::ffi::OsString> = match relative.to_str() {
                Some("") => ["HEAD", "config", "objects", "refs"].map(os).to_vec(),
                Some("objects") => ["info", "pack"].map(os).to_vec(),
                Some("refs") => ["heads", "tags"].map(os).to_vec(),
                Some("objects/pack") => [format!("pack-{hash}.idx"), format!("pack-{hash}.pack")]
                    .map(os)
                    .to_vec(),
                _ => Vec::new(),
            };
            let mut actual = Vec::new();
            for entry in std::fs::read_dir(&*path).map_err(|e| Error::local(e.to_string()))? {
                actual.push(entry.map_err(|e| Error::local(e.to_string()))?.file_name());
                if actual.len() > expected.len() {
                    return Err(Error::refused("unexpected owned checkout view entry"));
                }
            }
            actual.sort_unstable();
            expected.sort_unstable();
            if actual != expected {
                return Err(Error::refused("owned checkout view namespace differs"));
            }
            // The only permitted additions were our fixed files/directories
            // and two pair links. Neither native proof is allowed to write.
            *before = held;
        }
        self.check()
    }

    fn check(&self) -> Result<()> {
        check_object_intake_directories(&self.parents)?;
        self.files.iter().try_for_each(Held::check)?;
        for (path, file, before) in &self.directories {
            check_intake_directory(path, file, before)?;
        }
        Ok(())
    }
}

fn check_intake_directory(path: &Path, file: &File, before: &Metadata) -> Result<()> {
    let held = file.metadata().map_err(|e| Error::local(e.to_string()))?;
    let named = std::fs::symlink_metadata(path).map_err(|e| Error::local(e.to_string()))?;
    if !named.is_dir()
        || intake_file_stamp(before) != intake_file_stamp(&held)
        || intake_file_stamp(before) != intake_file_stamp(&named)
    {
        return Err(Error::refused(
            "owned checkout view changed during native proof",
        ));
    }
    Ok(())
}

fn check_directory_identity(path: &Path, file: &File, before: &Metadata) -> Result<()> {
    let held = file.metadata().map_err(|e| Error::local(e.to_string()))?;
    let named = std::fs::symlink_metadata(path).map_err(|e| Error::local(e.to_string()))?;
    let identity = |m: &Metadata| (m.dev(), m.ino(), m.mode(), m.uid());
    if !named.is_dir()
        || identity(before) != identity(&held)
        || identity(before) != identity(&named)
    {
        return Err(Error::refused("owned checkout view directory changed"));
    }
    Ok(())
}

/// Keep the exact independently verified pair through its controlled install.
pub(super) struct Reused {
    pub(super) hash: String,
    binding: String,
    held: Option<Candidate>,
}

impl Reused {
    pub(super) fn check(&self, children: &Children<'_>) -> Result<()> {
        self.observe(children, "checkout retained pair custody v1", |c| c.check())
    }

    pub(super) fn linked(&mut self, children: &Children<'_>, directory: &Path) -> Result<()> {
        local_check(
            children,
            "checkout retained pair link v1",
            &self.binding,
            &mut || {
                self.held
                    .as_mut()
                    .ok_or_else(|| Error::local("missing live checkout pack custody"))?
                    .linked(directory)
            },
        )
    }

    fn observe(
        &self,
        children: &Children<'_>,
        question: &str,
        check: impl Fn(&Candidate) -> Result<()>,
    ) -> Result<()> {
        local_check(children, question, &self.binding, &mut || {
            check(
                self.held
                    .as_ref()
                    .ok_or_else(|| Error::local("missing live checkout pack custody"))?,
            )
        })
    }
}

fn stopped(children: &Children<'_>) -> Result<()> {
    if children
        .gh
        .fx()
        .git_stop()
        .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Acquire))
    {
        return Err(Error::new(
            Kind::Interrupted,
            "checkout pack reuse interrupted",
        ));
    }
    Ok(())
}

/// A bounded v2 index hint, never object integrity or permission. The complete
/// index must fit its declared object count; trailing retained text is no input.
fn member_hint(bytes: &[u8], wanted: &[String], hash: &str) -> bool {
    if wanted.is_empty()
        || wanted.len() > MAX_OBJECTS
        || bytes.get(..8) != Some(&[255, b't', b'O', b'c', 0, 0, 0, 2])
    {
        return false;
    }
    let count = bytes
        .get(HEADER - 4..HEADER)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes);
    if count != u32::try_from(wanted.len()).ok() {
        return false;
    }
    let minimum = HEADER + wanted.len() * 28 + 40;
    let maximum = minimum + wanted.len() * 8;
    if bytes.len() < minimum || bytes.len() > maximum || (bytes.len() - minimum) % 8 != 0 {
        return false;
    }
    let mut expected_hash = [0; 20];
    if !aterm_codec::hex::decode_into(hash.as_bytes(), &mut expected_hash)
        || bytes[bytes.len() - 40..bytes.len() - 20] != expected_hash
    {
        return false;
    }
    let Some(names) = bytes.get(HEADER..HEADER + wanted.len() * 20) else {
        return false;
    };
    names.chunks_exact(20).zip(wanted).all(|(actual, oid)| {
        let mut expected = [0; 20];
        aterm_codec::hex::decode_into(oid.as_bytes(), &mut expected) && actual == expected
    })
}

fn find(
    children: &Children<'_>,
    cache: &Path,
    wanted: &[String],
    prefix: &Path,
) -> Result<Option<Candidate>> {
    if wanted.is_empty() || wanted.len() > MAX_OBJECTS {
        return Ok(None);
    }
    let mut indices = Vec::new();
    let entries = match std::fs::read_dir(cache.join("objects/pack")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::local(format!("read checkout cache packs: {error}"))),
    };
    for (visited, entry) in entries.enumerate() {
        stopped(children)?;
        if visited == MAX_DIRECTORY_ENTRIES {
            return Ok(None);
        }
        let entry = entry.map_err(|e| Error::local(format!("read checkout cache pack: {e}")))?;
        let name = entry.file_name();
        let Some(hash) = name
            .to_str()
            .and_then(|s| s.strip_prefix("pack-"))
            .and_then(|s| s.strip_suffix(".idx"))
            .filter(|s| crate::path::is_sha(s))
        else {
            continue;
        };
        indices.push((hash.to_owned(), entry.path()));
        if indices.len() > MAX_CANDIDATES {
            return Ok(None);
        }
    }
    indices.sort_unstable();
    for (hash, index) in indices {
        stopped(children)?;
        let mut index = Held::open(index)?;
        let bound = HEADER + wanted.len() * 36 + 40;
        if index.before.len() > bound as u64 {
            continue;
        }
        let mut bytes = Vec::with_capacity(index.before.len() as usize);
        let mut chunk = [0; 16 * 1024];
        loop {
            stopped(children)?;
            let n = index
                .file
                .read(&mut chunk)
                .map_err(|e| Error::local(format!("read checkout cache index: {e}")))?;
            if n == 0 {
                break;
            }
            if bytes.len().saturating_add(n) > bound {
                return Err(Error::refused("checkout cache index changed while reading"));
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        index.check()?;
        if !member_hint(&bytes, wanted, &hash) {
            continue;
        }
        let pack = Held::open(index.path.with_extension("pack"))?;
        let sources = vec![pack, index];
        let mut cloned = Vec::new();
        for source in &sources {
            stopped(children)?;
            let name = source.path.file_name().expect("a cache pack name");
            let destination = prefix.with_file_name(name);
            let supported = aterm_uds::regular_file::clone_held(&source.file, &destination)
                .map_err(|e| Error::local(format!("clone checkout cache pair: {e}")))?;
            if !supported {
                for file in &cloned {
                    let file: &Held = file;
                    file.check()?;
                    std::fs::remove_file(&file.path)
                        .map_err(|e| Error::local(format!("remove unused checkout clone: {e}")))?;
                }
                return Ok(None);
            }
            let mut held = Held::open(destination)?;
            if (held.before.dev(), held.before.ino()) == (source.before.dev(), source.before.ino())
                || held.before.nlink() != 1
            {
                return Err(Error::refused(
                    "checkout clone has no independent file custody",
                ));
            }
            held.file
                .set_permissions(std::fs::Permissions::from_mode(0o444))
                .map_err(|e| Error::local(format!("protect checkout pack clone: {e}")))?;
            held.before = held
                .file
                .metadata()
                .map_err(|e| Error::local(e.to_string()))?;
            source.check()?;
            cloned.push(held);
        }
        let candidate = Candidate {
            hash,
            sources,
            owned: cloned,
            index: bytes,
            view: None,
        };
        candidate.check()?;
        return Ok(Some(candidate));
    }
    Ok(None)
}

/// Reuse only after complete native verification of an independently owned pair.
/// A recorded decision is DATA: replay must still consume the same native checks.
pub(super) fn reuse(
    children: &Children<'_>,
    run: &GitRun,
    cache: &Path,
    wanted: &[String],
    prefix: &Path,
    batch: usize,
) -> Result<Option<Reused>> {
    let canonical = crate::fx::canon::git_request(run);
    let binding = crate::model::Sha256Digest::of(
        &[
            canonical.as_slice(),
            b"\nblob batch\n",
            wanted.join("\n").as_bytes(),
        ]
        .concat(),
    )
    .to_string();
    let started = children.gh.fx().mono()?;
    let mut candidate = None;
    let selected = local_decision(
        children,
        "checkout retained blob pair v1",
        &binding,
        &mut || {
            candidate = find(children, cache, wanted, prefix)?;
            Ok(candidate.as_ref().map(|c| c.hash.clone()))
        },
    )?;
    let finished = children.gh.fx().mono()?;
    if finished.saturating_sub(started) >= children.budget {
        return Err(Error::local(
            "checkout pack lookup exceeded its operation bound",
        ));
    }
    let Some(hash) = selected else {
        return Ok(None);
    };
    let step =
        |name: &str, args: Vec<std::ffi::OsString>, cwd: Option<&Path>, input: Option<&[u8]>| {
            let key = [
                canonical.as_slice(),
                format!("\nstep checkout-retained-{batch}-{name}").as_bytes(),
            ]
            .concat();
            let out = match input {
                Some(input) => children.git_input(&args, cwd, &key, input),
                None => children.git(&args, cwd, &key),
            }
            .map_err(unsent)?;
            if out.code != Some(0) {
                return Err(failed(
                    &run.op,
                    &out,
                    children.gh.redactor(),
                    &run.slug,
                    false,
                ));
            }
            Ok(out)
        };
    // All private links stay on the receiving quarantine's filesystem. A
    // supported donor clone must not depend on the system scratch device.
    let repository = prefix
        .parent()
        .ok_or_else(|| Error::local("checkout pack has no owner"))?
        .join(format!("checkout-pack-verifier-{batch}.git"));
    let pack_directory = repository.join("objects/pack");
    local_check(
        children,
        "checkout retained private view v1",
        &binding,
        &mut || {
            let candidate = candidate
                .as_mut()
                .ok_or_else(|| Error::local("missing live checkout pack custody"))?;
            candidate.check()?;
            let mut view = PrivateView::create(&repository)?;
            for file in &candidate.owned {
                std::fs::hard_link(
                    &file.path,
                    pack_directory.join(file.path.file_name().expect("pack name")),
                )
                .map_err(|e| Error::local(format!("link owned checkout verifier pair: {e}")))?;
            }
            candidate.linked(&pack_directory)?;
            view.seal(&candidate.hash)?;
            candidate.view = Some(view);
            candidate.check()
        },
    )?;
    step(
        "verify",
        vec![
            os("--git-dir"),
            os("."),
            os("-c"),
            os("protocol.allow=never"),
            os("index-pack"),
            os("--verify"),
            os("--strict"),
            os(format!("objects/pack/pack-{hash}.pack")),
        ],
        Some(&repository),
        None,
    )?;
    local_check(
        children,
        "checkout retained verified view v1",
        &binding,
        &mut || {
            candidate
                .as_ref()
                .ok_or_else(|| Error::local("missing live checkout pack custody"))?
                .check()
        },
    )?;
    let input = format!("{}\n", wanted.join("\n"));
    let types = step(
        "types",
        vec![
            os("--git-dir"),
            os("."),
            os("-c"),
            os("protocol.allow=never"),
            os("cat-file"),
            os("--batch-check=%(objectname) %(objecttype)"),
        ],
        Some(&repository),
        Some(input.as_bytes()),
    )?;
    let expected: String = wanted.iter().map(|oid| format!("{oid} blob\n")).collect();
    if types.stdout_may_be_truncated() || types.stdout != expected.as_bytes() {
        return Err(Error::protocol(
            "retained checkout pack does not contain the exact blob batch",
        ));
    }
    let reused = Reused {
        hash,
        binding,
        held: candidate,
    };
    reused.check(children)?;
    Ok(Some(reused))
}

#[cfg(test)]
#[path = "checkout_cached_pack_tests.rs"]
mod tests;

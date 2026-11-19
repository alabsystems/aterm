// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Recover only omitted blob dependencies licensed by freshly observed remote history.
//! This certificate never substitutes for the push's strict object validation.

use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::PathBuf;

use super::{
    Children, Error, FreshRemoteRoots, GitRun, Remote, Result, os, spooled_object_records, unsent,
};

// A missing record is exactly '?' + 40 hex digits + LF. This is the existing
// complete-inventory admission bound, not a new lower history-size limit.
const MAX_DEPENDENCIES: usize = crate::fx::child::MAX_RECORD_SPOOL_BYTES / 42;
const BATCH: usize = 2048;

/// Private fields and the sole constructor keep observations from becoming intake authority.
pub(super) struct Certificate {
    request: Vec<u8>,
    roots: Vec<String>,
    metadata_pack: String,
    metadata_files: Vec<MetadataFile>,
    blobs: Vec<String>,
    deadline: std::time::Duration,
}

fn bounded<'a>(children: &Children<'a>, deadline: std::time::Duration) -> Result<Children<'a>> {
    super::source_copy_check(children, deadline)?;
    let mut native = children.clone();
    native.budget = deadline
        .checked_sub(children.gh.fx().mono()?)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Error::refused("push historical preparation budget exhausted"))?;
    Ok(native)
}

struct MetadataFile {
    path: PathBuf,
    file: std::fs::File,
    stamp: (u64, u64, u64, i64, i64, i64, i64),
}

fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

impl MetadataFile {
    fn open(path: PathBuf) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| Error::local(format!("pin original metadata: {e}")))?;
        let meta = file.metadata().map_err(|e| Error::local(e.to_string()))?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(Error::refused(
                "original metadata file is not privately owned",
            ));
        }
        let pinned = Self {
            path,
            file,
            stamp: stamp(&meta),
        };
        pinned.check()?;
        Ok(pinned)
    }

    fn check(&self) -> Result<()> {
        let named =
            std::fs::symlink_metadata(&self.path).map_err(|e| Error::local(e.to_string()))?;
        let opened = self
            .file
            .metadata()
            .map_err(|e| Error::local(e.to_string()))?;
        if stamp(&named) != self.stamp || stamp(&opened) != self.stamp || !named.is_file() {
            return Err(Error::refused(
                "original metadata pack/index changed during certification",
            ));
        }
        Ok(())
    }
}

fn key(run: &GitRun, name: &str) -> Vec<u8> {
    [
        crate::fx::canon::git_request(run),
        format!("\nstep push-history-{name}").into_bytes(),
    ]
    .concat()
}

fn succeeded(out: super::ChildOut, name: &str) -> Result<super::ChildOut> {
    if out.code != Some(0) {
        return Err(Error::could_not_run(format!(
            "push preparation {name} did not succeed"
        )));
    }
    Ok(out)
}

fn checked(out: super::ChildOut, name: &str) -> Result<super::ChildOut> {
    let out = succeeded(out, name)?;
    if out.stdout_may_be_truncated() {
        return Err(Error::protocol(format!(
            "push preparation {name} has incomplete captured output"
        )));
    }
    Ok(out)
}

/// `--quiet --missing=print` emits only missing IDs. A second, blob-filtered walk
/// must emit none: Git therefore traversed every original commit/tree edge, and
/// every omission in the first walk was a blob edge, never a missing subtree.
fn missing(
    children: &Children<'_>,
    run: &GitRun,
    name: &str,
    roots: &[&str],
    excludes: &[&str],
    metadata_only: bool,
    deadline: std::time::Duration,
) -> Result<Vec<String>> {
    let scratch = children
        .scratch_git
        .as_deref()
        .ok_or_else(|| Error::local("historical dependency observation needs private storage"))?;
    let path = scratch
        .parent()
        .ok_or_else(|| Error::local("private history parent missing"))?
        .join(format!("push-history-{name}.records"));
    let mut original = bounded(children, deadline)?;
    original.env.extend([
        (os("GIT_GRAFT_FILE"), os("/dev/null")),
        (os("GIT_SHALLOW_FILE"), os("/dev/null")),
        (os("GIT_NO_REPLACE_OBJECTS"), os("1")),
        (os("GIT_NO_LAZY_FETCH"), os("1")),
    ]);
    original.stdout_records = Some(path.clone());
    let mut args = vec![
        os("-c"),
        os("core.commitGraph=false"),
        os("-c"),
        os("pack.useBitmaps=false"),
        os("rev-list"),
        os("--objects"),
        os("--no-object-names"),
        os("--quiet"),
        os("--missing=print"),
    ];
    if metadata_only {
        args.push(os("--filter=blob:none"));
    }
    args.extend(roots.iter().map(|oid| os(*oid)));
    args.extend(excludes.iter().map(|oid| os(format!("^{oid}"))));
    args.push(os("--"));
    // Authority comes from the complete owned spool, not diagnostic capture.
    // The current spool producer keeps no stdout in ChildOut at all.
    succeeded(
        original
            .git(&args, Some(scratch), &key(run, name))
            .map_err(unsent)?,
        name,
    )?;
    let mut ids = BTreeSet::new();
    spooled_object_records(&path, children, deadline, |_, batch, marked_missing| {
        if !marked_missing {
            return Err(Error::protocol(
                "unexpected present object in missing-only observation",
            ));
        }
        for oid in batch {
            if ids.len() == MAX_DEPENDENCIES && !ids.contains(oid) {
                return Err(Error::refused(
                    "historical dependency count exceeds the bounded intake",
                ));
            }
            if !ids.insert(oid.clone()) {
                return Err(Error::protocol(
                    "duplicate historical dependency observation",
                ));
            }
        }
        Ok(())
    })?;
    std::fs::remove_file(path)
        .map_err(|e| Error::local(format!("historical dependency observation: {e}")))?;
    Ok(ids.into_iter().collect())
}

/// Before a historical read, hash-check every original new object too. Link
/// completeness still belongs to the later strict checks; no absent new body
/// or substituted new blob may authorize even an otherwise licensed read.
fn verify_new_objects(
    children: &Children<'_>,
    run: &GitRun,
    roots: &[&str],
    excludes: &[&str],
    deadline: std::time::Duration,
) -> Result<Vec<MetadataFile>> {
    let scratch = children
        .scratch_git
        .as_deref()
        .ok_or_else(|| Error::local("new-object verification needs private storage"))?;
    let path = scratch
        .parent()
        .ok_or_else(|| Error::local("private history parent missing"))?
        .join("push-history-original-new.records");
    let mut reader = bounded(children, deadline)?;
    reader.stdout_records = Some(path.clone());
    reader.env.extend([
        (os("GIT_GRAFT_FILE"), os("/dev/null")),
        (os("GIT_SHALLOW_FILE"), os("/dev/null")),
        (os("GIT_NO_REPLACE_OBJECTS"), os("1")),
        (os("GIT_NO_LAZY_FETCH"), os("1")),
    ]);
    let mut args = vec![
        os("-c"),
        os("core.commitGraph=false"),
        os("-c"),
        os("pack.useBitmaps=false"),
        os("rev-list"),
        os("--objects"),
        os("--no-object-names"),
        os("--missing=print"),
    ];
    args.extend(roots.iter().map(|id| os(*id)));
    args.extend(excludes.iter().map(|id| os(format!("^{id}"))));
    args.push(os("--"));
    succeeded(
        reader
            .git(&args, Some(scratch), &key(run, "original-new-inventory"))
            .map_err(unsent)?,
        "original-new-inventory",
    )?;
    let mut files = Vec::new();
    super::spooled_object_records(&path, children, deadline, |index, batch, missing| {
        let native = bounded(children, deadline)?;
        if missing {
            return Err(Error::refused("new push objects are missing"));
        }
        let input = format!("{}\n", batch.join("\n"));
        let prefix = scratch.join("objects/pack/pack");
        let out = checked(
            native
                .git_input(
                    &[
                        os("-c"),
                        os("pack.useBitmaps=false"),
                        os("pack-objects"),
                        os("--quiet"),
                        os("--no-revs"),
                        os("--threads=1"),
                        os("--no-use-bitmap-index"),
                        prefix.as_os_str().to_owned(),
                    ],
                    Some(scratch),
                    &key(run, &format!("original-new-pack-{index}")),
                    input.as_bytes(),
                )
                .map_err(unsent)?,
            "original-new-pack",
        )?;
        let hash = std::str::from_utf8(&out.stdout)
            .ok()
            .and_then(|s| s.strip_suffix('\n'))
            .filter(|s| crate::path::is_sha(s))
            .ok_or_else(|| Error::protocol("new original pack ID malformed"))?;
        let pinned = ["pack", "idx"]
            .into_iter()
            .map(|ext| {
                MetadataFile::open(
                    scratch
                        .join("objects/pack")
                        .join(format!("pack-{hash}.{ext}")),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let native = bounded(children, deadline)?;
        checked(
            native
                .git(
                    &[
                        os("index-pack"),
                        os("--verify"),
                        os("--fsck-objects"),
                        pinned[0].path.as_os_str().to_owned(),
                    ],
                    Some(scratch),
                    &key(run, &format!("original-new-verify-{index}")),
                )
                .map_err(unsent)?,
            "original-new-verify",
        )?;
        for file in &pinned {
            file.check()?;
        }
        files.extend(pinned);
        Ok(())
    })?;
    std::fs::remove_file(path).map_err(|e| Error::local(e.to_string()))?;
    Ok(files)
}

/// Called only after fresh remote expected-ref matching. Bind every original
/// metadata byte to its index ID before certifying edges; no source ref supplies roots.
pub(super) fn certify(
    children: &Children<'_>,
    run: &GitRun,
    fresh: &FreshRemoteRoots,
    remote_main: Option<&str>,
    metadata_pack: &str,
) -> Result<Certificate> {
    if fresh.request != crate::fx::canon::git_request(run) {
        return Err(Error::refused("fresh remote roots belong to another push"));
    }
    let owned_roots = fresh.roots(remote_main)?;
    let after_roots = fresh.after.iter().map(String::as_str).collect::<Vec<_>>();
    let native = bounded(children, fresh.deadline)?;
    if owned_roots.is_empty()
        || !crate::path::is_sha(metadata_pack)
        || owned_roots
            .iter()
            .chain(&after_roots)
            .any(|id| !crate::path::is_sha(id))
    {
        return Err(Error::refused(
            "historical dependencies lack exact authenticated roots",
        ));
    }
    let scratch = children
        .scratch_git
        .as_deref()
        .ok_or_else(|| Error::local("historical certification needs private storage"))?;
    let mut metadata_files = ["pack", "idx"]
        .into_iter()
        .map(|ext| {
            MetadataFile::open(
                scratch
                    .join("objects/pack")
                    .join(format!("pack-{metadata_pack}.{ext}")),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    // --fsck-objects checks original object bodies without requiring excluded
    // blob links. --verify also binds their recomputed IDs to this pinned index.
    // The caller's subsequent --strict and full connectivity remain mandatory.
    checked(
        native
            .git(
                &[
                    os("index-pack"),
                    os("--verify"),
                    os("--fsck-objects"),
                    metadata_files[0].path.as_os_str().to_owned(),
                ],
                Some(scratch),
                &key(run, "original-metadata-ids"),
            )
            .map_err(unsent)?,
        "original-metadata-ids",
    )?;
    for file in &metadata_files {
        file.check()?;
    }
    // All observations finish before Certificate exists or any remote body read begins.
    let blobs = missing(
        children,
        run,
        "owned-missing",
        &owned_roots,
        &[],
        false,
        fresh.deadline,
    )?;
    if !missing(
        children,
        run,
        "owned-metadata",
        &owned_roots,
        &[],
        true,
        fresh.deadline,
    )?
    .is_empty()
    {
        return Err(Error::refused(
            "remote-owned history is missing commit or tree metadata",
        ));
    }
    if !after_roots.is_empty()
        && !missing(
            children,
            run,
            "new-missing",
            &after_roots,
            &owned_roots,
            false,
            fresh.deadline,
        )?
        .is_empty()
    {
        return Err(Error::refused(
            "new push objects are missing; historical intake is not authorized",
        ));
    }
    if !blobs.is_empty() && !after_roots.is_empty() {
        metadata_files.extend(verify_new_objects(
            children,
            run,
            &after_roots,
            &owned_roots,
            fresh.deadline,
        )?);
    }
    for file in &metadata_files {
        file.check()?;
    }
    Ok(Certificate {
        request: crate::fx::canon::git_request(run),
        roots: owned_roots.iter().map(|id| (*id).to_owned()).collect(),
        metadata_pack: metadata_pack.to_owned(),
        metadata_files,
        blobs,
        deadline: fresh.deadline,
    })
}

impl Certificate {
    pub(super) fn intake(
        self,
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        fresh: &FreshRemoteRoots,
        remote_main: Option<&str>,
        metadata_pack: &str,
    ) -> Result<()> {
        let owned_roots = fresh.roots(remote_main)?;
        if self.request != crate::fx::canon::git_request(run)
            || self.metadata_pack != metadata_pack
            || self.request != fresh.request
            || remote.url() != fresh.remote_url
            || self.deadline != fresh.deadline
            || !self
                .roots
                .iter()
                .map(String::as_str)
                .eq(owned_roots.iter().copied())
        {
            return Err(Error::refused(
                "historical dependency certificate belongs to another push boundary",
            ));
        }
        for file in &self.metadata_files {
            file.check()?;
        }
        let scratch = children
            .scratch_git
            .as_deref()
            .ok_or_else(|| Error::local("historical dependency intake needs private storage"))?;
        let parent = scratch
            .parent()
            .ok_or_else(|| Error::local("private history parent missing"))?;
        for (index, ids) in self.blobs.chunks(BATCH).enumerate() {
            for file in &self.metadata_files {
                file.check()?;
            }
            // A new empty object store for each batch advertises no source haves,
            // borrows no alternates/config, and bounds the complete membership answer.
            let worker = aterm_tempfile::Builder::new()
                .prefix("push-history-")
                .suffix(".noindex")
                .tempdir_in(parent)
                .map_err(|e| Error::local(format!("private historical intake: {e}")))?;
            let dir = worker.path().join("objects.git");
            let mut native = bounded(children, self.deadline)?;
            native.scratch_git = Some(dir.clone());
            native.stdout_records = None;
            // Never inherit an object-directory override or an alternate have
            // set, even if a future caller adds one to its private read view.
            native.env.retain(|(name, _)| {
                !matches!(
                    name.to_str(),
                    Some(
                        "GIT_OBJECT_DIRECTORY"
                            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                            | "GIT_COMMON_DIR"
                            | "GIT_DIR"
                    )
                )
            });
            native.env.extend([
                (os("GIT_NO_LAZY_FETCH"), os("1")),
                (os("GIT_NO_REPLACE_OBJECTS"), os("1")),
            ]);
            let local = |name: &str, args: Vec<std::ffi::OsString>, input: Option<&[u8]>| {
                let name = format!("intake-{index}-{name}");
                let native = bounded(&native, self.deadline)?;
                let out = match input {
                    Some(input) => native.git_input(&args, Some(&dir), &key(run, &name), input),
                    None => native.git(&args, Some(&dir), &key(run, &name)),
                }
                .map_err(unsent)?;
                checked(out, &name)
            };
            checked(
                native
                    .git(
                        &[
                            os("init"),
                            os("--quiet"),
                            os("--bare"),
                            os("--template="),
                            dir.as_os_str().to_owned(),
                        ],
                        None,
                        &key(run, &format!("intake-{index}-init")),
                    )
                    .map_err(unsent)?,
                "historical-intake-init",
            )?;
            let input = format!("{}\n", ids.join("\n"));
            let native = bounded(&native, self.deadline)?;
            let fetched = native
                .remote_read(
                    run,
                    &[
                        os("-c"),
                        os("fetch.fsckObjects=true"),
                        os("-c"),
                        os("transfer.fsckObjects=true"),
                        os("fetch"),
                        os("--quiet"),
                        os("--no-tags"),
                        os("--no-write-fetch-head"),
                        os("--recurse-submodules=no"),
                        os("--stdin"),
                        os(remote.url()),
                    ],
                    Some(&dir),
                    &key(run, &format!("intake-{index}-fetch")),
                    Some(input.as_bytes()),
                )
                .map_err(unsent)?;
            checked(fetched, "historical-intake-fetch")?;
            let inventory = local(
                "inventory",
                vec![
                    os("cat-file"),
                    os("--batch-all-objects"),
                    os("--batch-check=%(objectname) %(objecttype)"),
                ],
                None,
            )?;
            let text = std::str::from_utf8(&inventory.stdout)
                .map_err(|_| Error::protocol("historical intake inventory is not UTF-8"))?;
            let expected = ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
            let mut found = BTreeSet::new();
            for line in text.lines() {
                let Some((id, "blob")) = line.split_once(' ') else {
                    return Err(Error::refused(
                        "historical intake contains an unexpected object type",
                    ));
                };
                if !expected.contains(id) || !found.insert(id) {
                    return Err(Error::refused(
                        "historical intake contains an unlicensed object",
                    ));
                }
            }
            if found != expected || (!text.is_empty() && !text.ends_with('\n')) {
                return Err(Error::refused(
                    "historical intake did not contain the exact licensed blobs",
                ));
            }
            // Copy only licensed IDs, with no borrowed delta dependencies, into
            // the primary PRIVATE store. The caller's object store is untouched.
            let prefix = scratch.join("objects/pack/pack");
            let packed = local(
                "pack",
                vec![
                    os("pack-objects"),
                    os("--quiet"),
                    os("--no-revs"),
                    os("--threads=1"),
                    os("--no-reuse-delta"),
                    os("--no-reuse-object"),
                    prefix.as_os_str().to_owned(),
                ],
                Some(input.as_bytes()),
            )?;
            let hash = std::str::from_utf8(&packed.stdout)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| crate::path::is_sha(s))
                .ok_or_else(|| Error::protocol("historical intake pack identity is malformed"))?;
            local(
                "verify",
                vec![
                    os("index-pack"),
                    os("--verify"),
                    os("--strict"),
                    prefix
                        .with_file_name(format!("pack-{hash}.pack"))
                        .into_os_string(),
                ],
                None,
            )?;
        }
        for file in &self.metadata_files {
            file.check()?;
        }
        super::source_copy_check(children, self.deadline)?;
        Ok(())
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A commit-only object view: reuse immutable objects, never fetch a tree or
//! treat a shallow boundary, graft, replacement or ref as negative evidence.

use aterm_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use super::{
    ANONYMOUS_READ_PROGRESS_WINDOW, ChildOut, Children, Error, GitRun, Kind, Result, os, screened,
};
use crate::git::{AncestryQuery, CommitRead};
use crate::github::How;
use crate::policy::RouteTarget;

mod object;

const BODY_MAX: usize = 256 * 1024;
const API_BYTES_MAX: usize = 32 * 1024 * 1024;
const ACQUIRED_MAX: usize = 4096;
const WALK_MAX: usize = 1_000_000;
const WALK_BYTES_MAX: usize = 256 * 1024 * 1024;

fn local_children<'gh>(children: &Children<'gh>) -> Children<'gh> {
    let mut local = children.clone();
    local.env.retain(|(key, _)| key != "GIT_NO_LAZY_FETCH");
    local.env.push((os("GIT_NO_LAZY_FETCH"), os("1")));
    local
}

struct Reader<'a, 'gh> {
    children: &'a Children<'gh>,
    run: &'a GitRun,
    source: PathBuf,
    view: Option<PathBuf>,
    acquired: usize,
    api_bytes: usize,
    serial: usize,
    how: How,
    local_only: bool,
}

impl Reader<'_, '_> {
    fn git(
        &mut self,
        name: &str,
        args: Vec<std::ffi::OsString>,
        source: bool,
        input: Option<&[u8]>,
    ) -> Result<ChildOut> {
        let children = local_children(self.children);
        self.serial += 1;
        let key = [
            crate::fx::canon::git_request(self.run),
            format!("\ncommit-step {} {name}", self.serial).into_bytes(),
        ]
        .concat();
        let mut all = vec![os("-c"), os("core.commitGraph=false")];
        if source {
            all.extend([os("-C"), self.source.as_os_str().to_owned()]);
        }
        all.extend(args);
        let cwd = if source { None } else { self.view.as_deref() };
        match input {
            Some(bytes) => children.git_input(&all, cwd, &key, bytes),
            None => children.git(&all, cwd, &key),
        }
    }

    fn checked(
        &mut self,
        name: &str,
        args: Vec<std::ffi::OsString>,
        source: bool,
        input: Option<&[u8]>,
    ) -> Result<ChildOut> {
        let out = self.git(name, args, source, input)?;
        if out.code != Some(0) {
            return Err(Error::local(format!(
                "native commit {name} failed; original ancestry is unknown"
            )));
        }
        Ok(out)
    }

    fn read(&mut self, ids: &[String]) -> Result<Vec<Vec<u8>>> {
        let text = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
        let info = self.checked(
            "info",
            vec![os("cat-file"), os("--batch-check")],
            false,
            Some(text.as_bytes()),
        )?;
        let info = std::str::from_utf8(&info.stdout)
            .map_err(|_| Error::protocol("invalid commit object inventory"))?;
        let lines = info.lines().collect::<Vec<_>>();
        if lines.len() != ids.len() {
            return Err(Error::protocol("incomplete commit object inventory"));
        }
        for (id, line) in ids.iter().zip(lines) {
            if line == format!("{id} missing") {
                if self.local_only {
                    return Err(Error::local("the local commit witness is incomplete"));
                }
                self.acquire(id)?;
            } else {
                let fields = line.split(' ').collect::<Vec<_>>();
                if fields.len() != 3
                    || fields[0] != id
                    || fields[1] != "commit"
                    || fields[2]
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n <= BODY_MAX)
                        .is_none()
                {
                    return Err(Error::protocol(
                        "requested object is not a bounded exact commit",
                    ));
                }
            }
        }
        // Eight maximally sized objects could exceed the child capture. Read
        // each body's size first, then pack bounded contents queries below.
        let mut bodies = Vec::with_capacity(ids.len());
        for group in ids.chunks(2) {
            let text = group.iter().map(|id| format!("{id}\n")).collect::<String>();
            let out = self.checked(
                "contents",
                vec![os("cat-file"), os("--batch")],
                false,
                Some(text.as_bytes()),
            )?;
            let mut bytes = out.stdout.as_slice();
            for id in group {
                let end = bytes
                    .iter()
                    .position(|b| *b == b'\n')
                    .ok_or_else(|| Error::protocol("truncated commit header"))?;
                let header = std::str::from_utf8(&bytes[..end])
                    .map_err(|_| Error::protocol("invalid commit header"))?;
                let fields = header.split(' ').collect::<Vec<_>>();
                let size = fields
                    .get(2)
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|n| *n <= BODY_MAX)
                    .ok_or_else(|| Error::protocol("commit size is unavailable or too large"))?;
                if fields.len() != 3
                    || fields[0] != id
                    || fields[1] != "commit"
                    || bytes.len() < end + 2 + size
                    || bytes[end + 1 + size] != b'\n'
                {
                    return Err(Error::protocol("commit contents lost exact object framing"));
                }
                let body = &bytes[end + 1..end + 1 + size];
                if object::oid(body) != *id {
                    return Err(Error::protocol(
                        "held commit bytes differ from their object ID",
                    ));
                }
                bodies.push(body.to_vec());
                bytes = &bytes[end + 2 + size..];
            }
            if !bytes.is_empty() {
                return Err(Error::protocol("extra commit object bytes"));
            }
        }
        Ok(bodies)
    }

    fn acquire(&mut self, id: &str) -> Result<()> {
        if self.acquired >= ACQUIRED_MAX {
            return Err(Error::local(
                "commit acquisition count bound reached; ancestry unknown",
            ));
        }
        let mut call = self
            .how
            .get(
                format!(
                    "/repos/{}/{}/git/commits/{id}",
                    self.run.slug.owner(),
                    self.run.slug.repo()
                ),
                RouteTarget::Repo(self.run.slug.clone()),
            )
            .limit((BODY_MAX * 4) as u64)
            .deadlines(crate::wire::Deadlines::JSON);
        call.follow_moved = false;
        let answer = self.children.gh.call(&call)?;
        self.api_bytes = self
            .api_bytes
            .checked_add(answer.body.len())
            .filter(|n| *n <= API_BYTES_MAX)
            .ok_or_else(|| Error::local("commit API byte bound reached; ancestry unknown"))?;
        let raw = object::from_api(id, &answer.json()?)?;
        let out = self.checked(
            "write-exact",
            vec![
                os("hash-object"),
                os("-t"),
                os("commit"),
                os("-w"),
                os("--stdin"),
            ],
            true,
            Some(&raw),
        )?;
        if out.stdout != format!("{id}\n").as_bytes() {
            return Err(Error::protocol(
                "native Git did not confirm the original commit ID",
            ));
        }
        self.acquired += 1;
        Ok(())
    }

    fn ask(&mut self, query: &AncestryQuery) -> Result<Option<bool>> {
        let out = self.git(
            "merge-base",
            vec![
                os("merge-base"),
                os("--is-ancestor"),
                os(&query.ancestor),
                os(&query.descendant),
            ],
            false,
            None,
        )?;
        match out.code {
            Some(0) => Ok(Some(true)),
            Some(1) => Ok(Some(false)),
            _ => Ok(None),
        }
    }

    fn contained(&mut self, query: &AncestryQuery) -> Result<bool> {
        self.read(&[query.ancestor.clone(), query.descendant.clone()])?;
        if query.ancestor == query.descendant {
            return Ok(true);
        }
        // A native positive is an actual original parent path. A negative is
        // withheld until every reachable raw parent has been read and hashed.
        if self.ask(query)? == Some(true) {
            return Ok(true);
        }
        let mut pending = VecDeque::from([query.descendant.clone()]);
        let mut seen = BTreeSet::from([query.descendant.clone()]);
        let mut bytes = 0_usize;
        while !pending.is_empty() {
            let batch = (0..pending.len().min(128))
                .filter_map(|_| pending.pop_front())
                .collect::<Vec<_>>();
            let acquired_before = self.acquired;
            for body in self.read(&batch)? {
                bytes = bytes
                    .checked_add(body.len())
                    .filter(|n| *n <= WALK_BYTES_MAX)
                    .ok_or_else(|| {
                        Error::local("commit walk byte bound reached; ancestry unknown")
                    })?;
                for parent in object::parents(&body)? {
                    if seen.insert(parent.clone()) {
                        if seen.len() > WALK_MAX {
                            return Err(Error::local(
                                "commit walk count bound reached; ancestry unknown",
                            ));
                        }
                        // This edge is from an exact hashed original body;
                        // the requested ancestor's body was also validated.
                        // Its own older history cannot invalidate this path.
                        if parent == query.ancestor {
                            return Ok(true);
                        }
                        pending.push_back(parent);
                    }
                }
            }
            if self.acquired != acquired_before && self.ask(query)? == Some(true) {
                return Ok(true);
            }
        }
        // Every reachable original descendant parent was read and hashed,
        // and none was the validated requested ancestor. Git merge-base may
        // still require unrelated ancestor-side history before answering; it
        // is unnecessary for this complete negative and must not trigger a
        // whole-history acquisition. Missing descendant parents still fail
        // above, as do every original work and no-progress bound.
        Ok(false)
    }
}

pub(super) fn read(
    children: &Children<'_>,
    run: &GitRun,
    dir: &Path,
    request: &CommitRead,
    profile: Option<&str>,
    anonymous: bool,
) -> Result<ChildOut> {
    if !anonymous && profile.is_none() {
        return Err(Error::new(
            Kind::Unauthenticated,
            "commit ancestry requires the routed identity or explicit --anonymous",
        ));
    }
    // This operation is bounded by exact object, byte and walk counts below. An anonymous
    // reader's local Git children therefore use an inactivity window, just like its network
    // children, rather than turning a long but progressing original-parent walk into a timeout.
    // The shared object-directory inventory is also a local child of this
    // operation. Its source may be filtered, so cover setup as well as Reader.
    let mut children = local_children(children);
    if anonymous && children.progress_window.is_none() {
        children.progress_window = Some(ANONYMOUS_READ_PROGRESS_WINDOW);
    }
    let children = &children;
    // Only this immutable commit reader admits the exact filtered-clone metadata.
    // Every Reader Git child still disables lazy fetch, including source writes.
    let source = screened(children, run, dir)?;
    let mut reader = Reader {
        children,
        run,
        source,
        view: children.scratch_git.clone(),
        acquired: 0,
        api_bytes: 0,
        serial: 0,
        how: How::read()
            .named(run.named.clone())
            .as_profile(profile.map(str::to_owned))
            .anonymous(anonymous)
            .retry_policy(if anonymous {
                crate::retry::RetryPolicy::ANONYMOUS_HTTP
            } else {
                crate::retry::RetryPolicy::NONE
            }),
        local_only: false,
    };
    let common = reader.checked(
        "common-dir",
        vec![
            os("rev-parse"),
            os("--path-format=absolute"),
            os("--git-common-dir"),
        ],
        true,
        None,
    )?;
    let common = std::str::from_utf8(&common.stdout)
        .ok()
        .and_then(|s| s.strip_suffix('\n'))
        .filter(|s| Path::new(s).is_absolute() && !s.contains(['\n', '\r']))
        .ok_or_else(|| Error::local("commit source common directory is invalid"))?;
    let format = reader.checked(
        "object-format",
        vec![os("rev-parse"), os("--show-object-format")],
        true,
        None,
    )?;
    if format.stdout != b"sha1\n" {
        return Err(Error::refused(
            "commit ancestry requires SHA1 Git object format",
        ));
    }
    let mut init = vec![os("init"), os("--quiet"), os("--bare"), os("--template=")];
    let view = reader.view.take();
    if let Some(view) = &view {
        init.push(view.as_os_str().to_owned());
    }
    reader.checked("init", init, false, None)?;
    reader.view = view;
    // Only immutable objects are shared. Source shallow, grafts, replacements,
    // refs, commit-graph accelerators and configuration never enter this view.
    super::lend_objects(children, run, reader.view.as_deref(), common, false)?;
    reader.read(request.commits())?;
    let mut queries = Vec::<Value>::new();
    for query in request.queries() {
        let contained = reader.contained(query)?;
        queries.push(json!({"ancestor":query.ancestor, "descendant":query.descendant, "contained":contained}));
    }
    let value = json!({"schema":1,"scope":"commit-parent-ancestry-only-v1","commits":request.commits(),"queries":queries,
        "acquired_commits":reader.acquired,"api_body_bytes":reader.api_bytes,"refs_written":false,"checkout_complete":false});
    Ok(ChildOut {
        code: Some(0),
        stdout: aterm_json::to_vec(&value).map_err(|error| Error::protocol(error.to_string()))?,
        stderr: Vec::new(),
    })
}

/// A short, original-parent witness that a fresh remote tip already owns a
/// commit's whole closure. Hash every body used in the witness. Distant roots
/// use the full graph admission; this path never acquires anything over HTTP.
pub(super) fn witnessed_local_path(
    children: &Children<'_>,
    run: &GitRun,
    source: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let mut reader = Reader {
        children,
        run,
        source: source.to_path_buf(),
        view: children.scratch_git.clone(),
        acquired: 0,
        api_bytes: 0,
        serial: 0,
        how: How::read(),
        local_only: true,
    };
    reader.read(&[ancestor.to_owned()])?;
    let mut pending = vec![descendant.to_owned()];
    let mut seen = BTreeSet::new();
    while !pending.is_empty() {
        if seen.len() + pending.len() > 256 {
            return Ok(false);
        }
        let batch = std::mem::take(&mut pending);
        for (id, body) in batch.iter().zip(reader.read(&batch)?) {
            if id == ancestor {
                return Ok(true);
            }
            for parent in object::parents(&body)? {
                if parent == ancestor {
                    return Ok(true);
                }
                if seen.insert(parent.clone()) {
                    pending.push(parent);
                }
            }
        }
    }
    Ok(false)
}

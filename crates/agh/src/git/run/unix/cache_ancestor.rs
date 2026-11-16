// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A checked cached ancestor supplies object DATA and one negotiation tip.

use super::*;

/// Admit only a checked original ancestor's closure, never its unrelated refs
/// or dangling objects. The wanted root was independently fetched into `metadata`.
/// No cache name establishes which root the remote owns. The input cache is
/// self-contained and nonshallow; selected checkout's native private boundary
/// and every cache route stay outside the final receiver.
pub(super) fn find(
    children: &Children<'_>,
    run: &GitRun,
    cache: &Path,
    metadata: &Path,
    wanted: &str,
    full_history: bool,
) -> Result<Option<String>> {
    for suffix in [
        "objects/info/alternates",
        "objects/info/http-alternates",
        "info/grafts",
        "refs/replace",
        "shallow",
    ] {
        match std::fs::symlink_metadata(cache.join(suffix)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::local(error.to_string())),
            Ok(_) => return Ok(None),
        }
    }
    let mut original = children.clone();
    original.env.push((os("GIT_GRAFT_FILE"), os("/dev/null")));
    let step = |name: &str, args: &[OsString], cwd: Option<&Path>| {
        let key = [
            crate::fx::canon::git_request(run),
            format!("\nstep cache-ancestor-{name}").into_bytes(),
        ]
        .concat();
        original.git(args, cwd, &key).map_err(unsent)
    };
    let tip = step(
        "head",
        &[
            os("rev-parse"),
            os("--verify"),
            os("--quiet"),
            os("HEAD^{commit}"),
        ],
        Some(cache),
    )?;
    if tip.code != Some(0) && !(tip.code == Some(1) && tip.stdout.is_empty()) {
        return Err(failed(
            &run.op,
            &tip,
            children.gh.redactor(),
            &run.slug,
            false,
        ));
    }
    let identities = |out: &ChildOut| -> Result<Vec<String>> {
        let text = std::str::from_utf8(&out.stdout)
            .ok()
            .filter(|_| !out.stdout_may_be_truncated())
            .ok_or_else(|| Error::protocol("invalid cached ancestor identities"))?;
        if !text.is_empty() && !text.ends_with('\n') {
            return Err(Error::protocol("incomplete cached ancestor identity"));
        }
        text.split_terminator('\n')
            .map(|oid| {
                if crate::path::is_sha(oid) {
                    Ok(oid.to_owned())
                } else {
                    Err(Error::protocol("invalid cached ancestor identity"))
                }
            })
            .collect()
    };
    let mut candidates = identities(&tip)?;
    if tip.code == Some(0) && candidates.len() != 1 {
        return Err(Error::protocol("invalid cached HEAD identity count"));
    }
    // Work checkouts normally have unpublished HEADs. Cache refs contribute only
    // candidate identities: each must still occur in the authenticated original
    // graph and contain the requested grade's real object closure.
    for (name, args, cwd) in [
        (
            "refs",
            vec![
                os("for-each-ref"),
                os("--count=64"),
                os("--sort=-committerdate"),
                os("--format=%(objectname)"),
                os("refs/heads"),
                os("refs/remotes"),
                os("refs/agh"),
            ],
            cache,
        ),
        (
            "parents",
            vec![
                os("-c"),
                os("core.commitGraph=false"),
                os("rev-list"),
                os("--max-count=17"),
                os("--first-parent"),
                os(wanted),
            ],
            metadata,
        ),
    ] {
        let out = step(name, &args, Some(cwd))?;
        if out.code != Some(0) {
            return Err(failed(
                &run.op,
                &out,
                children.gh.redactor(),
                &run.slug,
                false,
            ));
        }
        let ids = identities(&out)?;
        if ids.len() > if name == "refs" { 64 } else { 17 }
            || (name == "parents" && ids.first().map(String::as_str) != Some(wanted))
        {
            return Err(Error::protocol("invalid bounded ancestor inventory"));
        }
        candidates.extend(ids);
    }
    let mut seen = std::collections::BTreeSet::new();
    for oid in &candidates {
        if oid == wanted || !seen.insert(oid) {
            // A partial wanted root is never promoted into a complete fetch have.
            continue;
        }
        let cached = step(
            &format!("cached-{oid}"),
            &[os("cat-file"), os("-e"), os(format!("{oid}^{{commit}}"))],
            Some(cache),
        )?;
        if cached.code == Some(1) || cached.code == Some(128) {
            continue;
        }
        if cached.code != Some(0) {
            return Err(failed(
                &run.op,
                &cached,
                children.gh.redactor(),
                &run.slug,
                false,
            ));
        }
        let present = step(
            &format!("present-{oid}"),
            &[os("cat-file"), os("-e"), os(format!("{oid}^{{commit}}"))],
            Some(metadata),
        )?;
        if present.code == Some(1) || present.code == Some(128) {
            continue;
        }
        if present.code != Some(0) {
            return Err(failed(
                &run.op,
                &present,
                children.gh.redactor(),
                &run.slug,
                false,
            ));
        }
        let ancestor = step(
            &format!("relation-{oid}"),
            &[
                os("-c"),
                os("core.commitGraph=false"),
                os("merge-base"),
                os("--is-ancestor"),
                os(oid),
                os(wanted),
            ],
            Some(metadata),
        )?;
        if ancestor.code == Some(1) {
            continue;
        }
        if ancestor.code != Some(0) {
            return Err(failed(
                &run.op,
                &ancestor,
                children.gh.redactor(),
                &run.slug,
                false,
            ));
        }
        let mut args = vec![
            os("-c"),
            os("core.commitGraph=false"),
            os("rev-list"),
            os("--objects"),
            os("--quiet"),
            os("--missing=print"),
        ];
        if !full_history {
            // Selected checkout haves need only the snapshot. The native local
            // depth-one transfer below advertises its actual private boundary.
            args.push(os("--max-count=1"));
        }
        args.push(os(oid));
        let closure = step(&format!("closure-{oid}"), &args, Some(cache))?;
        if closure.code != Some(0) {
            return Err(failed(
                &run.op,
                &closure,
                children.gh.redactor(),
                &run.slug,
                false,
            ));
        }
        if !closure.stdout.is_empty() || closure.stdout_may_be_truncated() {
            continue;
        }
        return Ok(Some(oid.to_owned()));
    }
    Ok(None)
}

/// Copy only an already admitted original identity into its owned body worker.
/// A native depth-one boundary belongs only to selected-checkout staging.
pub(super) fn copy(
    children: &Children<'_>,
    run: &GitRun,
    cache: &Path,
    body: &Path,
    oid: &str,
    full_history: bool,
) -> Result<()> {
    // Default protocol v0 upload-pack refuses unadvertised ancestors. An owned
    // DATA-only view advertises just the already admitted original identity,
    // independently of the input cache's ref names or upload-pack permissions.
    // This HEAD is never consulted as remote/source authority.
    let export = body.with_file_name("cache-ancestor-export.git");
    let mut original = children.clone();
    original.env.push((os("GIT_GRAFT_FILE"), os("/dev/null")));
    let step = |name: &str, args: &[OsString], cwd: Option<&Path>| -> Result<ChildOut> {
        let key = [
            crate::fx::canon::git_request(run),
            format!("\nstep cache-ancestor-{name}-{oid}").into_bytes(),
        ]
        .concat();
        let out = original.git(args, cwd, &key).map_err(unsent)?;
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
    step(
        "export-init",
        &[
            os("init"),
            os("--quiet"),
            os("--bare"),
            os("--template="),
            export.as_os_str().to_owned(),
        ],
        None,
    )?;
    let common = cache
        .to_str()
        .ok_or_else(|| Error::local("source cache path is not UTF-8"))?;
    lend_objects(&original, run, Some(&export), common, false)?;
    step(
        "export-head",
        &[os("update-ref"), os("--no-deref"), os("HEAD"), os(oid)],
        Some(&export),
    )?;
    // Use the same native, fsck-checked local transfer as complete source-cache
    // admission. pack-objects with a file prefix creates temporary files in its
    // source store even when the prefix names another repository; upload-pack's
    // stream keeps every write in this owned receiver instead. The exact root
    // excludes unrelated refs and dangling partial wanted commits.
    let mut args = vec![
        os("-c"),
        os("protocol.file.allow=always"),
        os("-c"),
        os("fetch.fsckObjects=true"),
        os("-c"),
        os("transfer.fsckObjects=true"),
        os("fetch"),
        os("--quiet"),
        os("--refetch"),
        os("--no-filter"),
        os("--no-tags"),
        os("--no-write-fetch-head"),
        os("--recurse-submodules=no"),
    ];
    if !full_history {
        args.push(os("--depth=1"));
    }
    args.extend([
        export.as_os_str().to_owned(),
        os(format!("+{oid}:refs/agh/cache-ancestor")),
    ]);
    step("copy", &args, Some(body))?;
    Ok(())
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Release CLI verbs: list, view, latest, newest, assets, and download.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::args::{Invocation, help_next};
use super::out::OutputOpts;
use super::project;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, FileWritten, Outcome, Receipt};
use crate::client::Github;
use crate::download::{Expect, Sink};
use crate::error::{Error, Kind, Result, Sub};
use crate::github::{How, TagObjects};
use crate::model::ReleaseObject;
use crate::policy::Access;
use crate::slug::Slug;

/// `--prerelease` → `Some(true)`, `--no-prerelease` → `Some(false)`, neither → `None`.
fn prerelease_filter(inv: &Invocation) -> Result<Option<bool>> {
    match (inv.has("--prerelease"), inv.has("--no-prerelease")) {
        (true, true) => Err(inv.usage("--prerelease and --no-prerelease contradict each other")),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

/// `--tag-regex`, compiled.
fn tag_regex(inv: &Invocation) -> Result<Option<aterm_regex::Regex>> {
    inv.flag("--tag-regex")
        .map(aterm_regex::Regex::new)
        .transpose()
        .map_err(|e| inv.usage(format!("--tag-regex: {e}")))
}

/// The release's raw JSON with `repo` set, and `latest` when `latest_id` was read.
fn release_value(release: ReleaseObject, slug: &Slug, latest_id: Option<Option<u64>>) -> Value {
    let id = release.id;
    let mut value = release.raw;
    if let Some(object) = value.as_object_mut() {
        object.insert("repo".into(), Value::from(slug.to_string()));
        if let Some(latest_id) = latest_id {
            object.insert("latest".into(), Value::from(latest_id == Some(id)));
        }
    }
    value
}

/// Reads GitHub's latest release id when the output projection includes `latest`, reusing
/// the caller's `how` so private repos remain visible.
fn latest_if_wanted(
    gh: &Github,
    slug: &Slug,
    how: &How,
    inv: &Invocation,
    noun: Noun,
) -> Result<Option<Option<u64>>> {
    if !project::wants(noun, &OutputOpts::from_global(&inv.global), "latest") {
        return Ok(None);
    }
    gh.releases(slug)
        .how(how.clone())
        .latest_id_checked()
        .map(Some)
}

/// `agh release list REPO [--drafts] [--tag-regex RE] [--prerelease|--no-prerelease]`.
///
/// # Errors
///
/// Usage for contradicting flags or a bad regex; the listing's error.
pub(crate) fn release_list(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let prerelease = prerelease_filter(inv)?;
    let regex = tag_regex(inv)?;
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let access = if inv.has("--drafts") {
        Access::WriterRead
    } else {
        Access::Read
    };
    let how = inv.how(access, named);
    let opts = inv.list_opts();
    let listing = gh
        .releases(&slug)
        .how(how.clone())
        .list(&opts.with_limit(None))?;
    let mut releases: Vec<ReleaseObject> = listing
        .items
        .into_iter()
        .filter(|release| prerelease.is_none_or(|want| release.prerelease == want))
        .filter(|release| {
            regex
                .as_ref()
                .is_none_or(|regex| regex.is_match(&release.tag_name))
        })
        .collect();
    let completeness = Completeness::of(crate::page::cut(listing.end, &mut releases, opts.limit));
    let latest = latest_if_wanted(&gh, &slug, &how, inv, Noun::ReleaseList)?;
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    receipt.pages = listing.pages;
    receipt.consistency = Some(listing.consistency);
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::ReleaseList,
            items: releases
                .into_iter()
                .map(|release| release_value(release, &slug, latest))
                .collect(),
            completeness,
        },
        receipt,
    })
}

/// How a `view`, `assets` or `download` names its release.
enum Which<'a> {
    /// `--id ID`.
    Id(u64),
    /// TAG.
    Tag(&'a str),
}

/// The release a `view`, `assets` or `download` names: by `--id`, by TAG as published, or
/// with `--include-drafts` by TAG among every release (as the writer). With the routing it
/// was read under.
fn named_release(gh: &Github, inv: &Invocation) -> Result<(Slug, How, ReleaseObject)> {
    let (slug, named) = inv.repo(gh.router())?;
    let which = match (inv.rest().first().map(String::as_str), inv.flag("--id")) {
        (None, None) => return Err(inv.usage("name the release: TAG or --id ID")),
        (Some(_), Some(_)) => return Err(inv.usage("TAG and --id contradict each other")),
        (None, Some(id)) => Which::Id(
            crate::path::digits::<u64>(id)
                .ok_or_else(|| inv.usage("--id wants a release id (digits)"))?,
        ),
        // Validate tag syntax before any request; the drafts branch lists releases first.
        (Some(tag), None) => {
            crate::path::refuse_bad_tag(tag)?;
            Which::Tag(tag)
        }
    };
    let drafts = inv.has("--include-drafts");
    let access = if drafts {
        Access::WriterRead
    } else {
        Access::Read
    };
    let how = inv.how(access, named);
    let releases = gh.releases(&slug).how(how.clone());
    let release = match which {
        Which::Id(id) => releases.get(id)?,
        Which::Tag(tag) if !drafts => releases.published_by_tag(tag)?,
        Which::Tag(tag) => match releases.objects_for_tag(tag)? {
            TagObjects::None => {
                return Err(Error::new(
                    Kind::NotFound,
                    format!("no release of {slug} has tag {tag}, drafts included"),
                )
                .with_repo(slug.to_string()));
            }
            TagObjects::One(release) => *release,
            TagObjects::Many(found) => {
                let ids: Vec<u64> = found.iter().map(|release| release.id).collect();
                return Err(Error::new(
                    Kind::Conflict,
                    format!("{} releases have tag {tag}", ids.len()),
                )
                .with_sub(Sub::AmbiguousTag { ids })
                .with_next(format!(
                    "name one: agh release view {slug} --id ID --include-drafts"
                ))
                .with_repo(slug.to_string()));
            }
        },
    };
    Ok((slug, how, release))
}

/// `agh release view REPO TAG | --id ID [--include-drafts]`.
///
/// # Errors
///
/// Usage for naming no release or two; the read's error (NotFound, Conflict
/// `AmbiguousTag`, Forbidden `no_push`).
pub(crate) fn release_view(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, how, release) = named_release(&gh, inv)?;
    let latest = latest_if_wanted(&gh, &slug, &how, inv, Noun::Release)?;
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Release,
            value: release_value(release, &slug, latest),
        },
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// `agh release latest REPO`: GitHub's own latest pointer.
///
/// # Errors
///
/// NotFound when the repository has no latest release.
pub(crate) fn release_latest(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let release = gh
        .releases(&slug)
        .how(inv.how(Access::Read, named))
        .latest()?;
    let id = release.id;
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Release,
            value: release_value(release, &slug, Some(Some(id))),
        },
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// `agh release newest REPO [--tag-regex RE] [--prerelease|--no-prerelease]`: the newest
/// published release by `created_at` that passes the filters.
///
/// # Errors
///
/// Usage for contradicting flags or a bad regex; NotFound when none matches.
pub(crate) fn release_newest(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let prerelease = prerelease_filter(inv)?;
    let regex = tag_regex(inv)?;
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let how = inv.how(Access::Read, named);
    let Some(release) = gh
        .releases(&slug)
        .how(how.clone())
        .newest(prerelease, regex.as_ref())?
    else {
        return Err(Error::new(
            Kind::NotFound,
            format!("no published release of {slug} matches"),
        )
        .with_repo(slug.to_string()));
    };
    let latest = latest_if_wanted(&gh, &slug, &how, inv, Noun::Release)?;
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Release,
            value: release_value(release, &slug, latest),
        },
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// `agh release assets REPO TAG | --id ID [--include-drafts]`: the release's assets, sorted
/// by name, `--limit` after the sort.
///
/// # Errors
///
/// As `release view`.
pub(crate) fn release_assets(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, _how, release) = named_release(&gh, inv)?;
    let mut assets = release.assets;
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    let completeness = Completeness::of(crate::page::cut(
        crate::page::End::Complete,
        &mut assets,
        inv.global.limit,
    ));
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::Asset,
            items: assets.into_iter().map(|asset| asset.raw).collect(),
            completeness,
        },
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// Where `release download` puts its bytes.
enum Out {
    /// `--output -`.
    Stdout,
    /// `--output F`.
    File(PathBuf),
    /// `--dir D` (default `.`).
    Dir(PathBuf),
}

/// stdout under `--output -`: the bytes it took, and whether its reader went away.
struct Taken<'a> {
    inner: &'a mut dyn std::io::Write,
    written: u64,
    gone: bool,
}

impl Taken<'_> {
    /// `result`, noting a gone reader ([`aterm_uds::stdout::reader_gone`]).
    fn noted<T>(&mut self, result: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(e) = &result
            && aterm_uds::stdout::reader_gone(e)
        {
            self.gone = true;
        }
        result
    }
}

impl std::io::Write for Taken<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let wrote = self.inner.write(buf);
        let n = self.noted(wrote)?;
        self.written = self
            .written
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let flushed = self.inner.flush();
        self.noted(flushed)
    }
}

/// `release download`'s argument checks, all before any request.
fn download_target(ctx: &Ctx, inv: &Invocation, names: &[&str]) -> Result<Out> {
    if names.is_empty() {
        return Err(inv.usage("--asset NAME is required"));
    }
    for (i, name) in names.iter().enumerate() {
        if names[..i].contains(name) {
            return Err(inv.usage(format!("--asset {name} given twice")));
        }
    }
    let out = output(ctx, inv, names)?;
    // Validate destination paths before making any network request.
    let dests: Vec<PathBuf> = match &out {
        Out::Stdout => Vec::new(),
        Out::File(file) => vec![file.clone()],
        Out::Dir(dir) => {
            for name in names {
                if !crate::download::plain_file_name(name) {
                    return Err(inv.usage(format!(
                        "--asset {name} is not a plain file name, so --dir cannot hold it"
                    )));
                }
                if crate::download::is_part_name(Path::new(name)) {
                    return Err(inv.usage(format!(
                        "--asset {name} ends in {}, the suffix of agh's own part files, so \
                             --dir cannot hold it",
                        crate::download::PART_SUFFIX
                    )));
                }
            }
            names.iter().map(|name| dir.join(name)).collect()
        }
    };
    for dest in &dests {
        crate::download::refuse_non_file(dest).map_err(|e| {
            if e.kind == Kind::Usage {
                e.with_next(help_next(&inv.path))
            } else {
                e
            }
        })?;
    }
    Ok(out)
}

/// `--dir`, `--output F` or `--output -`, judged.
fn output(ctx: &Ctx, inv: &Invocation, names: &[&str]) -> Result<Out> {
    match (inv.flag("--dir"), inv.flag("--output")) {
        (Some(_), Some(_)) => Err(inv.usage("--dir and --output contradict each other")),
        (None, Some("-")) if ctx.stdout_is_terminal => Err(inv.usage(
            "--output - writes the asset's bytes to stdout, which is a terminal: redirect it",
        )),
        (None, Some("-")) => Ok(Out::Stdout),
        (None, Some(_)) if names.len() > 1 => {
            Err(inv.usage("--output names one file: give one --asset, or use --dir"))
        }
        (None, Some(file)) if Path::new(file).file_name().is_none() => Err(inv.usage(format!(
            "--output {file:?} names no file: give a file name, or - for stdout"
        ))),
        (None, Some(file)) if crate::download::is_part_name(Path::new(file)) => {
            Err(inv.usage(format!(
                "--output {file} ends in {}, the suffix of agh's own part files",
                crate::download::PART_SUFFIX
            )))
        }
        (None, Some(file)) => Ok(Out::File(PathBuf::from(file))),
        (dir, None) => {
            let dir = dir.unwrap_or(".");
            if Path::new(dir).is_dir() {
                Ok(Out::Dir(PathBuf::from(dir)))
            } else {
                Err(inv.usage(format!("--dir {dir} is not a directory")))
            }
        }
    }
}

/// `agh release download REPO TAG | --id ID --asset NAME… [--dir D | --output F |
/// --output -] [--clobber] [--include-drafts]`.
///
/// # Errors
///
/// Usage for the argument checks; NotFound for an asset name the release lacks;
/// `download::fetch`'s errors.
pub(crate) fn release_download(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let names = inv.flag_all("--asset");
    let target = download_target(ctx, inv, &names)?;
    let gh = ctx.github()?;
    let (slug, how, release) = named_release(&gh, inv)?;
    let mut assets = Vec::with_capacity(names.len());
    for name in &names {
        let Some(asset) = release.assets.iter().find(|asset| asset.name == *name) else {
            return Err(Error::new(
                Kind::NotFound,
                format!("release {} has no asset named {name}", release.tag_name),
            )
            .with_next(format!("agh release assets {slug} {}", release.tag_name))
            .with_repo(slug.to_string()));
        };
        assets.push(asset);
    }
    let clobber = inv.has("--clobber");
    let mut rows = Vec::with_capacity(assets.len());
    let mut stdout = Taken {
        inner: &mut *ctx.stdout,
        written: 0,
        gone: false,
    };
    for asset in assets {
        let expect = Expect::from_asset(asset).map_err(|e| e.with_repo(slug.to_string()))?;
        let sink = match &target {
            Out::Stdout => Sink::Writer(&mut stdout),
            Out::File(path) => Sink::File {
                path: path.clone(),
                clobber,
            },
            Out::Dir(dir) => Sink::File {
                path: dir.join(&asset.name),
                clobber,
            },
        };
        let done = match gh
            .releases(&slug)
            .how(how.clone())
            .download(asset, sink, expect)
        {
            Ok(done) => done,
            // If stdout's reader closed early, end the stream cleanly with exit 0.
            Err(_) if stdout.gone => break,
            Err(error) => return Err(error),
        };
        if let Some(path) = done.path {
            rows.push(FileWritten {
                path,
                bytes: done.bytes,
                sha256: done.sha256,
                asset_id: done.asset_id,
                written: done.written,
                digest: done.digest,
            });
        }
    }
    let written = stdout.written;
    let answer = match target {
        Out::Stdout => Answer::Streamed { written },
        Out::File(_) | Out::Dir(_) => Answer::Files(rows),
    };
    Ok(Outcome {
        answer,
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

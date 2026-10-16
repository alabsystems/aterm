// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`GhCli`]: the production [`ReleaseHost`], the one place `gh` runs.
//!
//! * `GH_TOKEN` comes from the identity's token file. A read falls back to gh's own login
//!   when the file is absent (a dry run without the channel's token still reads the public
//!   channel); a write without a non-empty token file is refused before gh runs, so gh's
//!   keychain login never writes. A token in this process's environment is never
//!   inherited, so whoever runs the producer cannot pick the identity by accident.
//! * [`ReleaseHost::create`] always passes `--prerelease --latest=false` (a data release
//!   never holds `/releases/latest`); [`ReleaseHost::upload`] passes `--clobber` only for
//!   [`Replace::Clobber`]; a download always passes `--clobber`, because a retried call
//!   (`channel::download_retried`) meets the files an interrupted one left.
//! * [`ReleaseHost::probe_public`] / [`ReleaseHost::fetch_public`] ask the download host
//!   anonymously (`aterm_update_core::head_no_redirect` / `download_to`), as a client sees
//!   an asset.
//! * [`classify`] reads a failure: gh's HTTP 404 (and `release not found`, `no assets
//!   match`) is [`HostError::NotFound`]; a name that already exists is
//!   [`HostError::Refused`] (a racing publisher, never retried); any other failure is
//!   [`HostError::Unreachable`], never "absent".

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

/// The pause between attempts at a call GitHub did not answer: its release-asset
/// endpoint's intermittent HTTP 500 is usually still answering a moment later.
pub const RETRY_PAUSE: Duration = Duration::from_secs(5);

/// What `gh` prints per release: tag, draft, prerelease, and the asset names joined by `/`
/// (no asset name carries one), tab-separated.
const RELEASE_FIELDS: &str =
    "[.tag_name, (.draft|tostring), (.prerelease|tostring), ([.assets[].name]|join(\"/\"))] | @tsv";
const VIEW_FIELDS: &str = "[.tagName, (.isDraft|tostring), (.isPrerelease|tostring), ([.assets[].name]|join(\"/\"))] | @tsv";

/// `gh`, as one identity.
#[derive(Clone, Debug)]
pub struct GhCli {
    /// The token file this identity reads `GH_TOKEN` from; `None` uses gh's own login.
    pub token_file: Option<PathBuf>,
}

impl GhCli {
    /// `gh` speaking as the token in `token_file`, or as its own login.
    #[must_use]
    pub fn new(token_file: Option<PathBuf>) -> Self {
        Self { token_file }
    }

    /// `gh <args>` reading: the token when there is one, else gh's own login.
    fn read(&self, args: &[OsString]) -> Result<String, HostError> {
        let token = self
            .token_file
            .as_ref()
            .filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0));
        Self::gh(token.map(PathBuf::as_path), args)
    }

    /// `gh <args>` writing: as the configured identity's token, never gh's own login.
    fn write(&self, args: &[OsString]) -> Result<String, HostError> {
        let Some(file) = self.token_file.as_deref() else {
            return Err(HostError::Refused(
                "no token file is configured for this owner (it was absent or empty when the \
                 run started), and a write never falls back to gh's own login"
                    .to_string(),
            ));
        };
        Self::gh(Some(file), args)
    }

    fn gh(token_file: Option<&Path>, args: &[OsString]) -> Result<String, HostError> {
        let mut cmd = aterm_uds::spawnfd::command("gh");
        cmd.args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("GITHUB_ENTERPRISE_TOKEN");
        match token_file {
            Some(file) => {
                let token = std::fs::read_to_string(file).map_err(|e| {
                    HostError::Refused(format!(
                        "cannot read the token file {}: {e}",
                        file.display()
                    ))
                })?;
                let token = token.trim();
                if token.is_empty() {
                    return Err(HostError::Refused(format!(
                        "the token file {} is empty",
                        file.display()
                    )));
                }
                cmd.env("GH_TOKEN", token);
            }
            None => {
                cmd.env_remove("GH_TOKEN");
            }
        }
        let out = cmd
            .output()
            .map_err(|e| HostError::Unreachable(format!("gh could not be started: {e}")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(classify(&String::from_utf8_lossy(&out.stderr)))
        }
    }
}

/// What a failed `gh` call's stderr means.
#[must_use]
pub fn classify(stderr: &str) -> HostError {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh failed and said nothing")
        .to_string();
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("http 404")
        || lower.contains("release not found")
        || lower.contains("no assets match")
        || lower.contains("could not resolve to a repository")
    {
        HostError::NotFound
    } else if lower.contains("already exists") || lower.contains("already_exists") {
        HostError::Refused(first)
    } else {
        HostError::Unreachable(first)
    }
}

/// One `@tsv` line of [`RELEASE_FIELDS`] / [`VIEW_FIELDS`].
#[must_use]
pub fn parse_release_line(line: &str) -> Option<Release> {
    let mut fields = line.split('\t');
    let tag = fields.next()?.to_string();
    let flag = |f: Option<&str>| match f {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let draft = flag(fields.next())?;
    let prerelease = flag(fields.next())?;
    let assets = fields
        .next()
        .unwrap_or("")
        .split('/')
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();
    if tag.is_empty() || fields.next().is_some() {
        return None;
    }
    Some(Release {
        tag,
        draft,
        prerelease,
        assets,
    })
}

fn os(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

/// `gh release download`'s `-p` takes a glob: an asset name is matched literally.
fn literal_pattern(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The argv of every release list (drafts included).
#[must_use]
pub fn releases_args(slug: &Slug) -> Vec<OsString> {
    os(&[
        "api",
        "--paginate",
        &format!("repos/{slug}/releases?per_page=100"),
        "--jq",
        &format!(".[] | {RELEASE_FIELDS}"),
    ])
}

/// The argv of one release's view (drafts are found too).
#[must_use]
pub fn view_args(slug: &Slug, tag: &str) -> Vec<OsString> {
    os(&[
        "release",
        "view",
        tag,
        "-R",
        &slug.to_string(),
        "--json",
        "tagName,isDraft,isPrerelease,assets",
        "--jq",
        VIEW_FIELDS,
    ])
}

/// The argv of a download: always `--clobber`, every asset named literally.
#[must_use]
pub fn download_args(slug: &Slug, tag: &str, assets: &[&str], dir: &Path) -> Vec<OsString> {
    let mut v = os(&["release", "download", tag, "-R", &slug.to_string(), "-D"]);
    v.push(dir.as_os_str().to_owned());
    v.push("--clobber".into());
    for a in assets {
        v.push("-p".into());
        v.push(literal_pattern(a).into());
    }
    v
}

/// The argv of a create: a prerelease that never takes `latest`.
#[must_use]
pub fn create_args(
    slug: &Slug,
    tag: &str,
    title: &str,
    notes: &str,
    files: &[PathBuf],
) -> Vec<OsString> {
    let mut v = os(&[
        "release",
        "create",
        tag,
        "-R",
        &slug.to_string(),
        "--title",
        title,
        "--notes",
        notes,
        "--prerelease",
        "--latest=false",
    ]);
    v.extend(files.iter().map(|f| f.as_os_str().to_owned()));
    v
}

/// The argv of an upload: `--clobber` only when asked for.
#[must_use]
pub fn upload_args(slug: &Slug, tag: &str, files: &[PathBuf], replace: Replace) -> Vec<OsString> {
    let mut v = os(&["release", "upload", tag, "-R", &slug.to_string()]);
    v.extend(files.iter().map(|f| f.as_os_str().to_owned()));
    if replace == Replace::Clobber {
        v.push("--clobber".into());
    }
    v
}

/// The argv that publishes a draft, keeping it a prerelease.
#[must_use]
pub fn publish_draft_args(slug: &Slug, tag: &str) -> Vec<OsString> {
    os(&[
        "release",
        "edit",
        tag,
        "-R",
        &slug.to_string(),
        "--draft=false",
        "--prerelease",
    ])
}

/// What a download-host status means for one asset: 200, or the 302 into the CDN, is
/// published; 404 is not; anything else says neither.
#[must_use]
pub fn probe_of(code: u16) -> Probe {
    match code {
        200 | 302 => Probe::Present,
        404 => Probe::Absent,
        other => Probe::Unknown(format!("the download host answered HTTP {other}")),
    }
}

impl ReleaseHost for GhCli {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        let out = self.read(&releases_args(slug))?;
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                parse_release_line(l).ok_or_else(|| {
                    HostError::Unreachable(format!("gh listed a release this cannot read: {l}"))
                })
            })
            .collect()
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        let out = self.read(&view_args(slug, tag))?;
        let line = out.lines().next().unwrap_or("");
        parse_release_line(line).ok_or_else(|| {
            HostError::Unreachable(format!(
                "gh answered something this cannot read for {slug} {tag}: {line}"
            ))
        })
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Refused(format!("mkdir {}: {e}", dir.display())))?;
        self.read(&download_args(slug, tag, assets, dir))?;
        // gh exits 0 when a pattern matched nothing it was not asked to require; every
        // named asset must now be a file.
        match assets.iter().find(|a| !dir.join(a).is_file()) {
            Some(_) => Err(HostError::NotFound),
            None => Ok(()),
        }
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.write(&create_args(slug, tag, title, notes, files))
            .map(|_| ())
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        self.write(&upload_args(slug, tag, files, replace))
            .map(|_| ())
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        self.write(&publish_draft_args(slug, tag)).map(|_| ())
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        match aterm_update_core::head_no_redirect(&slug.download_url(tag, asset)) {
            Ok(answer) => probe_of(answer.code),
            Err(e) => Probe::Unknown(format!("no answer from the download host: {e:?}")),
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(format!("mkdir {}: {e}", parent.display()));
        }
        match aterm_update_core::download_to(&slug.download_url(tag, asset), dest, cap) {
            Ok(()) => Probe::Present,
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                if aterm_update_core::download_error_is_not_found(&e) {
                    Probe::Absent
                } else {
                    Probe::Unknown(e)
                }
            }
        }
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        let mut args = os(&["repo", "clone", &slug.to_string()]);
        args.push(dest.as_os_str().to_owned());
        args.extend(os(&["--", "--quiet"]));
        self.read(&args).map(|_| ())
    }

    fn retry_pause(&self) -> Duration {
        RETRY_PAUSE
    }

    /// `git fetch` of the slug's https URL (never the clone's `origin`, which may name an
    /// ssh remote and its ambient key), with every configured helper cleared and gh's
    /// helper reading `GH_TOKEN` from this identity's token file.
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        let mut cmd = aterm_uds::spawnfd::command("git");
        cmd.arg("-C")
            .arg(dir)
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
                "fetch",
                "--quiet",
                "--tags",
                "--force",
            ])
            .arg(format!("https://github.com/{slug}.git"))
            .arg("+refs/heads/*:refs/remotes/origin/*")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
        if let Some(file) = &self.token_file {
            let token = std::fs::read_to_string(file)
                .map_err(|e| HostError::Refused(format!("read {}: {e}", file.display())))?;
            cmd.env("GH_TOKEN", token.trim());
        }
        let out = cmd
            .output()
            .map_err(|e| HostError::Unreachable(format!("spawn git: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(HostError::Unreachable(format!(
            "git fetch of {slug}: {}",
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        )))
    }
}

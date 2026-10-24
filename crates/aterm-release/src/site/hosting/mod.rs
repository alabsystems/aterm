// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship site`'s deploy, over Firebase Hosting's REST API (v1beta1), as `firebase deploy
//! --only hosting` makes one, with no node and no firebase-tools:
//!
//! 1. an access token from the site's service-account key (an OAuth2 JWT-bearer grant,
//!    [`key`]), at the one token endpoint admitted ([`rest::Origins::admit`]);
//! 2. `sites.versions.create` with firebase.json's hosting block as its `ServingConfig`
//!    ([`config`]);
//! 3. `versions.populateFiles` with the SHA-256 of each file's gzip member ([`files`]),
//!    then an upload of only the members Hosting asks for, to its `uploadUrl`;
//! 4. `versions.patch` to `FINALIZED`;
//! 5. `sites.releases.create`, the one call that changes what the live site serves.
//!
//! A failure before the last call leaves the live site on its previous release.

pub mod config;
pub mod files;
pub mod key;
pub mod rest;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use aterm_json::{Map, Value};

pub use config::Hosting;
pub use files::File;
pub use key::Key;
pub use rest::{Body, Client, Origins, Unanswered};

/// How many files one `populateFiles` call names (firebase-tools' batch).
const POPULATE_BATCH: usize = 1000;

/// The longest release message Hosting keeps.
const MESSAGE_MAX: usize = 512;

/// What one deploy publishes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The Hosting site, when settings or firebase.json name it; else the key's project.
    pub site: Option<String>,
    /// The version's `ServingConfig`.
    pub config: Value,
    /// Every file, sorted by path.
    pub files: Vec<File>,
}

impl Plan {
    /// The plan for the site checkout `site_dir`: `hosting`'s config, and `public/`'s files
    /// that `hosting.ignore` leaves, with `overlay` (by path under `public/`) laid over them.
    /// # Errors
    /// An ignore pattern this deploy cannot match, or a file it cannot list or read.
    pub fn of(
        site_dir: &Path,
        hosting: &Hosting,
        site: Option<String>,
        overlay: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Plan, String> {
        let ignore = files::patterns(&hosting.ignore)?;
        let listed = files::list(&site_dir.join(config::PUBLIC), &ignore, overlay)?;
        Ok(Plan {
            site,
            config: hosting.config.clone(),
            files: files::files(listed),
        })
    }

    /// The files' bytes, and their gzip members' bytes.
    #[must_use]
    pub fn bytes(&self) -> (u64, u64) {
        self.files.iter().fold((0, 0), |(raw, gz), f| {
            (raw + f.bytes, gz + f.gz.len() as u64)
        })
    }

    /// The plan, said line by line: the site, the config's keys, and each file's path, size,
    /// member size and hash.
    pub fn say(&self, out: &mut dyn Write) {
        let site = self
            .site
            .as_deref()
            .unwrap_or("(the service-account key's project_id)");
        let keys: Vec<&str> = self
            .config
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let (raw, gz) = self.bytes();
        let _ = writeln!(
            out,
            "  hosting plan: site {site}; config {}; {} files, {raw} bytes ({gz} gzipped)",
            if keys.is_empty() {
                "(none)".to_string()
            } else {
                keys.join(", ")
            },
            self.files.len()
        );
        for f in &self.files {
            let _ = writeln!(
                out,
                "    {} {} bytes, {} gzipped, sha256 {}",
                f.path,
                f.bytes,
                f.gz.len(),
                f.hash
            );
        }
        let _ = writeln!(
            out,
            "  hosting plan: populateFiles names all {}; Hosting asks only for the hashes it \
             does not hold, and only those are uploaded",
            self.files.len()
        );
    }
}

/// Why a deploy published nothing new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failed {
    /// No usable credential: no key named, a key that cannot be read or parsed, a token
    /// endpoint not admitted, or a key Google or Hosting refused. Nothing was published.
    Credential(String),
    /// Hosting has not released anything: the live site serves its previous release. When
    /// `version` is named, it was created and left unreleased.
    NotReleased {
        why: String,
        version: Option<String>,
    },
    /// `releases.create` was sent for the finalized `version` and did not answer success:
    /// whether the live site switched to it is unknown until the live check reads it. (A
    /// `releases.create` that was never sent is [`Failed::NotReleased`].)
    Release { why: String, version: String },
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credential(why) => write!(f, "{why} — nothing was published"),
            Self::NotReleased { why, version } => {
                write!(
                    f,
                    "{why} — nothing was released, so the live site still serves its previous release"
                )?;
                match version {
                    Some(v) => write!(f, " ({v} was left unreleased)"),
                    None => Ok(()),
                }
            }
            Self::Release { why, version } => write!(
                f,
                "releasing {version} failed: {why} — the version is finalized but whether the \
                 live site switched to it is unknown"
            ),
        }
    }
}

/// What a deploy released.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Released {
    /// The release's name, `sites/{site}/releases/{id}`.
    pub release: String,
    /// The version it serves, `sites/{site}/versions/{id}`.
    pub version: String,
    /// How many files the version holds.
    pub files: usize,
    /// How many members were uploaded (the rest Hosting already held).
    pub uploaded: usize,
}

/// The Hosting site a deploy targets: firebase.json's `hosting.site` (`in_config`), as
/// firebase-tools deploys to it, else `configured` (`FIREBASE_SITE`, else publish/site.toml's
/// `firebase_site`); neither leaves it to the key's project.
/// # Errors
/// Both name a site and the two differ: neither is dropped silently.
pub fn site(in_config: Option<&str>, configured: Option<&str>) -> Result<Option<String>, String> {
    match (in_config, configured) {
        (Some(a), Some(b)) if a != b => Err(format!(
            "firebase.json's hosting.site is {a:?} but FIREBASE_SITE (or publish/site.toml's \
             firebase_site) is {b:?}: name one Hosting site, or make the two agree"
        )),
        (a, b) => Ok(a.or(b).map(str::to_string)),
    }
}

/// Whether `id` can be a Hosting site id (lowercase letters, digits and hyphens), and so
/// sit in a URL path as it is.
fn site_id(id: &str) -> Result<&str, String> {
    let ok = !id.is_empty()
        && id.len() <= 63
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(id)
    } else {
        Err(format!(
            "{id:?} is not a Hosting site id (lowercase letters, digits and hyphens)"
        ))
    }
}

/// `text` for a URL query: every byte but RFC 3986's unreserved ones percent-encoded.
fn query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A Hosting answer that refused for the credential (401, 403) is [`Failed::Credential`];
/// any other failure is [`Failed::NotReleased`] with `version`.
fn refused(answer: &rest::Answer, doing: &str, version: Option<&str>) -> Failed {
    let why = format!("{doing}: {}", answer.said());
    if matches!(answer.status, 401 | 403) {
        Failed::Credential(format!("Hosting refused the service-account key — {why}"))
    } else {
        Failed::NotReleased {
            why,
            version: version.map(str::to_string),
        }
    }
}

/// Deploy `plan` with `key` through `client` (now as `now`, Unix seconds), releasing it
/// with `message`, each step said to `out`.
/// # Errors
/// [`Failed`]: a credential refused; any step before the release failing, or the release's
/// request never going out (nothing released); or the release itself failing once sent.
#[allow(clippy::too_many_lines)]
pub fn deploy(
    client: &Client,
    key: &Key,
    plan: &Plan,
    message: &str,
    now: u64,
    out: &mut dyn Write,
) -> Result<Released, Failed> {
    let not_released = |why: String, version: Option<&str>| Failed::NotReleased {
        why,
        version: version.map(str::to_string),
    };
    let site = plan
        .site
        .clone()
        .or_else(|| key.project.clone())
        .ok_or_else(|| {
            not_released(
                "no Hosting site is named (firebase.json's hosting.site, FIREBASE_SITE, \
                 publish/site.toml's firebase_site, or the key's project_id)"
                    .into(),
                None,
            )
        })?;
    let site = site_id(&site)
        .map_err(|why| not_released(why, None))?
        .to_string();
    let bearer = client.token(key, now)?;
    let _ = writeln!(out, "  hosting: {site}: token issued to {}", key.email);

    let created = client
        .call(
            "POST",
            &client.api(&format!("v1beta1/sites/{site}/versions")),
            Body::Json(&aterm_json::json!({
                "config": plan.config.clone(),
                "labels": {"deployment-tool": "aterm-ship-site"},
            })),
            &bearer,
        )
        .map_err(|why| not_released(format!("creating a version: {why}"), None))?;
    if !(200..300).contains(&created.status) {
        return Err(refused(&created, "creating a version", None));
    }
    let prefix = format!("sites/{site}/versions/");
    let version = created
        .json()
        .ok()
        .and_then(|doc| doc.get("name").and_then(Value::as_str).map(str::to_string))
        .filter(|name| {
            name.strip_prefix(&prefix).is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        })
        .ok_or_else(|| {
            not_released(
                format!("creating a version: Hosting answered no version of {site}"),
                None,
            )
        })?;
    let _ = writeln!(out, "  hosting: created {version}");
    let v = Some(version.as_str());

    let mut required: BTreeSet<String> = BTreeSet::new();
    let mut upload_url: Option<String> = None;
    for batch in plan.files.chunks(POPULATE_BATCH) {
        let named: Map = batch
            .iter()
            .map(|f| (f.path.clone(), Value::String(f.hash.clone())))
            .collect();
        let hashes: BTreeSet<&str> = batch.iter().map(|f| f.hash.as_str()).collect();
        let answer = client
            .call(
                "POST",
                &client.api(&format!("v1beta1/{version}:populateFiles")),
                Body::Json(&aterm_json::json!({"files": Value::Object(named)})),
                &bearer,
            )
            .map_err(|why| not_released(format!("naming the files: {why}"), v))?;
        let doc = answer
            .json()
            .map_err(|_| refused(&answer, "naming the files", v))?;
        let asked = match doc.get("uploadRequiredHashes") {
            None => Vec::new(),
            Some(Value::Array(asked)) => asked.clone(),
            Some(_) => {
                return Err(not_released(
                    "naming the files: uploadRequiredHashes is not a list".into(),
                    v,
                ));
            }
        };
        for hash in asked {
            match hash.as_str() {
                Some(h) if hashes.contains(h) => {
                    required.insert(h.to_string());
                }
                _ => {
                    return Err(not_released(
                        format!(
                            "naming the files: Hosting asked for {hash}, a file this deploy did not name"
                        ),
                        v,
                    ));
                }
            }
        }
        if let Some(url) = doc.get("uploadUrl").and_then(Value::as_str) {
            if !Origins::on(&client.origins().upload, url) {
                return Err(not_released(
                    format!(
                        "naming the files: Hosting named the upload URL {url}, off {}; the \
                         token goes nowhere else",
                        client.origins().upload
                    ),
                    v,
                ));
            }
            upload_url = Some(url.trim_end_matches('/').to_string());
        }
    }
    let mut uploads: Vec<&File> = Vec::new();
    let mut queued: BTreeSet<&str> = BTreeSet::new();
    for f in &plan.files {
        if required.contains(&f.hash) && queued.insert(&f.hash) {
            uploads.push(f);
        }
    }
    let upload_bytes: u64 = uploads.iter().map(|f| f.gz.len() as u64).sum();
    let _ = writeln!(
        out,
        "  hosting: {} files; Hosting holds {} of them; uploading {} ({upload_bytes} bytes)",
        plan.files.len(),
        plan.files.len()
            - plan
                .files
                .iter()
                .filter(|f| required.contains(&f.hash))
                .count(),
        uploads.len()
    );
    if !uploads.is_empty() {
        let base = upload_url.ok_or_else(|| {
            not_released(
                "naming the files: Hosting asked for uploads but named no uploadUrl".into(),
                v,
            )
        })?;
        for f in &uploads {
            let doing = format!("uploading {} ({})", f.path, f.hash);
            let answer = client
                .call(
                    "POST",
                    &format!("{base}/{}", f.hash),
                    Body::Bytes(&f.gz),
                    &bearer,
                )
                .map_err(|why| not_released(format!("{doing}: {why}"), v))?;
            if answer.status != 200 {
                return Err(refused(&answer, &doing, v));
            }
            let _ = writeln!(out, "  hosting: uploaded {} ({} bytes)", f.path, f.gz.len());
        }
    }

    let finalized = client
        .call(
            "PATCH",
            &client.api(&format!("v1beta1/{version}?updateMask=status")),
            Body::Json(&aterm_json::json!({"status": "FINALIZED"})),
            &bearer,
        )
        .map_err(|why| not_released(format!("finalizing the version: {why}"), v))?;
    let status = finalized
        .json()
        .map_err(|_| refused(&finalized, "finalizing the version", v))?;
    if status.get("status").and_then(Value::as_str) != Some("FINALIZED") {
        return Err(not_released(
            "finalizing the version: Hosting did not answer FINALIZED".into(),
            v,
        ));
    }
    let _ = writeln!(out, "  hosting: finalized {version}");

    let message: String = message.chars().take(MESSAGE_MAX).collect();
    let release_failed = |why: String| Failed::Release {
        why,
        version: version.clone(),
    };
    let released = client
        .call(
            "POST",
            &client.api(&format!(
                "v1beta1/sites/{site}/releases?versionName={}",
                query(&version)
            )),
            Body::Json(&aterm_json::json!({"message": message})),
            &bearer,
        )
        .map_err(|e| match e {
            Unanswered::Unsent(why) => not_released(format!("releasing {version}: {why}"), v),
            Unanswered::Lost(why) => release_failed(why),
        })?;
    let doc = released
        .json()
        .map_err(|_| release_failed(released.said()))?;
    let release = doc
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| n.starts_with(&format!("sites/{site}/releases/")))
        .ok_or_else(|| release_failed("Hosting answered no release name".into()))?
        .to_string();
    let _ = writeln!(out, "  hosting: released {version} as {release}");
    Ok(Released {
        release,
        version,
        files: plan.files.len(),
        uploaded: uploads.len(),
    })
}

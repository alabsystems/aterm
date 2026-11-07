// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed operator GitHub reads through agh. This boundary never runs a caller's
//! Git argv, synthesizes credentials or identity, or starts a remote write.
//! An origin refresh records only its admitted main in the checkout's tracking
//! ref by expected-old CAS; other reads preserve source refs. Recorded routing
//! loads the identity already configured.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agh::git::{FetchBatch, FetchRow, GitRun};
use agh::policy::{Named, Router, Tier};
use agh::{Github, GithubBuilder, Slug};

const CALLER: &str = "aterm-release";
const BUDGET: Duration = Duration::from_secs(120);
// GitOut keeps the final 64 KiB, at a UTF-8 boundary. A near-limit answer may
// therefore be incomplete and cannot establish that a ref was absent.
const COMPLETE_LIMIT: usize = 64 * 1024 - 4;
const ORIGIN_MAIN: &str = "refs/remotes/origin/main";
const NULL_OID: &str = "0000000000000000000000000000000000000000";

pub(crate) struct FetchedOrigin {
    pub(crate) main: String,
    pub(crate) tags: String,
}

pub(crate) struct Remote {
    client: Github,
    slug: Slug,
    repo: PathBuf,
    named: Named,
}

/// Read recorded vault adoption and explicit agh configuration. No credential
/// pointer, owner row or publication grant is synthesized by the cutter.
fn configured(dirs: &agh::config::Dirs) -> Result<Github, String> {
    let env = Arc::new(agh::identity::ProcessEnv);
    let base = GithubBuilder::new(Router::empty())
        .transport(Arc::new(agh::wire::http::HttpTransport::new(env.clone())))
        .env(env)
        .fx(Arc::new(agh::Fx::live()))
        .caller(CALLER)
        .journal(dirs.clone())
        .rate_book(agh::rate::RateBook::at(dirs.rate()))
        .refuse_anonymous_api(true)
        .build();
    agh::routing::view(dirs, base.env())
        .and_then(|routing| routing.bind(&base))
        .map_err(|error| base.redactor().apply(&error.why))
}

fn origin_slug(repo: &Path) -> Result<Slug, String> {
    let mut command = crate::ledger::object_git(repo);
    command.args(["remote", "get-url", "origin"]);
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut command,
        std::time::Instant::now() + BUDGET,
    )
    .map_err(|error| format!("cannot read this source's origin: {error}"))?;
    if !out.status.success() {
        return Err("cannot read this source's origin".into());
    }
    let url = std::str::from_utf8(&out.stdout).map_err(|_| "origin is not UTF-8")?;
    let slug = crate::publish::github_slug_from_remote_url(url.trim())
        .map_err(|_| "origin must name one unambiguous GitHub OWNER/REPO".to_owned())?;
    Slug::parse(&slug).map_err(|error| error.why.clone())
}

/// The cutter embeds only the existing Git credential-helper grammar, never a
/// second general operator CLI. Git appends exactly one protocol operation.
pub(crate) fn helper_arguments(
    args: &[std::ffi::OsString],
) -> Result<Option<Vec<std::ffi::OsString>>, &'static str> {
    if args.first().is_none_or(|word| word != "agh") {
        return Ok(None);
    }
    if let [_, auth, credential, operation] = args
        && auth == "auth"
        && credential == "git-credential"
        && matches!(operation.to_str(), Some("get" | "store" | "erase"))
    {
        return Ok(Some(args[1..].to_vec()));
    }
    Err("the cutter's agh entry accepts only auth git-credential get|store|erase")
}

impl Remote {
    pub(crate) fn origin(repo: &Path) -> Result<Self, String> {
        let slug = origin_slug(repo)?;
        let dirs = agh::config::Dirs::from_env()
            .ok_or("agh configuration and state directories are unavailable")?;
        Self::new(configured(&dirs)?, slug, repo)
    }

    fn new(client: Github, slug: Slug, repo: &Path) -> Result<Self, String> {
        let named = if client.router().tier_of(slug.owner()) == Tier::Release {
            Named::Literal(
                client
                    .router()
                    .grant(CALLER, &slug)
                    .map_err(|error| error.why.clone())?,
            )
        } else {
            Named::Default
        };
        let remote = Self {
            client,
            slug,
            repo: repo.to_path_buf(),
            named,
        };
        // A listing does not otherwise inspect its caller's checkout. Admit its
        // actual source/config first, just as exact fetch does before importing.
        remote.run(GitRun::check_local(&remote.slug, repo))?;
        Ok(remote)
    }

    fn run(&self, run: GitRun) -> Result<agh::git::GitOut, String> {
        self.client
            .run_git(&run.with_named(self.named.clone()).with_budget(BUDGET))
            .map_err(|error| self.client.redactor().apply(&error.why))
    }

    /// Read main and every tag through the configured native identity, import
    /// that exact commit, then record only the owned remote-tracking ref.
    pub(crate) fn fetch_origin(&self) -> Result<FetchedOrigin, String> {
        let before = self.tracking_main()?;
        let main = self
            .get("refs/heads/main")?
            .ok_or("the authenticated origin has no main branch")?;
        let out = self.run(GitRun::ls_remote(&self.slug, Some("refs/tags/*")))?;
        let tags = parse_tags(&out.stdout)?;
        // Each native import uses a fresh admitted scratch repository. Import
        // the observed objects together rather than transfer history per tag.
        let objects = std::iter::once(main.clone())
            .chain(tags.values().cloned())
            .collect::<BTreeSet<_>>();
        let batch = FetchBatch::new(
            objects
                .into_iter()
                .map(|oid| FetchRow {
                    source: oid.clone(),
                    into: format!("refs/agh/aterm-release/object/{oid}"),
                    expected_oid: oid,
                })
                .collect(),
        )
        .map_err(|error| self.client.redactor().apply(&error.why))?;
        self.run(GitRun::fetch_batch(&self.slug, &self.repo, batch, false))?;
        self.require_commit(&main)?;
        let tags = self.peel_tags(tags)?;
        self.record_main(before.as_deref(), &main)?;
        Ok(FetchedOrigin {
            main,
            tags: tags
                .into_iter()
                .map(|(name, oid)| format!("{oid}\t{name}\n"))
                .collect(),
        })
    }

    /// Read one exact tag afresh, importing only its observed immutable object.
    pub(crate) fn tag(&self, tag: &str) -> Result<Option<String>, String> {
        let name = format!("refs/tags/{tag}");
        if !agh::path::ref_name_ok(&name) {
            return Err("a release tag must name one exact Git ref".into());
        }
        let Some(oid) = self.get(&name)? else {
            return Ok(None);
        };
        self.fetch_exact(&oid)?;
        Ok(Some(self.peel_tag(&oid)?.unwrap_or(oid)))
    }

    fn peel_tags(
        &self,
        mut tags: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, String> {
        let originals: Vec<_> = tags
            .iter()
            .filter(|(name, _)| !name.ends_with("^{}"))
            .map(|(name, oid)| (name.clone(), oid.clone()))
            .collect();
        let mut resolved: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (name, oid) in originals {
            let target = match resolved.get(&oid) {
                Some(target) => target.clone(),
                None => {
                    let target = self.peel_tag(&oid)?;
                    resolved.insert(oid, target.clone());
                    target
                }
            };
            let peeled = format!("{name}^{{}}");
            match (target, tags.get(&peeled)) {
                (Some(target), Some(advertised)) if &target != advertised => {
                    return Err("tag peel differs from its imported object".into());
                }
                (Some(target), _) => {
                    tags.insert(peeled, target);
                }
                (None, Some(_)) => return Err("a non-tag object has an advertised peel".into()),
                (None, None) => {}
            }
        }
        Ok(tags)
    }

    fn peel_tag(&self, oid: &str) -> Result<Option<String>, String> {
        // The caller imported this exact object through the native boundary.
        // No lazy network read or per-tag history transfer occurs here.
        let kind = self.local(&["cat-file", "-t", oid])?;
        if !kind.status.success() {
            return Err(self.local_error("read tagged object", &kind));
        }
        match kind.stdout.as_slice() {
            b"commit\n" | b"tree\n" | b"blob\n" => Ok(None),
            b"tag\n" => {
                let expression = format!("{oid}^{{}}");
                let out = self.local(&["rev-parse", "--verify", &expression])?;
                if !out.status.success() {
                    return Err(self.local_error("peel imported tag", &out));
                }
                let target = std::str::from_utf8(&out.stdout)
                    .map_err(|_| "peeled tag target is not UTF-8")?
                    .trim();
                if !agh::path::is_sha(target) {
                    return Err("peeled tag target is not one raw object ID".into());
                }
                Ok(Some(target.into()))
            }
            _ => Err("tag ref does not name a recognized Git object".into()),
        }
    }

    /// Git here reads or updates local metadata only. The explicit checkout,
    /// scrubbed Git context and disabled hooks cannot select a remote transport.
    fn local(&self, args: &[&str]) -> Result<std::process::Output, String> {
        let mut command = crate::ledger::object_git(&self.repo);
        command.args(["-c", "core.hooksPath=/dev/null"]).args(args);
        aterm_uds::deadline::output_until_complete_stdout(
            &mut command,
            std::time::Instant::now() + BUDGET,
        )
        .map_err(|error| {
            self.client
                .redactor()
                .apply(&format!("local Git metadata did not finish: {error}"))
        })
    }

    fn local_error(&self, phase: &str, out: &std::process::Output) -> String {
        self.client.redactor().apply(&format!(
            "{phase} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    fn require_commit(&self, oid: &str) -> Result<(), String> {
        let out = self.local(&["cat-file", "-t", oid])?;
        if !out.status.success() || out.stdout != b"commit\n" {
            return Err("origin/main must resolve to an available commit object".into());
        }
        Ok(())
    }

    fn tracking_main(&self) -> Result<Option<String>, String> {
        let symbolic = self.local(&["symbolic-ref", "-q", ORIGIN_MAIN])?;
        match symbolic.status.code() {
            Some(1) => {}
            Some(0) => return Err("origin/main is symbolic; refusing to replace it".into()),
            _ => return Err(self.local_error("inspect origin/main", &symbolic)),
        }
        let out = self.local(&["rev-parse", "--verify", "--quiet", ORIGIN_MAIN])?;
        match out.status.code() {
            Some(0) => {
                let oid = std::str::from_utf8(&out.stdout)
                    .map_err(|_| "origin/main is not a UTF-8 object ID")?
                    .trim();
                if !agh::path::is_sha(oid) {
                    return Err("origin/main is not one raw object ID".into());
                }
                self.require_commit(oid)?;
                Ok(Some(oid.into()))
            }
            Some(1) if out.stdout.is_empty() && out.stderr.is_empty() => Ok(None),
            _ => Err(self.local_error("read origin/main", &out)),
        }
    }

    fn record_main(&self, before: Option<&str>, main: &str) -> Result<(), String> {
        if !agh::path::is_sha(main) || before.is_some_and(|oid| !agh::path::is_sha(oid)) {
            return Err("tracking-ref update needs raw object IDs".into());
        }
        self.run(GitRun::check_local(&self.slug, &self.repo))?;
        if origin_slug(&self.repo)?.key() != self.slug.key() {
            return Err("source origin changed while its native refresh was running".into());
        }
        if self.tracking_main()?.as_deref() != before {
            return Err("origin/main changed while its native refresh was running".into());
        }
        let out = self.local(&[
            "update-ref",
            "--no-deref",
            ORIGIN_MAIN,
            main,
            before.unwrap_or(NULL_OID),
        ])?;
        if !out.status.success() {
            return Err(self.local_error("record fetched origin/main", &out));
        }
        Ok(())
    }

    /// List only refs beneath this full namespace, as one admitted snapshot.
    pub(crate) fn list(&self, namespace: &str) -> Result<BTreeMap<String, String>, String> {
        if !namespace.starts_with("refs/")
            || !namespace.ends_with('/')
            || !agh::path::ref_name_ok(&format!("{namespace}probe"))
        {
            return Err("a release ref listing needs a full ref namespace".into());
        }
        let pattern = format!("{namespace}*");
        let out = self.run(GitRun::ls_remote(&self.slug, Some(&pattern)))?;
        parse_refs(&out.stdout, namespace)
    }

    /// Resolve one exact full ref without interpreting it as a pattern.
    pub(crate) fn get(&self, reference: &str) -> Result<Option<String>, String> {
        if !agh::path::ref_name_ok(reference) {
            return Err("a release ref read needs a full Git ref".into());
        }
        let out = self.run(GitRun::ls_remote(&self.slug, Some(reference)))?;
        let mut refs = parse_refs(&out.stdout, reference)?;
        if refs.keys().any(|name| name != reference) {
            return Err("GitHub answered an exact ref read with another ref".into());
        }
        Ok(refs.remove(reference))
    }

    /// Import this advertised raw object into agh's cache namespace. The native
    /// runner preserves branches, tags, tracking refs, index and FETCH_HEAD.
    pub(crate) fn fetch_exact(&self, oid: &str) -> Result<(), String> {
        if !agh::path::is_sha(oid) {
            return Err("an exact release fetch needs a raw lowercase object ID".into());
        }
        let into = format!("refs/agh/aterm-release/object/{oid}");
        self.run(GitRun::fetch_ref(&self.slug, &self.repo, oid, &into, false))?;
        Ok(())
    }

    /// Read a blob from the imported raw tree, ignoring replacement refs and
    /// inherited Git state just as the native transfer does. No lazy fetch is
    /// allowed; an absent object remains an error.
    pub(crate) fn blob(&self, oid: &str, path: &Path) -> Result<Vec<u8>, String> {
        if !agh::path::is_sha(oid)
            || path.as_os_str().is_empty()
            || !path
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err("an exact release blob read needs an OID and a relative tree path".into());
        }
        let path = path.to_str().ok_or("release tree path is not UTF-8")?;
        let mut command = crate::ledger::object_git(&self.repo);
        command.args(["cat-file", "blob", &format!("{oid}:{path}")]);
        let out = aterm_uds::deadline::output_until_complete_stdout(
            &mut command,
            std::time::Instant::now() + BUDGET,
        )
        .map_err(|error| format!("cannot read imported release blob: {error}"))?;
        if !out.status.success() {
            return Err(format!(
                "cannot read imported release blob: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout)
    }
}

fn parse_refs(text: &str, prefix: &str) -> Result<BTreeMap<String, String>, String> {
    if text.len() >= COMPLETE_LIMIT {
        return Err("GitHub ref listing may be incomplete; refusing an absence judgment".into());
    }
    let mut refs = BTreeMap::new();
    for row in text.lines() {
        let (oid, name) = row.split_once('\t').ok_or("malformed GitHub ref listing")?;
        if !agh::path::is_sha(oid)
            || !agh::path::ref_name_ok(name)
            || !name.starts_with(prefix)
            || refs.insert(name.to_owned(), oid.to_owned()).is_some()
        {
            return Err("GitHub ref listing has an invalid, duplicate or unrelated ref".into());
        }
    }
    Ok(refs)
}

fn parse_tags(text: &str) -> Result<BTreeMap<String, String>, String> {
    if text.len() >= COMPLETE_LIMIT {
        return Err("GitHub tag listing may be incomplete; refusing an absence judgment".into());
    }
    let mut tags = BTreeMap::new();
    for row in text.lines() {
        let (oid, name) = row.split_once('\t').ok_or("malformed GitHub tag listing")?;
        let plain = name.strip_suffix("^{}").unwrap_or(name);
        if !agh::path::is_sha(oid)
            || !agh::path::ref_name_ok(plain)
            || !plain.starts_with("refs/tags/")
            || plain == "refs/tags/"
            || tags.insert(name.to_owned(), oid.to_owned()).is_some()
        {
            return Err("GitHub tag listing has an invalid, duplicate or unrelated ref".into());
        }
    }
    if tags.keys().any(|name| {
        name.strip_suffix("^{}")
            .is_some_and(|plain| !tags.contains_key(plain))
    }) {
        return Err("GitHub tag listing has a peel without its tag ref".into());
    }
    Ok(tags)
}

#[cfg(all(test, unix))]
#[path = "github_remote/tests.rs"]
pub(crate) mod tests;

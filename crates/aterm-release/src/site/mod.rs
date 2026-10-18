// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship site`: alab.systems follows each release. The `/terminal` engine is rebuilt
//! from the public commit, the download buttons name the elected downloads (a
//! Linux pick: the install command, [`pages::linux_install_command`]),
//! `/install.sh` is the public commit's `tools/install.sh`, and `/releases` carries
//! the notes — and the owner is never blocked by it: no site checkout, no Firebase
//! login, or `--skip-site` leave the release untouched.
//!
//! Who runs it: `ship release`'s after-cut step, as `--latest`, once the release's
//! shape as GitHub serves it passed — and the publication engine's post-promote hook
//! (`ship hook post-promote`), whose promote-time run does nothing: before the cut
//! there is no release to link.
//!
//! What it touches: in the site checkout only the files it owns ([`OWNED_FILES`],
//! [`OWNED_GLOB`]), one scratch clone, and two markers in the site repo's git
//! directory ([`OWED_MARK`]: a sync commit no deploy has shipped yet;
//! [`IN_FLIGHT_MARK`]: a sync that began writing and has not committed). It reads
//! GitHub through the authenticated `gh` only, and writes no release.
//!
//! Order: clone, elect the downloads, render the pages, `/install.sh` and `/releases`
//! aside — every refusal happens there, before the engine rebuild makes the first
//! write into the checkout — then the engine ([`engine`]: built here, from the clone,
//! never by a script of the site's), the sitemap, commit, pull, push, deploy, verify.

mod diff;
pub mod engine;
pub mod linux;
pub mod notes;
pub mod pages;
pub mod releases;
pub mod sitemap;
pub mod toolchain;
mod world;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use aterm_uds::spawnfd::{Child, Command, Stdio};
use serde::Deserialize;

pub use world::Real;

/// What `ship site` takes on its command line (the post-promote hook takes the same).
pub const USAGE: &str = "usage: targo --unverified ship site [--latest] [--dry-run] [--no-deploy] \
     [--skip-site] [--site-dir DIR] [--releases FILE]
  --latest       sync to the public repo's main tip (else PUB_PUBLIC_COMMIT names it)
  --dry-run      compute and print the plan; change nothing (PUB_DRY_RUN=1 too)
  --no-deploy    commit and push, no deploy, no live check (SITE_NO_DEPLOY=1 too)
  --skip-site    do nothing, say so, exit 0
  --site-dir DIR the site checkout (else SITE_DIR, else publish/site.toml's dir)
  --releases FILE plan from this release list instead of asking gh (with --dry-run only:
                 an offline plan, never a commit or a deploy)
exit: 0 synced (or synced and the live site unreadable, said), deferred or planned · 1 failed · 2 usage or an unreadable publish/site.toml · \
     3 no usable site checkout · 4 the live site does not show the committed site";

/// A release asset's bytes at a URL, at most the given count of them.
pub type Fetch<'f> = dyn FnMut(&str, u64) -> Result<Vec<u8>, String> + 'f;

/// Escape `s` for HTML text and attribute values.
#[must_use]
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Every file the sync writes, relative to the site directory. Nothing else in the
/// checkout is ever staged, and a pre-existing edit to any of these is a refusal —
/// unless an interrupted sync wrote it ([`IN_FLIGHT_MARK`]), when it is restored.
pub const OWNED_FILES: [&str; 9] = [
    "public/index.html",
    "public/aterm.html",
    "public/terminal.html",
    "public/releases.html",
    "public/assets/releases.css",
    "public/install.sh",
    "public/sitemap.xml",
    "public/assets/term/engine.js",
    "public/assets/term/ENGINE_SOURCE",
];

/// The installer `/install.sh` serves, as the public commit carries it.
pub const INSTALLER_SOURCE: &str = "tools/install.sh";

/// The public commit's installer, to be served as `/install.sh`: present, and piped
/// with no checkout (as `curl … | bash` runs it) installing from `slug`, the channel
/// whose releases the site links.
///
/// # Errors
/// No installer in the tree, or one whose public default is another repository.
pub fn installer(clone: &Path, slug: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(clone.join(INSTALLER_SOURCE)).map_err(|e| {
        format!(
            "the public commit has no readable {INSTALLER_SOURCE} to serve as /install.sh ({e})"
        )
    })?;
    let default = format!("OFFICIAL_REPO_SLUG=\"{slug}\"");
    if !text.lines().any(|line| line == default) {
        return Err(format!(
            "the public commit's {INSTALLER_SOURCE} does not declare {default}: piped from \
             alab.systems it would install from another repository than the one the site links"
        ));
    }
    Ok(text)
}

/// The engine's hash-named wasm pair, as a git pathspec.
pub const OWNED_GLOB: &str = "public/assets/term/aterm_wasm.*";

/// The marker of a sync commit no deploy has shipped, in the site repo's git
/// directory (never the working tree, never committed).
pub const OWED_MARK: &str = "aterm-post-promote-undeployed";

/// The marker of a sync that began writing into the checkout and has not committed,
/// in the site repo's git directory: what proves an owned file's edit is a sync's own,
/// left by an interrupt, and not a person's.
pub const IN_FLIGHT_MARK: &str = "aterm-site-sync-in-flight";

/// Firebase's ways of saying its login is missing or expired (lowercase).
const AUTH_PATTERNS: [&str; 8] = [
    "authentication error",
    "failed to authenticate",
    "firebase login",
    "not logged in",
    "invalid_grant",
    "unable to authorize",
    "firebase-tools is not installed",
    "credentials are no longer valid",
];

/// Whether `log` says the Firebase login is missing or expired.
#[must_use]
pub fn auth_failure(log: &str) -> bool {
    let low = log.to_lowercase();
    let cli_list = low.match_indices("not in the cli").any(|(at, m)| {
        let mut rest = low[at + m.len()..].chars();
        rest.next().is_some() && rest.as_str().starts_with("s account list")
    });
    cli_list || AUTH_PATTERNS.iter().any(|p| low.contains(p))
}

/// The command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub latest: bool,
    pub dry_run: bool,
    pub no_deploy: bool,
    pub skip: bool,
    pub site_dir: Option<PathBuf>,
    pub releases: Option<PathBuf>,
}

/// Parse the command line (`Ok(None)` is `--help`).
///
/// # Errors
/// An unknown argument, or a flag without its value.
pub fn parse_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Option<Options>, String> {
    let mut o = Options::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg {
            "--latest" => o.latest = true,
            "--dry-run" => o.dry_run = true,
            "--no-deploy" => o.no_deploy = true,
            "--skip-site" => o.skip = true,
            "--site-dir" | "--releases" => {
                let v = it
                    .next()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("{arg} needs a path"))?;
                let slot = if arg == "--site-dir" {
                    &mut o.site_dir
                } else {
                    &mut o.releases
                };
                *slot = Some(PathBuf::from(v));
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // a local list may be stale or hand-made: it plans, it is never committed or deployed
    if o.releases.is_some() && !o.dry_run {
        return Err("--releases plans offline from a local list: it needs --dry-run".into());
    }
    Ok(Some(o))
}

/// This machine's site settings, `publish/site.toml` — kept in the never-exported
/// publish/ because they are this machine's paths and accounts, not product source.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    dir: Option<String>,
    url: Option<String>,
    firebase_account: Option<String>,
    /// The deployer's service-account key, relative to the site directory.
    firebase_sa_key: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    subject_prefix: Option<String>,
}

/// Everything the sync reads besides its command line: the engine's facts
/// (`PUB_*`), the operator's knobs (`SITE_*`, `FIREBASE_*`, `GIT_AUTHOR_*`),
/// `publish/site.toml`, the wasm32 pin, `HOME`, and the trust root.
#[derive(Debug, Clone)]
pub struct Env {
    pub slug: String,
    /// `PUB_PUBLIC_COMMIT`: the public commit to sync to.
    pub commit: Option<String>,
    /// `PUB_VERSION`: the released version, named in the site commit.
    pub version: Option<String>,
    /// `PUB_STAGING_COMMIT`: set only by the engine's promote, which marks the
    /// promote-time run.
    pub staging_commit: Option<String>,
    pub dry_run: bool,
    /// `PUB_CLONE_URL`: where the public repo is cloned from.
    pub clone_url: String,
    pub site_dir: Option<PathBuf>,
    pub no_deploy: bool,
    /// The live origin the verify step reads.
    pub site_url: String,
    /// The release download host the toolchain facts are read from.
    pub download_base: String,
    pub verify_retries: u32,
    pub verify_sleep: u64,
    /// `YYYY-MM-DD` for the sitemap's `lastmod`.
    pub today: String,
    pub firebase_account: Option<String>,
    /// `FIREBASE_SA_KEY`: the deployer's service-account key.
    pub firebase_sa_key: Option<PathBuf>,
    /// `publish/site.toml`'s key, relative to the site directory the sync uses.
    pub firebase_sa_key_in_site: Option<String>,
    /// The site commit's identity; `None` leaves git's own.
    pub author: Option<(String, String)>,
    pub subject_prefix: String,
    /// The stock channel `publish/config.sh`'s wasm32 clause names.
    pub wasm_pin: Option<String>,
    /// `HOME`: remapped out of the `/terminal` engine, and refused if it survives there.
    pub home: Option<PathBuf>,
    /// The paper master keyset the Linux chain is verified under.
    pub masters: Vec<String>,
    pub now_unix: i64,
    /// Environment for every git child (a test isolates git from this machine's).
    pub git_env: Vec<(String, String)>,
}

/// `publish/site.toml` under `repo`: none is no settings; one that does not parse (a
/// typo'd key among them) is an error naming the file, never silently no settings.
///
/// # Errors
/// The file exists and cannot be read or parsed.
fn settings(repo: &Path) -> Result<Settings, String> {
    let path = repo.join("publish/site.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => aterm_toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl Env {
    /// The environment of this process, for the repository at `repo`; `slug` names the
    /// public repository instead of `PUB_REPO_SLUG`.
    ///
    /// # Errors
    /// A `publish/site.toml` that exists and does not parse.
    pub fn from_process(repo: &Path, slug: Option<&str>) -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let flag = |k: &str| var(k).is_some_and(|v| v != "0");
        let settings = settings(repo)?;
        let home = var("HOME").map(PathBuf::from).unwrap_or_default();
        let expand = |s: &str| match s.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(s),
        };
        let site_dir = var("SITE_DIR")
            .map(PathBuf::from)
            .or_else(|| settings.dir.as_deref().map(expand));
        let slug = slug
            .map(str::to_string)
            .or_else(|| var("PUB_REPO_SLUG"))
            .unwrap_or_else(|| "alabsystems/aterm".into());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let author = match (
            var("GIT_AUTHOR_NAME").or(settings.author_name),
            var("GIT_AUTHOR_EMAIL").or(settings.author_email),
        ) {
            (Some(n), Some(e)) => Some((n, e)),
            _ => None,
        };
        let wasm_pin = std::fs::read_to_string(repo.join("publish/config.sh"))
            .ok()
            .and_then(|c| crate::release_steps::wasm_clause(&c).ok())
            .map(|c| c.pin);
        Ok(Self {
            clone_url: var("PUB_CLONE_URL")
                .unwrap_or_else(|| format!("https://github.com/{slug}.git")),
            slug,
            commit: var("PUB_PUBLIC_COMMIT"),
            version: var("PUB_VERSION"),
            staging_commit: var("PUB_STAGING_COMMIT"),
            dry_run: flag("PUB_DRY_RUN"),
            firebase_sa_key: var("FIREBASE_SA_KEY").map(PathBuf::from),
            firebase_sa_key_in_site: settings.firebase_sa_key,
            site_dir,
            no_deploy: flag("SITE_NO_DEPLOY"),
            site_url: var("SITE_URL")
                .or(settings.url)
                .unwrap_or_else(|| sitemap::ORIGIN.into()),
            download_base: var("SITE_DOWNLOAD_BASE").unwrap_or_else(|| "https://github.com".into()),
            verify_retries: var("SITE_VERIFY_RETRIES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            verify_sleep: var("SITE_VERIFY_SLEEP")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            today: var("SITE_SYNC_TODAY")
                .unwrap_or_else(|| aterm_types::rfc3339::format_rfc3339(now)[..10].to_string()),
            firebase_account: var("FIREBASE_ACCOUNT").or(settings.firebase_account),
            author,
            subject_prefix: settings.subject_prefix.unwrap_or_default(),
            wasm_pin,
            home: var("HOME").map(PathBuf::from),
            masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect(),
            now_unix: i64::try_from(now).unwrap_or(i64::MAX),
            git_env: Vec::new(),
        })
    }
}

/// A child's end: its exit code and its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: i32,
    pub log: String,
}

/// What a started child holds while it runs; dropped once it has ended.
pub type Guard = Box<dyn std::any::Any>;

/// How the sync starts every child it waits on — git and the world's alike. Under
/// `ship release` they keep the release's child rules (`release_steps`: none once
/// interrupted, each in a session of its own with git's prompt off, the interrupt
/// handed to it, the running child noted); `ship site` from a shell starts them as
/// they are ([`Plain`]).
pub trait Launch {
    /// The signal that interrupted the run, if one did: the sync starts nothing after it.
    fn interrupted(&self) -> Option<i32>;
    /// Start `cmd`, its stdio already set: the child, and what lives while it runs.
    ///
    /// # Errors
    /// The spawn's own, or `ErrorKind::Interrupted` once [`Launch::interrupted`] answers.
    fn spawn(&self, cmd: &mut Command) -> std::io::Result<(Child, Guard)>;
}

/// `ship site` from a shell: children start as they are, and a Ctrl-C reaches them
/// through the terminal's process group.
#[derive(Debug, Clone, Copy, Default)]
pub struct Plain;

impl Launch for Plain {
    fn interrupted(&self) -> Option<i32> {
        None
    }

    fn spawn(&self, cmd: &mut Command) -> std::io::Result<(Child, Guard)> {
        Ok((cmd.spawn()?, Box::new(())))
    }
}

/// `cmd` through `launch` as `Command::output` has it: stdin empty, both streams read.
///
/// # Errors
/// As [`Launch::spawn`], or a failed wait.
pub fn output(launch: &dyn Launch, cmd: &mut Command) -> std::io::Result<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, guard) = launch.spawn(cmd)?;
    let out = child.wait_with_output();
    drop(guard);
    out
}

/// Everything the sync asks of the world besides git: GitHub (through `gh`), the
/// release download host, the live site, the engine's stock wasm32 build and its
/// `wasm-opt` pass, the site's deploy script, and the clock. [`Real`] is the
/// machine's; a test serves a fixture world.
pub trait World {
    /// How every child of this sync starts, git's included.
    fn launch(&self) -> Rc<dyn Launch>;
    /// `gh api PATH` (`--paginate` when asked): GitHub through the authenticated gh only.
    ///
    /// # Errors
    /// gh's own words when it cannot answer.
    fn gh_api(&mut self, path: &str, paginate: bool) -> Result<String, String>;
    /// A release asset (`https://` or `file://`), at most `limit` bytes.
    ///
    /// # Errors
    /// A failed read.
    fn download(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String>;
    /// The live site's copy at `url`, past every cache.
    ///
    /// # Errors
    /// A failed fetch.
    fn live(&mut self, url: &str) -> Result<Vec<u8>, String>;
    /// The CPU module (`aterm_wasm.wasm`): `aterm-wasm` built in `clone` for wasm32,
    /// release profile, by `pin`'s stock cargo (a STOCK EXCEPTION: Trust ships only its
    /// host std), with `rustflags` on the wasm32 target alone. Its bytes.
    ///
    /// # Errors
    /// A missing tool (`wasm-opt` is asked before the build), or a build that did not
    /// finish.
    fn build_module(
        &mut self,
        clone: &Path,
        pin: &str,
        rustflags: &[String],
        out: &mut dyn Write,
    ) -> Result<Vec<u8>, String>;
    /// The shipping `wasm-opt` pass (`aterm_verify::wasm_glue::WASM_OPT_FLAGS`) over the
    /// bound module at `wasm`, in place.
    ///
    /// # Errors
    /// A pass that did not finish.
    fn optimise(&mut self, wasm: &Path, out: &mut dyn Write) -> Result<(), String>;
    /// The site's `deploy.sh`, run in `site`.
    fn deploy(&mut self, site: &Path, out: &mut dyn Write) -> Ran;
    /// When `firebase projects:list` cannot authenticate with the credential
    /// deploy.sh would use (the service-account key when it exists, else the
    /// account), what it said; `None` when it can.
    fn firebase_refuses(
        &mut self,
        site: &Path,
        account: Option<&str>,
        sa_key: Option<&Path>,
    ) -> Option<String>;
    fn sleep(&mut self, secs: u64);
}

/// One way the live site does not show the committed site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disagreement {
    /// A page, the engine pair or `/releases` differs from the checkout.
    Page(pages::Difference),
    /// The live `/terminal` engine was built from `served`, the site repo committed
    /// `committed`: the deployed engine is not in git.
    EngineNotCommitted { served: String, committed: String },
}

impl std::fmt::Display for Disagreement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Page(d) => d.fmt(f),
            Self::EngineNotCommitted { served, committed } => write!(
                f,
                "live /terminal engine is built from {served} but the site repo has committed \
                 {committed} — the deployed engine is not in git"
            ),
        }
    }
}

/// How a sync ended, short of a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `--skip-site`.
    Skipped,
    /// The promote-time run: the site follows the cut.
    Deferred,
    /// A dry run: what a real run would rewrite (site-relative), and whether it would
    /// rebuild the engine. Nothing was changed.
    Planned { files: Vec<String>, engine: bool },
    /// Synced (or already current) and the live site shows it; deployed by this run or
    /// not.
    Synced { deployed: bool },
    /// Synced (or already current), but the live site could not be fetched, so nothing
    /// says it shows the release: deployed by this run or not.
    LiveUnchecked { deployed: bool },
    /// Committed and pushed as asked, the deploy and the live check left out
    /// (`--no-deploy`, `SITE_NO_DEPLOY`): the commit stays owed and the next plain run
    /// deploys it.
    NoDeploy,
    /// Committed and pushed, but Firebase's login is missing or expired: the commit
    /// stays owed and the next run deploys it.
    NotDeployed,
    /// No usable site checkout here; nothing was touched.
    NoCheckout(String),
    /// The live site does not show the committed site after the settle loop.
    LiveDisagrees {
        deployed: bool,
        why: Vec<Disagreement>,
    },
}

impl Outcome {
    /// The CLI's exit code.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::NoCheckout(_) => 3,
            Self::LiveDisagrees { .. } => 4,
            _ => 0,
        }
    }
}

/// Where an interrupt stopped the sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// Before this step started.
    Before(&'static str),
    /// While a step ran: how that step ended.
    During(Box<Failure>),
}

/// What a refused sync would not act on: each refusal is one of these, so a caller (and
/// a test) tells one from another by kind, never by its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The site directory is no git checkout, or its git directory cannot be found.
    NotACheckout,
    /// Files this sync owns are dirty and no interrupted sync of ours left them: a
    /// person's edits.
    DirtyOwned,
    /// An interrupted sync left them, but a site commit of ours is still owed.
    DirtyOwed,
    /// An interrupted sync left them, and a dry run changes nothing, so it cannot put
    /// them back.
    DirtyDryRun,
    /// An interrupted sync's files could not all be put back from HEAD.
    NotPutBack,
    /// The site repo has staged changes this sync does not own.
    StagedUnrelated,
    /// No promoted commit was named, it is not a full sha, or the clone of it is not
    /// exactly it.
    Commit,
    /// The release stream does not read, or names nothing the buttons may link.
    Release,
    /// A page or the sitemap is missing or out of the shape its rules rewrite.
    Page,
    /// The public commit has no installer to serve as `/install.sh`, or one whose piped
    /// run would install from another repository than the one the site links
    /// ([`installer`]).
    Installer,
    /// The sync commit did not capture everything this sync owns.
    Uncaptured,
    /// The engine on disk is not the one the site repo committed.
    EngineNotInGit,
    /// A file or directory it cannot read, make or write.
    Io,
}

/// A sync that stopped: what happened, and the way on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Interrupted by `signal`: nothing further was started.
    Interrupted { signal: i32, step: Stop },
    /// What the sync found, or was handed, and will not act on (`kind`); `why` carries
    /// the way on.
    Refused { kind: Refusal, why: String },
    /// A child it ran failed: `child` names it (`git push`, `deploy.sh` …), `said` is
    /// the account, the way on included.
    Child { child: &'static str, said: String },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted {
                signal,
                step: Stop::Before(step),
            } => write!(
                f,
                "interrupted (signal {signal}) before {step} — nothing further was started"
            ),
            Self::Interrupted {
                signal,
                step: Stop::During(ended),
            } => write!(
                f,
                "interrupted (signal {signal}) — nothing further was started; the step it \
                 stopped said: {ended}"
            ),
            Self::Refused { why, .. } | Self::Child { said: why, .. } => f.write_str(why),
        }
    }
}

fn fail<T>(kind: Refusal, msg: impl Into<String>) -> Result<T, Failure> {
    Err(refusal(kind, msg))
}

/// A refusal of `kind`, said as `why`.
fn refusal(kind: Refusal, why: impl Into<String>) -> Failure {
    Failure::Refused {
        kind,
        why: why.into(),
    }
}

/// The child `child` failed, as `said`.
fn child_failed(child: &'static str, said: impl Into<String>) -> Failure {
    Failure::Child {
        child,
        said: said.into(),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// git, with the sync's environment, started through the sync's [`Launch`].
struct Git<'e> {
    env: &'e [(String, String)],
    launch: &'e dyn Launch,
}

impl Git<'_> {
    fn run(&self, dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut git = aterm_uds::spawnfd::command("git");
        git.arg("-C")
            .arg(dir)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        output(self.launch, &mut git)
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> bool {
        self.run(dir, args).is_ok_and(|o| o.status.success())
    }

    /// stdout when git succeeds, else its last stderr line.
    fn out(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        match self.run(dir, args) {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(format!("cannot run git: {e}")),
        }
    }

    fn line(&self, dir: &Path, args: &[&str]) -> Option<String> {
        self.out(dir, args)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

fn last_line(s: &str) -> &str {
    s.trim().lines().last().unwrap_or("")
}

/// Stop before `step` once the run was interrupted: nothing further is started.
fn halt(launch: &dyn Launch, step: &'static str) -> Result<(), Failure> {
    match launch.interrupted() {
        Some(signal) => Err(Failure::Interrupted {
            signal,
            step: Stop::Before(step),
        }),
        None => Ok(()),
    }
}

/// A new private (0700) directory `<temp>/<prefix>-<pid>-<n>` this process alone holds:
/// `create` refuses an existing entry, so a planted path is stepped over, never written
/// through, and the process-wide counter keeps concurrent callers apart.
///
/// # Errors
/// No directory could be made.
pub(crate) fn private_dir(prefix: &str) -> Result<PathBuf, String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    let mut last = String::from("no attempt");
    for _ in 0..64 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = base.join(format!("{prefix}-{}-{n}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        match b.create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last = format!("{} exists", path.display());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err(format!(
        "no free scratch directory in {} ({last})",
        base.display()
    ))
}

/// A private scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, Failure> {
        private_dir("aterm-site")
            .map(Self)
            .map_err(|e| refusal(Refusal::Io, format!("cannot make a scratch directory: {e}")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn say(out: &mut dyn Write, line: &str) {
    let _ = writeln!(out, "== {line}");
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Bring the site at `opts.site_dir` (or the environment's) to the public commit and
/// the elected downloads; see the module docs.
///
/// # Errors
/// [`Failure`] — exit 1: a refusal before anything was written, a pull or push of the
/// site repo that failed (the sync commit stays on this machine and nothing is
/// deployed; a re-run pushes and deploys it), a failed deploy that is not a login, an
/// interrupt (said as one, whatever the step it stopped said).
pub fn sync(
    opts: &Options,
    env: &Env,
    world: &mut dyn World,
    out: &mut dyn Write,
) -> Result<Outcome, Failure> {
    let launch = world.launch();
    let ended = steps(opts, env, world, out);
    match (ended, launch.interrupted()) {
        (Err(ended), Some(signal)) if !matches!(ended, Failure::Interrupted { .. }) => {
            Err(Failure::Interrupted {
                signal,
                step: Stop::During(Box::new(ended)),
            })
        }
        (ended, _) => ended,
    }
}

/// [`sync`]'s steps, in order.
#[allow(clippy::too_many_lines)] // one ordered procedure, its steps commented in place
fn steps(
    opts: &Options,
    env: &Env,
    world: &mut dyn World,
    out: &mut dyn Write,
) -> Result<Outcome, Failure> {
    let slug = env.slug.as_str();
    if opts.skip {
        say(
            out,
            "site sync skipped (--skip-site) — sync later with: targo --unverified ship site --latest",
        );
        return Ok(Outcome::Skipped);
    }
    // The engine runs the hook at promote time with PUB_STAGING_COMMIT and no
    // --latest; the promoted version has no release yet, so that run could only
    // re-link the previous DMG. The release's after-cut step syncs.
    if !opts.latest && env.staging_commit.is_some() {
        let v = env
            .version
            .as_deref()
            .map(|v| format!(" (v{v})"))
            .unwrap_or_default();
        say(
            out,
            &format!(
                "promote-time run: alab.systems follows the CUT, not the promote — the promoted \
                 version{v} has no release yet, so this run could only re-link the previous DMG. \
                 Nothing was done."
            ),
        );
        say(
            out,
            "the release's after-cut step runs `targo --unverified ship site --latest`; with no \
             cut to follow, run that by hand",
        );
        return Ok(Outcome::Deferred);
    }
    let dry = opts.dry_run || env.dry_run;
    let no_deploy = opts.no_deploy || env.no_deploy;
    let launch = world.launch();
    let launch = &*launch;
    let git = Git {
        env: &env.git_env,
        launch,
    };
    halt(launch, "the site checkout was read")?;

    // 0. preconditions: a usable site checkout, nothing of ours dirty or staged
    let Some(site_dir) = opts.site_dir.clone().or_else(|| env.site_dir.clone()) else {
        return Ok(Outcome::NoCheckout(
            "no site checkout is configured — set SITE_DIR (or publish/site.toml's dir) or skip \
             with --skip-site"
                .into(),
        ));
    };
    if !site_dir.join("public/index.html").is_file() {
        return Ok(Outcome::NoCheckout(format!(
            "site checkout not found — set SITE_DIR or skip with --skip-site (looked in {})",
            site_dir.display()
        )));
    }
    if !is_executable(&site_dir.join("deploy.sh")) {
        return Ok(Outcome::NoCheckout(format!(
            "site checkout at {} has no executable deploy.sh — not a usable site checkout; set \
             SITE_DIR or skip with --skip-site",
            site_dir.display()
        )));
    }
    let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
    let Some(site_root) = git
        .line(&site_dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
    else {
        return fail(
            Refusal::NotACheckout,
            format!("{} is not inside a git checkout", site_dir.display()),
        );
    };
    let public = site_dir.join("public");
    let term = public.join("assets/term");
    let mark = term.join("ENGINE_SOURCE");
    let git_dir = git
        .line(&site_root, &["rev-parse", "--absolute-git-dir"])
        .map(PathBuf::from)
        .ok_or_else(|| {
            refusal(
                Refusal::NotACheckout,
                "cannot find the site repo's git directory",
            )
        })?;
    let owed_mark = git_dir.join(OWED_MARK);
    let in_flight = git_dir.join(IN_FLIGHT_MARK);
    let branch = git
        .line(&site_root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|| "main".into());
    // Our commits origin does not have yet (none when origin's branch is unknown).
    let unpushed_owned = || -> String {
        let upstream = format!("refs/remotes/origin/{branch}");
        if !git.ok(
            &site_root,
            &[
                "rev-parse",
                "--quiet",
                "--verify",
                &format!("{upstream}^{{commit}}"),
            ],
        ) {
            return String::new();
        }
        let range = format!("{upstream}..HEAD");
        let mut args = vec!["log", "--format=%h", range.as_str(), "--"];
        args.extend(OWNED_FILES);
        args.push(OWNED_GLOB);
        git.out(&site_dir, &args)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut owned: Vec<&str> = vec!["status", "--porcelain", "--"];
    owned.extend(OWNED_FILES);
    owned.push(OWNED_GLOB);
    let dirty = git
        .out(&site_dir, &owned)
        .map_err(|e| child_failed("git status", e))?;
    if !dirty.trim().is_empty() {
        // An interrupted sync's own writes, with no commit of ours owed, are put back:
        // this run writes them again. Anything else is a person's, and refused.
        let owed = read_text(&owed_mark).is_some_and(|m| !m.trim().is_empty())
            || !unpushed_owned().is_empty();
        if !in_flight.is_file() || owed || dry {
            let (kind, why) = if !in_flight.is_file() {
                (Refusal::DirtyOwned, "commit or stash them first")
            } else if owed {
                (
                    Refusal::DirtyOwed,
                    "an interrupted sync left them, but a site commit of ours is still owed — \
                     put them back with `git checkout HEAD -- <them>` in the site checkout, then \
                     re-run",
                )
            } else {
                (
                    Refusal::DirtyDryRun,
                    "an interrupted sync left them: a run that is not --dry-run puts them back",
                )
            };
            return fail(
                kind,
                format!(
                    "site checkout has uncommitted changes in files this sync rewrites — \
                     {why}:\n{}",
                    dirty.trim_end()
                ),
            );
        }
        halt(launch, "the interrupted sync's files were put back")?;
        let put_back = || -> Result<String, String> {
            // what differs from HEAD in the index or the tree, tracked, then the untracked
            let mut diff: Vec<&str> = vec!["diff", "--name-only", "--relative", "HEAD", "--"];
            diff.extend(OWNED_FILES);
            diff.push(OWNED_GLOB);
            let changed = git.out(&site_dir, &diff)?;
            let changed: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();
            if !changed.is_empty() {
                let mut restore = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
                restore.extend(&changed);
                git.out(&site_dir, &restore)?;
            }
            let mut clean: Vec<&str> = vec!["clean", "-f", "-q", "--"];
            clean.extend(OWNED_FILES);
            clean.push(OWNED_GLOB);
            git.out(&site_dir, &clean)
        };
        put_back().map_err(|e| child_failed("git restore", e))?;
        let left = git
            .out(&site_dir, &owned)
            .map_err(|e| child_failed("git status", e))?;
        if !left.trim().is_empty() {
            return fail(
                Refusal::NotPutBack,
                format!(
                    "an interrupted sync's files could not all be put back from HEAD:\n{}",
                    left.trim_end()
                ),
            );
        }
        say(
            out,
            &format!(
                "an interrupted sync left files this sync owns; put back from HEAD:\n{}",
                dirty.trim_end()
            ),
        );
        let _ = std::fs::remove_file(&in_flight);
    }
    if !git.ok(&site_root, &["diff", "--cached", "--quiet"]) {
        return fail(
            Refusal::StagedUnrelated,
            format!(
                "the site repo ({}) has staged changes unrelated to this sync — commit or \
                 unstage them first",
                site_root.display()
            ),
        );
    }

    // 1. the public commit, and a fresh clone at exactly it
    let commit = match (&env.commit, opts.latest) {
        (Some(c), _) => c.clone(),
        (None, true) => {
            halt(launch, "asking gh for main's tip")?;
            let sha = world
                .gh_api(&format!("repos/{slug}/commits/main"), false)
                .ok()
                .and_then(|j| aterm_json::from_str::<aterm_json::Value>(&j).ok())
                .and_then(|v| {
                    v.get("sha")
                        .and_then(aterm_json::Value::as_str)
                        .map(str::to_string)
                })
                .ok_or_else(|| {
                    child_failed(
                        "gh",
                        format!(
                            "could not read the tip of {slug} main through gh (gh auth status)"
                        ),
                    )
                })?;
            say(out, &format!("--latest: {slug} main is at {sha}"));
            sha
        }
        (None, false) => {
            return fail(
                Refusal::Commit,
                "PUB_PUBLIC_COMMIT is required (the promoted public commit), or pass --latest",
            );
        }
    };
    let hex = (40..=64).contains(&commit.len())
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !hex {
        return fail(
            Refusal::Commit,
            format!("PUB_PUBLIC_COMMIT must be a full hex commit sha, got: {commit}"),
        );
    }
    let scratch = Scratch::new()?;
    let v_note = env
        .version
        .as_deref()
        .map(|v| format!(" (version {v})"))
        .unwrap_or_default();
    say(
        out,
        &format!(
            "site sync: {slug} @ {}{v_note} -> {}",
            short(&commit),
            site_dir.display()
        ),
    );
    if dry {
        say(
            out,
            &format!("DRY RUN — nothing in {} will change", site_dir.display()),
        );
    }
    let clone = scratch.0.join("clone");
    halt(launch, "the clone")?;
    say(out, &format!("cloning {} (shallow)", env.clone_url));
    let clone_arg = clone.to_string_lossy().into_owned();
    git.out(
        &scratch.0,
        &[
            "clone",
            "--quiet",
            "--depth",
            "1",
            &env.clone_url,
            &clone_arg,
        ],
    )
    .map_err(|e| {
        child_failed(
            "git clone",
            format!("clone of {} failed: {}", env.clone_url, last_line(&e)),
        )
    })?;
    if git.line(&clone, &["rev-parse", "HEAD"]).as_deref() != Some(commit.as_str()) {
        git.out(
            &clone,
            &["fetch", "--quiet", "--depth", "1", "origin", &commit],
        )
        .map_err(|e| {
            child_failed(
                "git fetch",
                format!(
                    "{commit} is not on {slug} (fetch failed: {})",
                    last_line(&e)
                ),
            )
        })?;
        git.out(&clone, &["checkout", "--quiet", "--detach", &commit])
            .map_err(|e| child_failed("git checkout", e))?;
    }
    if git.line(&clone, &["rev-parse", "HEAD"]).as_deref() != Some(commit.as_str()) {
        return fail(
            Refusal::Commit,
            format!("clone is not at {commit} (refusing to build from a different tree)"),
        );
    }
    if git.line(&clone, &["status", "--porcelain"]).is_some() {
        return fail(Refusal::Commit, "fresh clone is dirty (?)");
    }
    say(out, &format!("clone is at {commit}"));

    // 2. the downloads the buttons name — read, never written: every refusal here
    //    happens before the checkout is touched
    halt(launch, "the release list was read")?;
    let list = match &opts.releases {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| refusal(Refusal::Io, format!("cannot read {}: {e}", path.display())))?,
        None => world
            .gh_api(&format!("repos/{slug}/releases?per_page=100"), true)
            .map_err(|e| {
                child_failed(
                    "gh",
                    format!(
                        "could not list releases for {slug} through gh — the complete history, \
                         every page ({}; gh auth status)",
                        last_line(&e)
                    ),
                )
            })?,
    };
    let catalog = releases::parse_releases(&list).map_err(|e| {
        refusal(
            Refusal::Release,
            format!("releases JSON is not valid JSON: {e}"),
        )
    })?;
    halt(launch, "the downloads were elected")?;
    let mut notes = Vec::new();
    let masters: Vec<&str> = env.masters.iter().map(String::as_str).collect();
    let elected = {
        let mut get = |url: &str, limit: u64| world.download(url, limit);
        let mut chain = linux::Chain {
            slug,
            masters: &masters,
            now_unix: env.now_unix,
            get: &mut get,
        };
        releases::elect(&catalog, slug, &mut chain, &mut notes)
    };
    for n in &notes {
        say(out, &format!("note: {n}"));
    }
    let facts = elected.map_err(|e| refusal(Refusal::Release, e.to_string()))?;
    let mac = &facts.mac;
    let size_text = mac.size_label().unwrap_or_else(|| "size unknown".into());
    say(
        out,
        &format!(
            "download: {} ({size_text}, release {}) -> {}",
            mac.label, mac.tag, mac.url
        ),
    );
    match facts.linux("x86_64") {
        Some(d) => {
            say(
                out,
                &format!("linux: {} (release {}) -> {}", d.label, d.tag, d.url),
            );
            if d.tag != mac.tag {
                say(
                    out,
                    &format!(
                        "NOTE: macOS is {}; the newest published Linux x86_64 download is {}. No \
                         publication time is promised.",
                        mac.tag, d.tag
                    ),
                );
            }
        }
        None => say(
            out,
            "linux: no stable release carries its aterm-<v>-linux-x86_64.tar.gz and checksum — \
             no link offered",
        ),
    }
    say(
        out,
        &match facts.linux("aarch64") {
            Some(d) => format!("linux-aarch64: {} -> {}", d.label, d.url),
            None => "linux-aarch64: no published download — no link offered".into(),
        },
    );
    for arch in releases::LINUX_ARCHES {
        say(
            out,
            &match pages::linux_install_command(&facts, arch) {
                Some(command) => format!("linux-{arch} install: {command}"),
                None => format!(
                    "linux-{arch} install: no signed executable — the page offers no command"
                ),
            },
        );
    }
    let behind = match &env.version {
        Some(v) if format!("v{v}") != mac.tag => {
            say(
                out,
                &format!(
                    "NOTE: v{v} has no release yet — the newest published release is {}, so the \
                     download stays {}; the release's after-cut step re-runs this (or by hand: \
                     targo --unverified ship site --latest)",
                    mac.tag, mac.label
                ),
            );
            format!(" (v{v} not cut yet)")
        }
        _ => String::new(),
    };
    if mac.size_bytes.unwrap_or(0) > 200_000_000 {
        say(
            out,
            &format!(
                "WARNING: {} is {size_text} — not the lean image; the site links it as-is. Ship \
                 its successor from current main.",
                mac.label
            ),
        );
    }
    match &facts.intel_proof {
        Some(p) => say(
            out,
            &format!(
                "intel: {} ran its x86_64 slice under Rosetta — {p}",
                mac.tag
            ),
        ),
        None => say(
            out,
            &format!(
                "intel: {} carries no x86_64 run record — the Intel mark shows no proof",
                mac.tag
            ),
        ),
    }
    halt(launch, "the toolchain facts were read")?;
    let levels = {
        let mut get = |url: &str, limit: u64| world.download(url, limit);
        toolchain::fetch(&catalog, slug, &env.download_base, &mut get)
            .and_then(|f| toolchain::fetched_levels(&f).map(|l| (f.tag, f.manifests.len(), l)))
    };
    let levels = match levels {
        Ok((tag, n, l)) => {
            say(
                out,
                &format!(
                    "toolchain: aarch64={} x86_64={} ({tag}: index.toml and {n} pinned \
                     manifest(s))",
                    l[1].as_str(),
                    l[0].as_str()
                ),
            );
            Some(l)
        }
        Err(why) => {
            say(
                out,
                &format!(
                    "NOTE: toolchain facts unknown — {why} — the pages carry no data-toolchain-* \
                     attribute"
                ),
            );
            None
        }
    };

    // 3. the pages and the release notes, rendered aside
    let mut planned: BTreeMap<&str, String> = BTreeMap::new();
    for page in pages::PAGES {
        let text = read_text(&public.join(page)).ok_or_else(|| {
            refusal(
                Refusal::Page,
                format!("{page} is missing from {}", public.display()),
            )
        })?;
        let rewritten = pages::rewrite(page, &text, &facts, levels.as_ref())
            .map_err(|e| refusal(Refusal::Page, e.to_string()))?;
        planned.insert(
            match page {
                "index.html" => "public/index.html",
                "aterm.html" => "public/aterm.html",
                _ => "public/terminal.html",
            },
            rewritten,
        );
    }
    let (mut sections, source) = match read_text(&clone.join("CHANGELOG.md")) {
        Some(log) => (notes::from_changelog(&log), "CHANGELOG.md"),
        // the public snapshot exports no changelog: its notes are the release bodies
        None => (notes::from_releases(&catalog), "release bodies"),
    };
    sections.truncate(notes::LIMIT);
    let tags: Vec<&str> = sections.iter().map(|s| s.tag.as_str()).collect();
    say(
        out,
        &format!(
            "rendered releases.html from {source}: {}",
            if tags.is_empty() {
                "(no sections)".to_string()
            } else {
                tags.join(", ")
            }
        ),
    );
    planned.insert("public/releases.html", notes::render_page(slug, &sections));
    planned.insert("public/assets/releases.css", notes::CSS.to_string());
    let served = installer(&clone, slug).map_err(|e| refusal(Refusal::Installer, e))?;
    say(
        out,
        &format!(
            "installer: /install.sh = {INSTALLER_SOURCE} @ {} ({} bytes; piped, it installs \
             from {slug})",
            short(&commit),
            served.len()
        ),
    );
    planned.insert("public/install.sh", served);

    // 4. the /terminal engine — the first write into the checkout, so a real run marks
    //    itself in flight first (until its commit) — built here (engine.rs) on the wasm32
    //    pin publish/config.sh names (a STOCK EXCEPTION: Trust ships only its host std),
    //    else rustup's stable, as the site's script did
    if !dry {
        std::fs::write(&in_flight, format!("{commit}\n")).map_err(|e| {
            refusal(
                Refusal::Io,
                format!("cannot write {}: {e}", in_flight.display()),
            )
        })?;
    }
    let pin = env.wasm_pin.as_deref().unwrap_or("stable");
    let with = match &env.wasm_pin {
        Some(p) => format!("the wasm32 pin {p}"),
        None => "rustup's stable (no wasm32 clause names a channel in publish/config.sh)".into(),
    };
    let built = read_text(&mark).is_some_and(|m| m.trim() == commit);
    let engine_changed = if built && term.join("engine.js").is_file() {
        say(
            out,
            &format!(
                "engine: already built from {} (ENGINE_SOURCE) — skipping the rebuild",
                short(&commit)
            ),
        );
        false
    } else if dry {
        say(
            out,
            &format!(
                "engine: would rebuild public/assets/term from {} on {with}: aterm-wasm for \
                 wasm32 (+simd128), its glue from verify glue's generator, wasm-opt -O3, one \
                 hash-named pair behind engine.js",
                short(&commit)
            ),
        );
        true
    } else {
        halt(launch, "the engine build")?;
        say(
            out,
            &format!("engine: rebuilding from {} on {with}", short(&commit)),
        );
        let pair = engine::build(
            world,
            &clone,
            pin,
            env.home.as_deref(),
            &scratch.0.join("engine"),
            &term,
            out,
        )
        .map_err(|e| {
            child_failed(
                "the /terminal engine build",
                format!("the /terminal engine build failed: {e}"),
            )
        })?;
        say(
            out,
            &format!(
                "engine: aterm_wasm.{pair}.js and aterm_wasm.{pair}.wasm installed behind \
                 engine.js"
            ),
        );
        std::fs::write(&mark, format!("{commit}\n"))
            .map_err(|e| refusal(Refusal::Io, format!("cannot write {}: {e}", mark.display())))?;
        true
    };

    // 5. what changed, and the sitemap
    let differs = |rel: &str, new: &str| {
        std::fs::read(site_dir.join(rel)).map_or(true, |old| old != new.as_bytes())
    };
    let mut changed: Vec<&str> = Vec::new();
    let mut touch: Vec<&str> = Vec::new();
    for (rel, path) in [
        ("public/index.html", "/"),
        ("public/aterm.html", "/aterm"),
        ("public/terminal.html", "/terminal"),
        ("public/releases.html", "/releases"),
    ] {
        if differs(rel, &planned[rel]) {
            changed.push(rel);
            touch.push(path);
        }
    }
    if engine_changed {
        touch.push("/terminal");
    }
    for rel in ["public/assets/releases.css", "public/install.sh"] {
        if differs(rel, &planned[rel]) {
            changed.push(rel);
        }
    }
    let current_map = read_text(&public.join("sitemap.xml")).ok_or_else(|| {
        refusal(
            Refusal::Page,
            format!("{} has no sitemap.xml", public.display()),
        )
    })?;
    let map = sitemap::update(&current_map, "/releases", &env.today, &touch)
        .map_err(|e| refusal(Refusal::Page, e))?;
    if map.added {
        say(out, "sitemap: added /releases");
    }
    if differs("public/sitemap.xml", &map.text) {
        changed.push("public/sitemap.xml");
    }
    planned.insert("public/sitemap.xml", map.text);

    if dry {
        if changed.is_empty() {
            say(
                out,
                "dry run: pages, release notes and sitemap are already in sync",
            );
        } else {
            say(
                out,
                &format!("dry run: would update: {}", changed.join(" ")),
            );
            for rel in &changed {
                let new = &planned[rel];
                match read_text(&site_dir.join(rel)) {
                    None => say(out, &format!("  {rel}: new file ({} bytes)", new.len())),
                    Some(old) => {
                        for line in diff::unified(rel, &old, new).iter().take(40) {
                            let _ = writeln!(out, "  {line}");
                        }
                    }
                }
            }
        }
        say(
            out,
            &format!(
                "dry run: would commit, push origin main, and run {}/deploy.sh — nothing changed",
                site_dir.display()
            ),
        );
        return Ok(Outcome::Planned {
            files: changed.iter().map(|f| (*f).to_string()).collect(),
            engine: engine_changed,
        });
    }
    for rel in &changed {
        let path = site_dir.join(rel);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &planned[rel]));
        #[cfg(unix)]
        let written = written.and_then(|()| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        });
        written
            .map_err(|e| refusal(Refusal::Io, format!("cannot write {}: {e}", path.display())))?;
    }
    if !changed.is_empty() {
        say(out, &format!("updated: {}", changed.join(" ")));
    }

    // 6. commit — only what exists or is tracked: `git add` refuses a pathspec that
    //    matches nothing
    halt(launch, "the site commit")?;
    let mut stage: Vec<&str> = vec!["add", "-A", "--"];
    for f in OWNED_FILES {
        if site_dir.join(f).exists() || git.ok(&site_dir, &["ls-files", "--error-unmatch", "--", f])
        {
            stage.push(f);
        }
    }
    let pair_on_disk = std::fs::read_dir(&term).is_ok_and(|d| {
        d.flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("aterm_wasm."))
    });
    if pair_on_disk
        || git
            .line(&site_dir, &["ls-files", "--", OWNED_GLOB])
            .is_some()
    {
        stage.push(OWNED_GLOB);
    }
    git.out(&site_dir, &stage).map_err(|e| {
        child_failed(
            "git add",
            format!("staging the files this sync owns failed:\n{e}"),
        )
    })?;
    let mut nothing = git.ok(&site_dir, &["diff", "--cached", "--quiet"]);
    if nothing {
        say(
            out,
            &format!(
                "site already in sync with {slug} @ {} — nothing to commit",
                short(&commit)
            ),
        );
    } else {
        let version = env.version.clone().unwrap_or_else(|| mac.version.clone());
        let subject = format!(
            "{}alab.systems — aterm {version} published: /terminal engine @ {}, download → {}{behind}, \
             /releases",
            env.subject_prefix,
            short(&commit),
            mac.label
        );
        let mut body = format!(
            "Synced by aterm's `ship site` from github.com/{slug} @ {commit}.\nDownload: {} \
             ({size_text}, release {}).",
            mac.url, mac.tag
        );
        if !behind.is_empty() {
            body.push_str(&format!(
                " v{version} has no release yet; the release's after-cut step re-runs `ship site \
                 --latest`."
            ));
        }
        if let Some(d) = facts.linux("x86_64") {
            body.push_str(&format!(" Linux: {} (release {}).", d.url, d.tag));
        }
        body.push_str(" Release notes: /releases.");
        let mut args: Vec<String> = Vec::new();
        if let Some((name, email)) = &env.author {
            args.extend(["-c".into(), format!("user.name={name}")]);
            args.extend(["-c".into(), format!("user.email={email}")]);
        }
        args.extend(["commit", "--quiet", "-m"].map(String::from));
        args.push(subject.clone());
        args.push("-m".into());
        args.push(body);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        git.out(&site_dir, &refs)
            .map_err(|e| child_failed("git commit", format!("the site commit failed: {e}")))?;
        say(out, &format!("committed: {subject}"));
        // owed until a deploy ships it, whatever happens to the commit in between
        if let Some(head) = git.line(&site_dir, &["rev-parse", "HEAD"]) {
            let _ = std::fs::write(&owed_mark, format!("{head}\n"));
        }
    }
    // what this run wrote is committed (or was already): nothing of it is in flight
    let _ = std::fs::remove_file(&in_flight);

    // 6a. origin has it before anything is deployed: deploy.sh publishes the WORKING
    //     TREE, so an unpushed commit deployed is bytes no remote has. A pull or push
    //     that fails stops here; a re-run finishes the job — an unpushed commit of
    //     ours, or one pushed but never deployed (the owed marker), is pushed and
    //     deployed. Local commits touching nothing of ours are the site repo's own.
    let unpushed = unpushed_owned();
    let owed = read_text(&owed_mark)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let mut owed_only = false;
    if nothing && unpushed.is_empty() && !owed.is_empty() && !no_deploy {
        say(
            out,
            &format!(
                "an earlier sync commit ({} when it was made) is on origin but was never deployed \
                 (pushed by hand, --no-deploy, or a deploy that did not finish): deploying it now",
                short(&owed)
            ),
        );
        owed_only = true;
        nothing = false;
    }
    if !nothing || !unpushed.is_empty() {
        if nothing {
            say(
                out,
                &format!(
                    "an earlier sync left its commit(s) on this machine only ({unpushed}) — never \
                     pushed, so never deployed: pushing and deploying them now"
                ),
            );
            nothing = false;
        }
        let tail = |e: &str, out: &mut dyn Write| {
            let lines: Vec<&str> = e.lines().collect();
            for l in &lines[lines.len().saturating_sub(5)..] {
                let _ = writeln!(out, "  {l}");
            }
        };
        halt(launch, "the pull")?;
        if let Err(e) = git.out(
            &site_root,
            &[
                "pull",
                "--rebase",
                "--autostash",
                "--quiet",
                "origin",
                &branch,
            ],
        ) {
            let _ = git.run(&site_root, &["rebase", "--abort"]);
            tail(&e, out);
            return Err(child_failed(
                "git pull",
                format!(
                    "pull --rebase from origin {branch} failed (above) — the site commit is on THIS \
                 MACHINE ONLY and NOTHING WAS DEPLOYED. Resolve the pull by hand, and do not push: \
                 (cd {} && git pull --rebase origin {branch}); then re-run targo --unverified ship \
                 site --latest, which pushes the commit and deploys it",
                    site_root.display()
                ),
            ));
        }
        let pushed = if owed_only && unpushed_owned().is_empty() {
            None
        } else {
            halt(launch, "the push")?;
            Some(git.out(
                &site_root,
                &["push", "--quiet", "origin", &format!("HEAD:{branch}")],
            ))
        };
        if pushed.is_none() {
            say(
                out,
                &format!("origin {branch} already has it — nothing to push"),
            );
        } else if let Some(Err(e)) = pushed {
            tail(&e, out);
            return Err(child_failed(
                "git push",
                format!(
                    "push to origin {branch} failed (above) — the site commit is on THIS MACHINE \
                     ONLY and NOTHING WAS DEPLOYED. Re-run targo --unverified ship site --latest \
                     once origin accepts pushes: it pushes the commit and deploys it"
                ),
            ));
        } else {
            say(out, &format!("pushed origin {branch}"));
        }
    }

    // 6b. what gets deployed must be what got committed: `status` cannot see an
    //     ignored or assume-unchanged file, so the committed engine marker is compared
    //     with the one on disk too
    halt(launch, "the deploy")?;
    let leftover = git.out(&site_dir, &owned).unwrap_or_default();
    if !leftover.trim().is_empty() {
        return fail(
            Refusal::Uncaptured,
            format!(
                "the sync commit did not capture everything this sync owns, so deploying would \
             publish bytes the site repo does not have:\n{}",
                leftover.trim_end()
            ),
        );
    }
    let committed_engine = || {
        git.line(&public, &["show", "HEAD:./assets/term/ENGINE_SOURCE"])
            .unwrap_or_default()
    };
    let tree_engine = read_text(&mark)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_engine = committed_engine();
    if !tree_engine.is_empty() && tree_engine != head_engine {
        let head = if head_engine.is_empty() {
            "nothing"
        } else {
            &head_engine
        };
        return fail(
            Refusal::EngineNotInGit,
            format!(
                "the /terminal engine on disk was built from {tree_engine} but the site repo has \
             committed {head} — deploying would publish an engine that is not in git (is it \
             ignored, or was the commit skipped?)"
            ),
        );
    }

    // 7. deploy — a missing or expired Firebase login is not a failure of the release
    let deploy_later = format!("(cd {} && ./deploy.sh)", site_dir.display());
    let not_deployed = |out: &mut dyn Write| {
        let _ = writeln!(
            out,
            "SITE NOT DEPLOYED — committed and pushed; deploy later with: {deploy_later}"
        );
        Ok(Outcome::NotDeployed)
    };
    let mut deployed = false;
    if no_deploy {
        say(
            out,
            &format!("deploy skipped (--no-deploy) — deploy later with: {deploy_later}"),
        );
    } else if nothing {
        say(out, "nothing changed — no deploy needed");
    } else {
        halt(launch, "the deploy")?;
        say(out, &format!("deploying: {}/deploy.sh", site_dir.display()));
        let ran = world.deploy(&site_dir, out);
        if ran.code == 0 {
            deployed = true;
            let _ = std::fs::remove_file(&owed_mark);
        } else if auth_failure(&ran.log) {
            return not_deployed(out);
        } else {
            halt(launch, "the login probe")?;
            let key = env.firebase_sa_key.clone().or_else(|| {
                env.firebase_sa_key_in_site
                    .as_ref()
                    .map(|k| site_dir.join(k))
            });
            let Some(probe) =
                world.firebase_refuses(&site_dir, env.firebase_account.as_deref(), key.as_deref())
            else {
                return Err(child_failed(
                    "deploy.sh",
                    format!(
                        "deploy.sh failed (rc {}) — the site commit is pushed; fix and re-run: \
                         {deploy_later}",
                        ran.code
                    ),
                ));
            };
            say(
                out,
                &format!(
                    "deploy.sh failed (rc {}) and `firebase projects:list` cannot authenticate \
                     either — treating it as a missing or expired Firebase login",
                    ran.code
                ),
            );
            let lines: Vec<&str> = probe.lines().collect();
            for l in &lines[lines.len().saturating_sub(3)..] {
                let _ = writeln!(out, "  {l}");
            }
            return not_deployed(out);
        }
    }

    // 8. the live site, against the checkout
    let (mut live_checked, mut live_read) = (false, true);
    if no_deploy {
        say(out, "live check skipped (--no-deploy)");
    } else if deployed || nothing {
        let url = env.site_url.trim_end_matches('/');
        let local = |name: &str| -> Option<String> {
            match name {
                "engine.js" => read_text(&term.join("engine.js")),
                other => read_text(&public.join(other)),
            }
        };
        let (mut verified, mut fetched) = (false, false);
        live_checked = true;
        let mut said: Vec<Disagreement> = Vec::new();
        for attempt in 1..=env.verify_retries.max(1) {
            halt(launch, "the live check")?;
            if let Some(live) = fetch_live(world, url) {
                fetched = true;
                let get = |name: &str| live.get(name).cloned();
                let differences = pages::compare(
                    &facts,
                    &pages::Copies { get: &local },
                    &pages::Copies { get: &get },
                );
                said = differences.into_iter().map(Disagreement::Page).collect();
                if said.is_empty() {
                    // the engine the LIVE site serves must be the one the repo COMMITTED
                    let committed = committed_engine();
                    let served = live
                        .get("ENGINE_SOURCE")
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if !committed.is_empty() && !served.is_empty() && committed != served {
                        said.push(Disagreement::EngineNotCommitted { served, committed });
                    } else {
                        verified = true;
                        break;
                    }
                }
            }
            if attempt < env.verify_retries {
                halt(launch, "the live check")?;
                world.sleep(env.verify_sleep);
            }
        }
        if verified {
            let _ = writeln!(
                out,
                "  verify: live site matches the checkout (engine pair, download links, platform \
                 attributes, /releases, /install.sh)"
            );
            say(out, &format!("live: {url} matches the checkout"));
        } else if !fetched {
            say(
                out,
                &format!(
                    "WARNING: could not fetch {url} to verify — check it by hand: {url}/terminal"
                ),
            );
            live_read = false;
        } else {
            for l in &said {
                let _ = writeln!(out, "  verify: {l}");
            }
            let _ = if deployed {
                writeln!(
                    out,
                    "SITE DEPLOYED BUT THE LIVE SITE DISAGREES — deploy.sh succeeded, yet {url} \
                     still serves something else after {} checks (CDN lag, or another deploy in \
                     flight). Re-check in a minute: {url}/terminal",
                    env.verify_retries
                )
            } else {
                writeln!(
                    out,
                    "LIVE SITE DIFFERS FROM THE COMMITTED SITE — nothing changed here, but {url} \
                     does not show the committed checkout; deploy with: {deploy_later}"
                )
            };
            return Ok(Outcome::LiveDisagrees {
                deployed,
                why: said,
            });
        }
    }
    if nothing {
        say(
            out,
            &format!(
                "done — site already current for {slug} @ {}",
                short(&commit)
            ),
        );
    } else {
        say(
            out,
            &format!(
                "done — alab.systems synced to {slug} @ {}: engine {}, download {}, /releases{}",
                short(&commit),
                if engine_changed {
                    "rebuilt"
                } else {
                    "unchanged"
                },
                mac.label,
                if deployed {
                    ", deployed"
                } else if no_deploy {
                    ", not deployed (--no-deploy)"
                } else {
                    ""
                }
            ),
        );
    }
    if no_deploy {
        return Ok(Outcome::NoDeploy);
    }
    if live_checked && !live_read {
        return Ok(Outcome::LiveUnchecked { deployed });
    }
    Ok(Outcome::Synced { deployed })
}

/// The live copies the comparison reads, or `None` when one could not be fetched.
/// `/install.sh` is read too, and left out when the live site does not serve it: the
/// comparison names a checkout installer the live site lacks.
fn fetch_live(world: &mut dyn World, url: &str) -> Option<BTreeMap<&'static str, String>> {
    let mut live = BTreeMap::new();
    for (path, name) in [
        ("/", "index.html"),
        ("/aterm", "aterm.html"),
        ("/terminal", "terminal.html"),
        ("/releases", "releases.html"),
        ("/assets/term/engine.js", "engine.js"),
        ("/assets/term/ENGINE_SOURCE", "ENGINE_SOURCE"),
    ] {
        let bytes = world.live(&format!("{url}{path}")).ok()?;
        live.insert(name, String::from_utf8_lossy(&bytes).into_owned());
    }
    if let Ok(bytes) = world.live(&format!("{url}/install.sh")) {
        live.insert("install.sh", String::from_utf8_lossy(&bytes).into_owned());
    }
    Some(live)
}

/// `ship site` and the engine's post-promote hook: parse, sync, print, exit
/// ([`USAGE`] names the codes).
#[must_use]
pub fn run_cli(args: &[String], repo: &Path) -> i32 {
    let opts = match parse_args(args.iter().map(String::as_str)) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("ship site: {e}\n{USAGE}");
            return 2;
        }
    };
    let env = match Env::from_process(repo, None) {
        Ok(env) => env,
        Err(e) => {
            eprintln!("ship site: {e}");
            return 2;
        }
    };
    // `targo ... ship` hands this process cargo's own variables; no child inherits them
    let mut world = Real {
        unset: std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .filter(|k| crate::release_steps::launcher_env(k))
            .collect(),
        launch: Rc::new(Plain),
    };
    let mut out = std::io::stdout();
    let ended = sync(&opts, &env, &mut world, &mut out);
    let _ = out.flush();
    report(&ended)
}

/// The line a sync that did not finish its job ends with (stderr), and its exit code.
#[must_use]
pub fn report(ended: &Result<Outcome, Failure>) -> i32 {
    match ended {
        Ok(Outcome::NoCheckout(line)) => {
            eprintln!("ship site: {line}");
            3
        }
        Ok(o) => o.exit_code(),
        Err(failure) => {
            eprintln!("ship site: FAIL: {failure}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firebase_login_failures_are_recognised_and_others_are_not() {
        for log in [
            "Error: Authentication Error: Your credentials are no longer valid.",
            "firebase-tools is not installed. Run:  npm install -g firebase-tools",
            "admin@example.invalid is not in the CLI\u{2019}s account list",
            "Error: invalid_grant",
        ] {
            assert!(auth_failure(log), "{log}");
        }
        for log in [
            "Error: HTTP Error: 500, Internal Server Error",
            "Error: An unexpected error has occurred.",
        ] {
            assert!(!auth_failure(log), "{log}");
        }
    }

    #[test]
    fn the_command_line_is_the_hooks() {
        let o = parse_args(["--latest", "--site-dir", "/s", "--no-deploy"])
            .unwrap()
            .unwrap();
        assert!(o.latest && o.no_deploy && !o.dry_run);
        assert_eq!(o.site_dir.as_deref(), Some(Path::new("/s")));
        assert_eq!(parse_args(["--help"]).unwrap(), None);
        assert!(parse_args(["--site-dir"]).is_err());
        assert!(parse_args(["--bogus"]).is_err());
    }

    /// A local release list plans; a real run never commits or deploys from one.
    #[test]
    fn a_local_release_list_is_for_a_dry_run_only() {
        let why = parse_args(["--latest", "--releases", "/l.json"]).unwrap_err();
        assert!(why.contains("needs --dry-run"), "{why}");
        let o = parse_args(["--releases", "/l.json", "--dry-run"])
            .unwrap()
            .unwrap();
        assert_eq!(o.releases.as_deref(), Some(Path::new("/l.json")));
    }

    /// No publish/site.toml is no settings; one that does not parse — a typo'd key — is
    /// an error naming the file and the key, never silently "no site configured".
    #[test]
    fn a_site_toml_that_does_not_parse_is_named_never_ignored() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-site-settings-")
            .tempdir()
            .unwrap();
        assert!(settings(dir.path()).unwrap().dir.is_none());
        std::fs::create_dir_all(dir.path().join("publish")).unwrap();
        let file = dir.path().join("publish/site.toml");
        std::fs::write(&file, "dir = \"~/site\"\n").unwrap();
        assert_eq!(settings(dir.path()).unwrap().dir.as_deref(), Some("~/site"));
        std::fs::write(&file, "dirr = \"~/site\"\n").unwrap();
        let why = settings(dir.path()).unwrap_err();
        assert!(why.contains(&file.display().to_string()), "{why}");
        assert!(why.contains("dirr"), "{why}");
        assert!(Env::from_process(dir.path(), None).is_err());
    }

    /// Scratch names never collide inside one process, however close together.
    #[test]
    fn private_dirs_are_distinct_and_private() {
        let made: Vec<PathBuf> = (0..8)
            .map(|_| private_dir("aterm-site-t").unwrap())
            .collect();
        let unique: std::collections::BTreeSet<&PathBuf> = made.iter().collect();
        assert_eq!(unique.len(), made.len());
        #[cfg(unix)]
        for d in &made {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(d.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        for d in made {
            std::fs::remove_dir(d).unwrap();
        }
    }
}

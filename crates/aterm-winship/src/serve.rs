// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `release-serve`: the Windows half of the pull lane (`crate::pull`). The Mac's unattended
//! release pump reserves a release and writes a request for its Windows zip to origin; this
//! host, which the Mac cannot reach, polls origin, builds the requested release commit with
//! `release-build`, signs the receipt with its pinned builder key (`crate::attest`) and pushes
//! the pair back to origin as a handoff commit, which the cut admits in place of a digest a
//! person would type.
//!
//! One poll ([`serve_once`]):
//!
//! 1. Fetch origin's `main` and, when there is one, its request ([`pull::REQUEST_REF`], into
//!    [`LOCAL_REQUEST`]). git never prompts: a credential it cannot get without a person
//!    fails the poll.
//! 2. Serve only as a builder origin/main pins with this key, not expired
//!    ([`pull::BUILDERS_FILE`]): an unpinned host pushes no heartbeat, so no cut waits on it.
//! 3. Hold the request to its shape: one parent, the release commit C; a tree of
//!    [`pull::REQUEST_FILE`] alone, naming C and C's tree; C's one parent on origin/main; C
//!    changing only `RELEASES.ledger`, `CHANGELOG.md` and deleting files under `changes/`
//!    (the release commit's rule, crates/aterm-release/src/ledger.rs `reserve`); C's ledger
//!    ending in `<build> <version>` and its `Cargo.toml` stamping the version. A request of
//!    another shape is never built: what is compiled is origin/main's code (a `--fix` line's
//!    head is on origin/main too: the release records it there before it reserves).
//! 4. Pass over a request whose handoff is on origin already, one C does not pin this
//!    builder for, and one built [`MAX_ATTEMPTS`] times without a handoff.
//! 5. Push the heartbeat ([`pull::ALIVE_REF`]): this builder, the time, the request commit it
//!    read and, when it passed that request over, why ([`pull::PassedOver`]), so a cut waits
//!    only on a builder that builds its request. Again every [`HEARTBEAT_EVERY`] while a build
//!    runs.
//! 6. For a request it builds or handed off, delete origin's handoff refs of every other
//!    request: each holds a whole zip, and no cut reads an older request's.
//! 7. Build it ([`Build`]: in real runs [`TreeBuild`], C checked out in the release tree
//!    `%LOCALAPPDATA%\aterm\release\src` and built by this tool's own `release-build`),
//!    holding `update`'s lock so the two never build at once; admit the pair as the cutter
//!    will, sign it, split the zip into parts ([`pull::split`]) and push the handoff commit,
//!    a new ref that is never forced.
//!
//! One poll at a time: `release-serve` holds `%LOCALAPPDATA%\aterm\release\serve.lock`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::attest::{self, Builders};
use crate::handoff::{self, Cut, Pins};
use crate::proc;
use crate::pull::{self, Alive, PassedOver, Request};
use crate::repo::{self, GIT_LOCAL, GIT_NETWORK, Repo};
use crate::update::{Layout, Out};
use crate::{FAILED, NOT_RUN, PASS};

/// How long `release-serve` without `--once` waits between polls.
pub const POLL: Duration = Duration::from_secs(600);
/// How often the heartbeat is pushed while a build runs.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(300);
/// How many times one request is built before it is passed over.
pub const MAX_ATTEMPTS: u32 = 2;
/// The longest a release build may run before it and everything it started are ended.
pub const BUILD_HANG: Duration = Duration::from_secs(3 * 3600);
/// The longest a heartbeat's git commands may take.
const ALIVE_HANG: Duration = Duration::from_secs(120);
/// The local ref the request is fetched into: it keeps C from being collected while it builds.
pub const LOCAL_REQUEST: &str = "refs/aterm-winship/request";
/// origin's `main`, as the poll fetches it.
const MAIN: &str = "refs/remotes/origin/main";
/// A log past this size is moved aside to `<log>.old` before a poll appends to it.
pub const LOG_MAX_BYTES: u64 = 1 << 20;
/// The lines of a build's own log copied into the poll's.
const LOG_TAIL: usize = 30;

/// What `release-serve` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Poll once and exit; else poll every [`POLL`] until ended.
    pub once: bool,
    /// The repository; decided at run time when `None` (`crate::repo::resolve`).
    pub repo: Option<PathBuf>,
    /// The builder key; `%USERPROFILE%\.aterm\windows-builder\ed25519.pk8` when `None`.
    pub key: Option<PathBuf>,
    /// The builder id; this computer's name when `None`.
    pub builder: Option<String>,
}

/// What a git command said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRan {
    /// Its exit code.
    pub code: Option<i32>,
    /// Its standard output, as written.
    pub stdout: Vec<u8>,
    /// Its standard error.
    pub stderr: String,
}

/// git in one repository, with the variables this lane gives every git it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    /// The repository (any of its worktrees).
    pub dir: PathBuf,
    /// Set on every git started.
    pub env: Vec<(String, String)>,
}

impl Git {
    /// git in `dir` as a scheduled run starts it: never a terminal prompt or a credential
    /// window (`GIT_TERMINAL_PROMPT=0`, Git Credential Manager's `GCM_INTERACTIVE=never` and
    /// `credential.interactive=false`), so a credential that needs a person fails the poll.
    #[must_use]
    pub fn for_runs(dir: PathBuf) -> Self {
        Self {
            dir,
            env: vec![
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GCM_INTERACTIVE".into(), "never".into()),
            ],
        }
    }

    /// Runs `git -C <dir> -c credential.interactive=false args…` with `extra` variables, at
    /// most `within`.
    ///
    /// # Errors
    /// git did not start or did not end within `within`.
    pub fn ran(
        &self,
        args: &[&str],
        extra: &[(&str, &str)],
        within: Duration,
    ) -> Result<GitRan, String> {
        let mut all: Vec<&std::ffi::OsStr> = vec![
            "-C".as_ref(),
            self.dir.as_os_str(),
            "-c".as_ref(),
            "credential.interactive=false".as_ref(),
        ];
        all.extend(args.iter().map(std::ffi::OsStr::new));
        let mut cmd = proc::command(Path::new("git"), &all, proc::Place::default());
        cmd.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.envs(extra.iter().copied());
        cmd.stdin(aterm_uds::spawnfd::Stdio::null());
        let out = aterm_uds::deadline::output_within(&mut cmd, within)
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        Ok(GitRan {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// The standard output of a git command that must exit 0.
    ///
    /// # Errors
    /// It did not start, did not end in time or exited otherwise.
    pub fn bytes(&self, args: &[&str], within: Duration) -> Result<Vec<u8>, String> {
        self.bytes_with(args, &[], within)
    }

    fn bytes_with(
        &self,
        args: &[&str],
        extra: &[(&str, &str)],
        within: Duration,
    ) -> Result<Vec<u8>, String> {
        let ran = self.ran(args, extra, within)?;
        if ran.code == Some(0) {
            Ok(ran.stdout)
        } else {
            Err(format!(
                "git {} in {}: {} (exit {:?})",
                args.join(" "),
                self.dir.display(),
                crate::smoke::one_line(&ran.stderr, 240),
                ran.code
            ))
        }
    }

    /// The trimmed UTF-8 standard output of a git command that must exit 0.
    ///
    /// # Errors
    /// As [`Git::bytes`], or output that is not UTF-8.
    pub fn text(&self, args: &[&str], within: Duration) -> Result<String, String> {
        self.text_with(args, &[], within)
    }

    fn text_with(
        &self,
        args: &[&str],
        extra: &[(&str, &str)],
        within: Duration,
    ) -> Result<String, String> {
        String::from_utf8(self.bytes_with(args, extra, within)?)
            .map(|text| text.trim().to_string())
            .map_err(|_| format!("git {} wrote no UTF-8", args.join(" ")))
    }

    /// The object origin's `refname` names, exactly that ref; `None` when origin has none.
    ///
    /// # Errors
    /// origin could not be read.
    pub fn remote_ref(&self, refname: &str, within: Duration) -> Result<Option<String>, String> {
        let listing = self.text(&["ls-remote", "origin", refname], within)?;
        Ok(listing.lines().find_map(|line| {
            let (oid, name) = line.split_once('\t')?;
            (name.trim() == refname).then(|| oid.trim().to_string())
        }))
    }

    /// A commit of `files` (each `(name, path)`: the file at `path`, its exact bytes, as
    /// `name` at the tree's root) with `parents` and `message`, made from objects alone
    /// through a private index at `index`: the repository's own index and worktree are never
    /// touched.
    ///
    /// # Errors
    /// A git failure.
    pub fn commit_files(
        &self,
        files: &[(String, PathBuf)],
        parents: &[&str],
        index: &Path,
        message: &str,
        within: Duration,
    ) -> Result<String, String> {
        let mut entries = Vec::new();
        for (name, path) in files {
            let path = path.to_string_lossy();
            let oid = self.text(&["hash-object", "-w", "--no-filters", "--", &path], within)?;
            entries.push(format!("100644,{oid},{name}"));
        }
        let _ = std::fs::remove_file(index);
        let index_text = index.to_string_lossy();
        let private = [("GIT_INDEX_FILE", index_text.as_ref())];
        let mut args = vec!["update-index", "--add"];
        for entry in &entries {
            args.extend(["--cacheinfo", entry.as_str()]);
        }
        let tree = self
            .bytes_with(&args, &private, within)
            .and_then(|_| self.text_with(&["write-tree"], &private, within));
        let _ = std::fs::remove_file(index);
        let tree = tree?;
        let mut args = vec![
            "-c",
            "user.name=aterm-winship",
            "-c",
            "user.email=aterm-winship@users.noreply.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit-tree",
            tree.as_str(),
        ];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        args.extend(["-m", message]);
        self.text(&args, within)
    }

    /// Pushes `commit` to origin's `refname`; `force` replaces what is there. `Ok(false)`: the
    /// push was refused and origin's ref is there already (another run handed off first).
    ///
    /// # Errors
    /// The push failed otherwise.
    pub fn push(
        &self,
        commit: &str,
        refname: &str,
        force: bool,
        within: Duration,
    ) -> Result<bool, String> {
        let spec = format!("{}{commit}:{refname}", if force { "+" } else { "" });
        let pushed = self.bytes(
            &["push", "--no-recurse-submodules", "origin", &spec],
            within,
        );
        match pushed {
            Ok(_) => Ok(true),
            Err(e) if !force => match self.remote_ref(refname, within) {
                Ok(Some(_)) => Ok(false),
                _ => Err(e),
            },
            Err(e) => Err(e),
        }
    }
}

/// Builds a requested release.
pub trait Build {
    /// Builds `request`'s release commit into `out`, which does not exist yet: the zip and its
    /// receipt, as `release-build` writes them.
    ///
    /// # Errors
    /// Why there is no pair.
    fn build(&mut self, request: &Request, out: &Path, log: &mut Out) -> Result<(), String>;
}

/// The real build: the release commit checked out in the release tree
/// (`%LOCALAPPDATA%\aterm\release\src`, a worktree of the repository made like `update`'s
/// build tree) and built there by `tool`'s `release-build`, its output in a log of its own,
/// in a job that ends it and everything it started at [`BUILD_HANG`].
pub struct TreeBuild<'a> {
    /// The repository.
    pub repo: &'a Repo,
    /// Where the files go.
    pub layout: &'a Layout,
    /// This tool's own executable, which runs `release-build`.
    pub tool: PathBuf,
}

impl Build for TreeBuild<'_> {
    fn build(&mut self, request: &Request, out: &Path, log: &mut Out) -> Result<(), String> {
        let src = self.layout.release_src();
        crate::update::ensure_worktree("release-serve", self.repo, &src, log)?;
        // The release tree is this tool's alone: what a build left in it goes, but its
        // ignored target folder stays for the next build. The submodule's fetch may reach
        // the network, so these run as the poll's own git does: never prompting.
        let git = Git::for_runs(src.clone());
        git.bytes(
            &[
                "checkout",
                "--no-recurse-submodules",
                "--detach",
                "--force",
                &request.commit,
            ],
            GIT_LOCAL,
        )?;
        git.bytes(&["clean", "-ffdq"], GIT_LOCAL)?;
        git.bytes(
            &["submodule", "update", "--init", "vendor/astream"],
            GIT_NETWORK,
        )?;
        let file = self.layout.release_build_log(&request.key());
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let build = request.build.to_string();
        let args: [&std::ffi::OsStr; 11] = [
            "release-build".as_ref(),
            "--version".as_ref(),
            request.version.as_ref(),
            "--build-number".as_ref(),
            build.as_ref(),
            "--commit".as_ref(),
            request.commit.as_ref(),
            "--out".as_ref(),
            out.as_os_str(),
            "--repo".as_ref(),
            src.as_os_str(),
        ];
        log.say(&format!(
            "release-serve: building v{} build {} at {} in {}; its log is {}",
            request.version,
            request.build,
            &request.commit[..12],
            src.display(),
            file.display()
        ));
        let ran = proc::run_to_file(&self.tool, &args, &file, BUILD_HANG);
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        for line in &lines[lines.len().saturating_sub(LOG_TAIL)..] {
            log.say(&format!("  {line}"));
        }
        match ran? {
            Some(0) => Ok(()),
            Some(code) => Err(format!(
                "release-build exited {code}; its log is {}",
                file.display()
            )),
            None => Err(format!(
                "release-build ran past {} s and was ended with what it started; its log is {}",
                BUILD_HANG.as_secs(),
                file.display()
            )),
        }
    }
}

/// One poll's state.
pub struct Serve<'a> {
    /// git in the repository: where origin is fetched and the handoff committed.
    pub git: Git,
    /// Where the files go.
    pub layout: &'a Layout,
    /// The builder's PKCS#8 key.
    pub key: &'a [u8],
    /// The builder's id.
    pub builder: &'a str,
    /// How often the heartbeat is pushed while a build runs ([`HEARTBEAT_EVERY`]).
    pub heartbeat_every: Duration,
    /// The build.
    pub build: &'a mut dyn Build,
}

/// What a heartbeat says of the request: the request commit the poll read, and why it is not
/// built when it is not ([`Alive::request`], [`Alive::passed_over`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    request: Option<String>,
    passed_over: Option<PassedOver>,
}

/// What a poll made of origin's request (the module's steps 4 and 5).
enum Verdict {
    /// Build it.
    Build(Request),
    /// Its handoff is on origin already.
    Done(Request),
    /// Not built: why, the poll's exit code and its words.
    Passed(PassedOver, i32, String),
    /// Not judged at this poll (origin could not be read for the handoff): its words.
    Unjudged(String),
}

/// Pushes a heartbeat as `builder` saying `seen`, its files in `root` under `tag` (two
/// heartbeats at once use two tags). The time it says.
fn push_alive(
    git: &Git,
    root: &Path,
    builder: &str,
    seen: &Seen,
    tag: &str,
) -> Result<String, String> {
    let at = crate::pointer::now_rfc3339();
    let text = Alive {
        builder: builder.to_string(),
        at: at.clone(),
        request: seen.request.clone(),
        passed_over: seen.passed_over,
    }
    .to_toml()?;
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let file = root.join(format!("{tag}.alive.toml"));
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    let commit = git.commit_files(
        &[(pull::ALIVE_FILE.to_string(), file.clone())],
        &[],
        &root.join(format!("{tag}.alive.index")),
        &format!("release-serve: {builder} alive at {at}"),
        ALIVE_HANG,
    );
    let _ = std::fs::remove_file(&file);
    git.push(&commit?, pull::ALIVE_REF, true, ALIVE_HANG)?;
    Ok(at)
}

/// The last record of a `RELEASES.ledger`: its last line that is neither blank nor a comment.
fn ledger_tail(ledger: &str) -> Option<&str> {
    ledger
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Whether a `git diff-tree --name-status` entry is one a release commit makes: the ledger or
/// the changelog changed, or an entry file under `changes/` deleted.
fn release_change(status: &str, path: &str) -> bool {
    matches!((status, path), ("M", "RELEASES.ledger" | "CHANGELOG.md"))
        || (status == "D" && path.starts_with("changes/") && path.len() > "changes/".len())
}

impl Serve<'_> {
    /// Fetches origin's `main` and its request; whether there is a request.
    fn fetch(&self) -> Result<bool, String> {
        let found = self
            .git
            .remote_ref(pull::REQUEST_REF, GIT_NETWORK)?
            .is_some();
        let main = format!("+refs/heads/main:{MAIN}");
        let request = format!("+{}:{LOCAL_REQUEST}", pull::REQUEST_REF);
        let mut args = vec![
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            main.as_str(),
        ];
        if found {
            args.push(request.as_str());
        }
        self.git.bytes(&args, GIT_NETWORK)?;
        Ok(found)
    }

    /// The builders `rev` pins; none when it has no [`pull::BUILDERS_FILE`].
    fn builders_at(&self, rev: &str) -> Result<Builders, String> {
        let spec = format!("{rev}:{}", pull::BUILDERS_FILE);
        let there = self.git.ran(&["cat-file", "-e", &spec], &[], GIT_LOCAL)?;
        if there.code != Some(0) {
            return Ok(Builders::default());
        }
        let text = String::from_utf8(self.git.bytes(&["show", &spec], GIT_LOCAL)?)
            .map_err(|_| format!("{spec} is not UTF-8"))?;
        Builders::parse(&text).map_err(|e| format!("{rev}: {e}"))
    }

    /// The request commit fetched into [`LOCAL_REQUEST`].
    fn request_commit(&self) -> Result<String, String> {
        self.git.text(
            &[
                "rev-parse",
                "--verify",
                &format!("{LOCAL_REQUEST}^{{commit}}"),
            ],
            GIT_LOCAL,
        )
    }

    /// The request the request commit `commit` makes, held to its shape: one parent, and a
    /// tree of [`pull::REQUEST_FILE`] alone, which names that parent.
    fn request_at(&self, commit: &str) -> Result<Request, String> {
        let parents = self
            .git
            .text(&["rev-list", "--parents", "-n", "1", commit], GIT_LOCAL)?;
        let words: Vec<&str> = parents.split_whitespace().collect();
        let [_, parent] = words[..] else {
            return Err(format!(
                "the request commit {commit} has {} parents: a request's one parent is the \
                 release commit",
                words.len().saturating_sub(1)
            ));
        };
        let listing = self
            .git
            .text(&["ls-tree", "-z", "--full-tree", commit], GIT_LOCAL)?;
        let entries: Vec<&str> = listing.split('\0').filter(|e| !e.is_empty()).collect();
        let [entry] = entries[..] else {
            return Err(format!(
                "the request commit {commit} holds {} entries, not {} alone",
                entries.len(),
                pull::REQUEST_FILE
            ));
        };
        let (head, name) = entry
            .split_once('\t')
            .ok_or_else(|| format!("git ls-tree wrote {entry:?}"))?;
        let fields: Vec<&str> = head.split_whitespace().collect();
        let [mode, kind, oid] = fields[..] else {
            return Err(format!("git ls-tree wrote {entry:?}"));
        };
        if mode != "100644" || kind != "blob" || name != pull::REQUEST_FILE {
            return Err(format!(
                "the request commit {commit} holds {name:?} ({mode} {kind}), not {} alone",
                pull::REQUEST_FILE
            ));
        }
        let size: u64 = self
            .git
            .text(&["cat-file", "-s", oid], GIT_LOCAL)?
            .parse()
            .map_err(|_| "git cat-file -s wrote no size".to_string())?;
        if size > pull::RECORD_MAX_BYTES {
            return Err(format!("the request is {size} bytes, oversized"));
        }
        let text = String::from_utf8(self.git.bytes(&["cat-file", "blob", oid], GIT_LOCAL)?)
            .map_err(|_| "the request is not UTF-8".to_string())?;
        let request = Request::parse(&text)?;
        if request.commit != parent {
            return Err(format!(
                "the request names the commit {}, and the request commit's parent is {parent}",
                request.commit
            ));
        }
        Ok(request)
    }

    /// The release commit is one the release cutter makes of origin/main's code (the
    /// module's step 4).
    fn check_release_commit(&self, request: &Request) -> Result<(), String> {
        let c = request.commit.as_str();
        let parents = self
            .git
            .text(&["rev-list", "--parents", "-n", "1", c], GIT_LOCAL)?;
        let words: Vec<&str> = parents.split_whitespace().collect();
        let [_, source] = words[..] else {
            return Err(format!(
                "the release commit {c} has {} parents, not one",
                words.len().saturating_sub(1)
            ));
        };
        let on_main = self.git.ran(
            &["merge-base", "--is-ancestor", source, MAIN],
            &[],
            GIT_LOCAL,
        )?;
        if on_main.code != Some(0) {
            return Err(format!(
                "the release commit's parent {source} is not on origin/main: a release is cut \
                 from published code"
            ));
        }
        let tree = self.git.text(
            &["rev-parse", "--verify", &format!("{c}^{{tree}}")],
            GIT_LOCAL,
        )?;
        if tree != request.tree {
            return Err(format!(
                "the request names the tree {}, and the release commit's is {tree}",
                request.tree
            ));
        }
        let diff = self.git.text(
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--name-status",
                source,
                c,
            ],
            GIT_LOCAL,
        )?;
        let fields: Vec<&str> = diff.split('\0').filter(|f| !f.is_empty()).collect();
        if !fields.len().is_multiple_of(2) {
            return Err(format!("git diff-tree wrote {diff:?}"));
        }
        for pair in fields.chunks(2) {
            if let [status, path] = pair
                && !release_change(status, path)
            {
                return Err(format!(
                    "the release commit {status} {path}: a release commit changes only \
                     RELEASES.ledger and CHANGELOG.md and deletes entry files under changes/"
                ));
            }
        }
        let ledger = String::from_utf8(
            self.git
                .bytes(&["show", &format!("{c}:RELEASES.ledger")], GIT_LOCAL)?,
        )
        .map_err(|_| "the release commit's RELEASES.ledger is not UTF-8".to_string())?;
        let want = format!("{} {}", request.build, request.version);
        if ledger_tail(&ledger) != Some(want.as_str()) {
            return Err(format!(
                "the release commit's ledger ends in {:?}, not the request's {want:?}",
                ledger_tail(&ledger)
            ));
        }
        let manifest = self
            .git
            .text(&["show", &format!("{c}:Cargo.toml")], GIT_LOCAL)?;
        crate::release::check_stamped(&manifest, &request.version)
    }

    /// Runs the build of `request` (read from the request commit `asked`) while a second thread
    /// pushes the heartbeat every [`Serve::heartbeat_every`].
    fn build_alive(
        &mut self,
        request: &Request,
        asked: &str,
        out_dir: &Path,
        out: &mut Out,
    ) -> Result<(), String> {
        let (stop, stopped) = mpsc::channel::<()>();
        let seen = Seen {
            request: Some(asked.to_string()),
            passed_over: None,
        };
        let (git, root, builder, every) = (
            self.git.clone(),
            self.layout.release_root(),
            self.builder.to_string(),
            self.heartbeat_every,
        );
        let beat = std::thread::spawn(move || {
            let mut missed = Vec::new();
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                if let Err(e) = push_alive(&git, &root, &builder, &seen, "beat") {
                    missed.push(e);
                }
            }
            missed
        });
        let built = self.build.build(request, out_dir, out);
        drop(stop);
        let missed = beat.join().unwrap_or_default();
        if let Some(first) = missed.first() {
            out.say(&format!(
                "release-serve: {} heartbeats during the build were not pushed; the first: {first}",
                missed.len()
            ));
        }
        built
    }

    /// Admits the pair the build wrote in `out_dir` as the cutter will, signs it, lays the
    /// handoff out and pushes it. Whether this run pushed it (`false`: another run had).
    fn hand_off(&self, request: &Request, out_dir: &Path, out: &mut Out) -> Result<bool, String> {
        let (stock, trust_commit) = crate::release::commit_pins(&self.git.dir, &request.commit)?;
        let pins = Pins {
            stock: &stock,
            trust_commit: trust_commit.as_deref(),
        };
        let cut = Cut {
            version: &request.version,
            build: request.build,
            commit: &request.commit,
            source_tree: &request.tree,
        };
        let receipt = handoff::admit(out_dir, &cut, &pins)
            .map_err(|e| format!("the build's pair is not admitted: {e}"))?;
        let asset = receipt.asset.clone();
        if std::fs::symlink_metadata(out_dir.join(pull::attestation_name(&asset))).is_err() {
            attest::write_beside(out_dir, &asset, self.key, self.builder)?;
        }
        // What the cut will check: this receipt, attested by a builder C pins.
        let builders = self.builders_at(&request.commit)?;
        let attested = attest::verify_beside(out_dir, &asset, &builders, &pull::today())
            .map_err(|e| format!("the attestation is refused: {e}"))?;
        let key = request.key();
        let stage = self.layout.release_stage(&key);
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
        let parts = pull::split(&out_dir.join(&asset), &stage, pull::PART_MAX_BYTES)?;
        for name in [
            handoff::receipt_name(&asset),
            pull::attestation_name(&asset),
        ] {
            std::fs::copy(out_dir.join(&name), stage.join(&name))
                .map_err(|e| format!("{}: {e}", stage.join(&name).display()))?;
        }
        let files: Vec<(String, PathBuf)> = parts
            .names()
            .into_iter()
            .map(|name| {
                let path = stage.join(&name);
                (name, path)
            })
            .collect();
        let commit = self.git.commit_files(
            &files,
            &[],
            &self.layout.release_root().join("handoff.index"),
            &format!(
                "windows handoff: v{} build {} at {} by {}",
                request.version,
                request.build,
                &request.commit[..12],
                attested.builder
            ),
            GIT_NETWORK,
        );
        let _ = std::fs::remove_dir_all(&stage);
        let commit = commit?;
        let refname = request.handoff_ref();
        let pushed = self.git.push(&commit, &refname, false, GIT_NETWORK)?;
        out.say(&format!(
            "release-serve: {} {refname} ({commit}): {} ({} bytes, sha256 {}) in {} parts, \
             attested by {}",
            if pushed {
                "handed off as"
            } else {
                "another run handed off first:"
            },
            asset,
            parts.size,
            parts.sha256,
            parts.part.len(),
            attested.builder
        ));
        Ok(pushed)
    }

    fn attempts(&self, key: &str) -> u32 {
        std::fs::read_to_string(self.layout.release_attempts(key))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    fn record_attempt(&self, key: &str, attempts: u32) -> Result<(), String> {
        let path = self.layout.release_attempts(key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        aterm_uds::atomic_file::write(&path, format!("{attempts}\n").as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// What this builder, of public key `public`, makes on `today` of the request commit
    /// `asked` (the module's steps 4 and 5): its shape, whether its handoff is on origin, its
    /// release commit, whether that commit pins this builder, and the builds it had.
    fn judge(&self, asked: &str, public: &str, today: &str, out: &mut Out) -> Verdict {
        let refused = |e: String| {
            Verdict::Passed(
                PassedOver::Refused,
                FAILED,
                format!("the request is refused, nothing built: {e}"),
            )
        };
        let request = match self.request_at(asked) {
            Ok(request) => request,
            Err(e) => return refused(e),
        };
        out.say(&format!(
            "release-serve: the request is v{} build {} at {} (reserved {})",
            request.version, request.build, request.commit, request.reserved_at
        ));
        match self.git.remote_ref(&request.handoff_ref(), GIT_NETWORK) {
            Ok(Some(_)) => return Verdict::Done(request),
            Ok(None) => {}
            Err(e) => return Verdict::Unjudged(format!("not run: {e}")),
        }
        if let Err(e) = self.check_release_commit(&request) {
            return refused(e);
        }
        if let Err(e) = self.builders_at(&request.commit).and_then(|pinned| {
            pinned
                .admits(self.builder, public, today)
                .map(drop)
                .map_err(|e| e.to_string())
        }) {
            return Verdict::Passed(
                PassedOver::Unpinned,
                NOT_RUN,
                format!(
                    "not building: the release commit does not admit this builder ({}, key \
                     {public}): {e}",
                    self.builder
                ),
            );
        }
        let key = request.key();
        let attempts = self.attempts(&key);
        if attempts >= MAX_ATTEMPTS {
            return Verdict::Passed(
                PassedOver::Spent,
                FAILED,
                format!(
                    "{key} was built {attempts} times without a handoff; passed over until the \
                     cut reserves again (the logs are in {})",
                    self.layout.release_root().join("logs").display()
                ),
            );
        }
        Verdict::Build(request)
    }

    /// Deletes origin's handoff refs of every request but `request`'s, each only while it
    /// still names what was listed: a builder that took a newer request has no use left for
    /// an older one's handoff, and each holds a whole zip. One it cannot delete is said and
    /// left for a later poll.
    fn prune(&self, request: &Request, out: &mut Out) {
        let keep = request.handoff_ref();
        let pattern = format!("{}*", pull::HANDOFF_REF_PREFIX);
        let listing = match self
            .git
            .text(&["ls-remote", "origin", &pattern], GIT_NETWORK)
        {
            Ok(listing) => listing,
            Err(e) => {
                out.say(&format!(
                    "release-serve: older handoffs are left on origin: {e}"
                ));
                return;
            }
        };
        for line in listing.lines() {
            let Some((oid, name)) = line.split_once('\t') else {
                continue;
            };
            let (oid, name) = (oid.trim(), name.trim());
            if name == keep || !name.starts_with(pull::HANDOFF_REF_PREFIX) {
                continue;
            }
            let lease = format!("--force-with-lease={name}:{oid}");
            let delete = format!(":{name}");
            match self.git.bytes(
                &["push", "--no-recurse-submodules", &lease, "origin", &delete],
                GIT_NETWORK,
            ) {
                Ok(_) => out.say(&format!(
                    "release-serve: deleted {name}, an older request's handoff"
                )),
                Err(e) => out.say(&format!("release-serve: {name} is left on origin: {e}")),
            }
        }
    }

    /// Builds `request`, read from the request commit `asked`, and hands it off: the exit
    /// code.
    fn handle(&mut self, request: &Request, asked: &str, out: &mut Out) -> i32 {
        let key = request.key();
        let attempts = self.attempts(&key);
        let dist = self.layout.dist_root();
        let _update = match std::fs::create_dir_all(&dist)
            .map_err(|e| format!("{}: {e}", dist.display()))
            .and_then(|()| {
                aterm_uds::file_lock::LockFile::take(
                    &self.layout.update_lock(),
                    Instant::now() + Duration::from_secs(2),
                    "aterm-winship update is building",
                )
                .map_err(|e| e.to_string())
            }) {
            Ok(lock) => lock,
            Err(e) => {
                out.say(&format!(
                    "release-serve: not building now ({e}); the next poll tries again"
                ));
                return NOT_RUN;
            }
        };
        if let Err(e) = self.record_attempt(&key, attempts + 1) {
            out.say(&format!("release-serve: not run: {e}"));
            return NOT_RUN;
        }
        let root = self.layout.release_root();
        for leftover in [root.join("out"), root.join("stage")] {
            if let Err(e) = std::fs::remove_dir_all(&leftover)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                out.say(&format!(
                    "release-serve: not run: {}: {e}",
                    leftover.display()
                ));
                return NOT_RUN;
            }
        }
        let out_dir = self.layout.release_out(&key);
        if let Some(dir) = out_dir.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            out.say(&format!("release-serve: not run: {}: {e}", dir.display()));
            return NOT_RUN;
        }
        if let Err(e) = self.build_alive(request, asked, &out_dir, out) {
            out.say(&format!(
                "release-serve: the build failed (attempt {} of {MAX_ATTEMPTS}): {e}",
                attempts + 1
            ));
            return FAILED;
        }
        match self.hand_off(request, &out_dir, out) {
            Ok(_) => PASS,
            Err(e) => {
                out.say(&format!(
                    "release-serve: nothing handed off (attempt {} of {MAX_ATTEMPTS}): {e}",
                    attempts + 1
                ));
                FAILED
            }
        }
    }
}

/// One poll (the module's steps): the exit code. 0 the poll did its part (a heartbeat, and a
/// handoff when one was asked for and is not there yet); 1 a request was refused, a build or
/// a push failed; 3 it could not run (no key, origin unreachable, this builder not pinned, a
/// build already running).
pub fn serve_once(serve: &mut Serve<'_>, out: &mut Out) -> i32 {
    let public = match attest::public_key(serve.key) {
        Ok(public) => public,
        Err(e) => {
            out.say(&format!("release-serve: not run: {e}"));
            return NOT_RUN;
        }
    };
    let found = match serve.fetch() {
        Ok(found) => found,
        Err(e) => {
            out.say(&format!("release-serve: not run: {e}"));
            return NOT_RUN;
        }
    };
    let today = pull::today();
    if let Err(e) = serve.builders_at(MAIN).and_then(|pinned| {
        pinned
            .admits(serve.builder, &public, &today)
            .map(drop)
            .map_err(|e| e.to_string())
    }) {
        out.say(&format!(
            "release-serve: not serving, and no heartbeat: origin/main does not pin this \
             builder ({e}); to serve, pin it in {}:\n[[builder]]\nid = \"{}\"\npublic_key = \
             \"{public}\"\nnot_after = \"YYYY-MM-DD\"",
            pull::BUILDERS_FILE,
            serve.builder
        ));
        return NOT_RUN;
    }
    // The request is judged before the heartbeat goes, so the heartbeat says what this
    // builder made of it: a cut waits on a builder that builds its request, never on one that
    // passed it over.
    let judged = found.then(|| match serve.request_commit() {
        Ok(asked) => {
            let verdict = serve.judge(&asked, &public, &today, out);
            (Some(asked), verdict)
        }
        Err(e) => (
            None,
            Verdict::Passed(
                PassedOver::Refused,
                FAILED,
                format!("the request is refused, nothing built: {e}"),
            ),
        ),
    });
    let seen = match &judged {
        Some((asked, verdict)) => Seen {
            request: asked.clone(),
            passed_over: match verdict {
                Verdict::Passed(over, ..) => Some(*over),
                _ => None,
            },
        },
        None => Seen::default(),
    };
    let mut code = PASS;
    match push_alive(
        &serve.git,
        &serve.layout.release_root(),
        serve.builder,
        &seen,
        "poll",
    ) {
        Ok(at) => out.say(&format!(
            "release-serve: {} is alive at {at}",
            serve.builder
        )),
        Err(e) => {
            out.say(&format!("release-serve: the heartbeat was not pushed: {e}"));
            code = FAILED;
        }
    }
    let Some((asked, verdict)) = judged else {
        out.say("release-serve: origin holds no request");
        return code;
    };
    let request = match verdict {
        Verdict::Build(request) => request,
        Verdict::Done(request) => {
            serve.prune(&request, out);
            out.say(&format!(
                "release-serve: {} is on origin already; nothing to build",
                request.handoff_ref()
            ));
            return code;
        }
        Verdict::Passed(_, exit, words) => {
            out.say(&format!("release-serve: {words}"));
            return exit;
        }
        Verdict::Unjudged(words) => {
            out.say(&format!("release-serve: {words}"));
            return NOT_RUN;
        }
    };
    serve.prune(&request, out);
    let asked = asked.unwrap_or_default();
    match serve.handle(&request, &asked, out) {
        PASS => code,
        other => other,
    }
}

/// Moves `log` aside to `<log>.old` when it has grown past [`LOG_MAX_BYTES`], so a poll every
/// ten minutes keeps at most two of them.
pub fn rotate(log: &Path) {
    if std::fs::metadata(log).is_ok_and(|meta| meta.len() > LOG_MAX_BYTES) {
        let mut old = log.as_os_str().to_os_string();
        old.push(".old");
        let _ = std::fs::rename(log, PathBuf::from(old));
    }
}

/// One poll as the verb runs it: the poll lock, the key, the builder id and the repository
/// decided from `opts` and `env`, then [`serve_once`] with [`TreeBuild`].
fn poll(opts: &Options, layout: &Layout, env: &[(String, String)], out: &mut Out) -> i32 {
    let root = layout.release_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        out.say(&format!("release-serve: not run: {}: {e}", root.display()));
        return NOT_RUN;
    }
    let _lock = match aterm_uds::file_lock::LockFile::take(
        &layout.serve_lock(),
        Instant::now() + Duration::from_secs(2),
        "another release-serve is running",
    ) {
        Ok(lock) => lock,
        Err(e) => {
            out.say(&format!("release-serve: not run: {e}"));
            return NOT_RUN;
        }
    };
    let ready = (|| {
        let path = opts
            .key
            .clone()
            .or_else(|| attest::default_key(env))
            .ok_or("the environment has no USERPROFILE: name the key with --key <path>")?;
        let key = attest::read_key(&path).map_err(|e| {
            format!("{e} (make the builder key with `aterm-winship release-key init`)")
        })?;
        let builder = match &opts.builder {
            Some(builder) => builder.clone(),
            None => attest::default_builder(env)?,
        };
        let cwd = std::env::current_dir().map_err(|e| format!("the working directory: {e}"))?;
        let repo = repo::resolve(opts.repo.as_deref(), layout, &cwd)?;
        let tool = std::env::current_exe().map_err(|e| format!("this tool's own path: {e}"))?;
        Ok::<_, String>((key, builder, repo, tool))
    })();
    let (key, builder, repo, tool) = match ready {
        Ok(ready) => ready,
        Err(e) => {
            out.say(&format!("release-serve: not run: {e}"));
            return NOT_RUN;
        }
    };
    let mut build = TreeBuild {
        repo: &repo,
        layout,
        tool,
    };
    let mut serve = Serve {
        git: Git::for_runs(repo.dir.clone()),
        layout,
        key: &key,
        builder: &builder,
        heartbeat_every: HEARTBEAT_EVERY,
        build: &mut build,
    };
    serve_once(&mut serve, out)
}

/// `release-serve`: one poll with `--once`, else a poll every [`POLL`] until ended. The exit
/// code of the (last) poll.
pub fn serve(opts: &Options, layout: &Layout, env: &[(String, String)], out: &mut Out) -> i32 {
    loop {
        let code = poll(opts, layout, env, out);
        out.say(&format!("release-serve: exit {code}"));
        if opts.once {
            return code;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;

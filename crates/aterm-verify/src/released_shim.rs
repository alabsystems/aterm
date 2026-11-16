// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bootstrap compatibility is decided by a pair, a `targo` shim's text and the helper it names,
//! run in a disposable home. Two pairs are judged, each by its own check.
//!
//! `released-shim` runs the installed public release's pair, never candidate atpkg code.
//! Relocation changes only the shim's baked store path. The helper stays the installed image.
//!
//! `candidate-shim` runs this tree's pair: the text this tree's atpkg lays for the store's
//! `targo`, naming the `atpkg` this tree built, once under each placement the shim can select
//! (`mac::Placement`). A pair no home can build with is red there, before it is a release: the
//! installed pair shows a defect only once it is installed.
//!
//! All leases, ownership records, Cargo state and artifacts go to scratch; SBPL enforces that
//! the real toolchain, registry and build caches cannot be written, even by build scripts.
//! The helper retains its normal account-wide capacity-ledger bookkeeping.

use crate::{Ctx, Scope, ladder::Report};

fn bootstrap_input(path: &str, confined: &[String]) -> bool {
    if path.split('/').any(|part| part == ".cargo")
        || matches!(
            path.rsplit('/').next().unwrap_or(path),
            "Cargo.toml" | "Cargo.lock" | "trust.lock" | "rust-toolchain" | "rust-toolchain.toml"
        )
    {
        return true;
    }
    let package = [
        ("crates/atpkg/", "atpkg"),
        ("crates/aterm-toolchain/", "aterm-toolchain"),
        ("crates/aterm-tempfile/", "aterm-tempfile"),
        ("crates/aterm-release/", "aterm-release"),
        ("crates/aterm-verify/src/released_shim", "aterm-verify"),
    ]
    .iter()
    .find(|(prefix, _)| path.starts_with(prefix))
    .map(|(_, name)| *name);
    match package {
        // The cone's `alone` evidence proves that no build without tests reads changed
        // code or bytes. A filename alone supplies no such evidence; uncertainty runs the rig.
        Some(name) => !path.ends_with(".rs") || !confined.iter().any(|member| member == name),
        None => path.starts_with("tools/install.sh"),
    }
}

/// The line of a refused build that says why.
#[cfg(target_os = "macos")]
fn refusal(output: &crate::exec::MachineRun) -> String {
    output
        .run
        .output
        .lines()
        .find(|line| line.contains("refused") || line.starts_with("error:"))
        .unwrap_or("see child output")
        .to_owned()
}

/// What makes the judgment owed where no change cone narrows it: `verify released-shim`, `-p`,
/// `--all`, and a release's early ask of its own tree ([`judge`]).
pub const WHOLE_TREE: &str = "explicit selection (all bootstrap inputs)";

/// How the judgment of one checkout ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The installed public release built the checkout.
    Accepted,
    /// The installed public release refused it: red.
    Refused,
    /// Nothing was decided (no installed public release that may judge, no fetched public tag,
    /// a rig that could not be made): COULD NOT RUN, never green.
    Undecided,
    /// This host is no Mac: the lane is judged on one.
    NotThisHost,
}

/// The judgment of one checkout: how it ended, the words its row says, and what the judging
/// build wrote (its stderr; its stdout too where it refused).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judged {
    pub verdict: Verdict,
    pub row: String,
    pub output: String,
}

/// THE RELEASED-SHIM JUDGMENT of the checkout at `root`: the installed public release's shim
/// and helper build its tracked files' `aterm-time` offline in a disposable home. `inputs`
/// names what makes it owed, for its row. The check's stage ([`run`]) and a release's early ask
/// of its own tree (`ship release`, step 1) share the supervised judgment and its words.
/// What it finds depends on which release is public and installed as well as on the tree: it
/// can turn red with no change to the tree.
#[must_use]
pub fn judge(root: &std::path::Path, env: &crate::EnvSnapshot, inputs: &[String]) -> Judged {
    judge_with_exec(
        root,
        env,
        inputs,
        crate::exec::ExecEnv {
            events: None,
            cwd: root,
            path: &env.path,
            scratch: root,
            child_ceiling: None,
            remove_env: &[],
            add_env: &[],
            stage_groups: None,
            host: &crate::exec::LIVE_HOST,
            hang_samples: None,
        },
    )
}

fn judge_with_exec(
    root: &std::path::Path,
    env: &crate::EnvSnapshot,
    inputs: &[String],
    execution: crate::exec::ExecEnv<'_>,
) -> Judged {
    #[cfg(target_os = "macos")]
    {
        match mac::judge(root, env, execution) {
            Ok((judging, output)) if output.run.ok => Judged {
                verdict: Verdict::Accepted,
                row: format!(
                    "released-shim: {} built aterm-time; inputs: {}",
                    judging.who(),
                    inputs.join(", ")
                ),
                output: output.run.output,
            },
            Ok((judging, output)) => {
                let diagnostic = format!(
                    "{}{}",
                    output.stdout.as_deref().unwrap_or(""),
                    output.run.output
                );
                let Some(code) = output.run.code else {
                    return Judged {
                        verdict: Verdict::Undecided,
                        row: format!(
                            "released-shim: {} bootstrap did not complete; see child output",
                            judging.who()
                        ),
                        output: diagnostic,
                    };
                };
                Judged {
                    verdict: Verdict::Refused,
                    row: format!(
                        "released-shim: {} refuses candidate (exit status: {code}); incompatible bootstrap inputs: \
                         {}; {}; land shim support first, release it, then adopt the configuration",
                        judging.who(),
                        inputs.join(", "),
                        refusal(&output)
                    ),
                    output: diagnostic,
                }
            }
            Err(why) => Judged {
                verdict: Verdict::Undecided,
                row: format!("released-shim: {why}"),
                output: String::new(),
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, env, inputs, execution);
        Judged {
            verdict: Verdict::NotThisHost,
            row: "released-shim: macOS installed-app bootstrap lane (run on a Mac)".into(),
            output: String::new(),
        }
    }
}

pub(crate) fn run(ctx: &Ctx, report: &mut Report) {
    let inputs = match &ctx.scope {
        Scope::Changed(change) => {
            match crate::changed::changed_since(&ctx.root, &ctx.path_env, &change.base) {
                Ok(paths) => paths
                    .into_iter()
                    .filter(|p| bootstrap_input(p, &change.alone))
                    .collect(),
                Err(why) => {
                    report.cannot_run(why);
                    return;
                }
            }
        }
        _ => vec![WHOLE_TREE.to_owned()],
    };
    if inputs.is_empty() {
        report.pass("released-shim: no executable bootstrap inputs changed; no child build");
        return;
    }
    let judged = judge_with_exec(&ctx.root, &ctx.env, &inputs, ctx.exec_env());
    report.raw(judged.output);
    match judged.verdict {
        Verdict::Accepted => report.pass(judged.row),
        Verdict::Refused => report.fail(judged.row),
        Verdict::Undecided => report.cannot_run(judged.row),
        Verdict::NotThisHost => report.skip(judged.row),
    }
}

/// The name of the helper `candidate-shim` builds from this tree: atpkg's own `[[bin]]`, whose
/// `__build_admit` front is the one the app's `atpkg` runs.
pub(crate) const CANDIDATE_HELPER_BIN: &str = "atpkg";

/// Why `candidate-shim` builds and judges nothing on this OS, where it does not: the pair it
/// judges is macOS's.
pub(crate) const CANDIDATE_ELSEWHERE: Option<&str> = if cfg!(target_os = "macos") {
    None
} else {
    Some("candidate-shim: macOS bootstrap lane (run on a Mac)")
};

/// This tree's pair, `helper` being the `atpkg` it built, judged under each placement: one row
/// each, and a red under one does not keep the others from being judged.
pub(crate) fn run_candidate(ctx: &Ctx, report: &mut Report, helper: &std::path::Path) {
    #[cfg(target_os = "macos")]
    mac::candidate(&ctx.root, &ctx.env, helper, ctx.exec_env(), report);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ctx, helper);
        report.skip(CANDIDATE_ELSEWHERE.unwrap_or_default());
    }
}

#[cfg(test)]
#[path = "released_shim_tests.rs"]
mod tests;

/// The fetched public release tags the check picks its judging release from
/// ([`mac::public_releases`]): what the land seal names as the check's release tags
/// ([`crate::land_seal::RELEASE_TAGS`]).
///
/// # Errors
/// The tags could not be listed, or the workspace version is no release version.
#[cfg(target_os = "macos")]
pub(crate) fn public_releases(
    root: &std::path::Path,
    env: &crate::EnvSnapshot,
    head: &str,
    current: &str,
) -> Result<Vec<String>, String> {
    mac::public_releases(root, env, head, current)
}

#[cfg(target_os = "macos")]
mod mac {
    use crate::{EnvSnapshot, exec};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    fn io<T, E: std::fmt::Display>(value: Result<T, E>) -> Result<T, String> {
        value.map_err(|e| e.to_string())
    }

    /// `MAJOR.MINOR.0` of a public release's version or `v`-tag; `None` for anything else.
    fn release_version(text: &str) -> Option<(u64, u64, u64)> {
        let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
        let version = (
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        (parts.next().is_none() && version.2 == 0).then_some(version)
    }

    /// One fetched public release tag (`vMAJOR.MINOR.0`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct PublicTag {
        /// Its version, `X.Y.0`, without the `v`.
        pub(super) version: String,
        /// Whether the candidate holds it: the commit it names is in `head`'s ancestry.
        pub(super) held: bool,
    }

    /// The names of `root`'s `refs/tags/v*`, or of those merged into `merged` where it is given.
    fn tag_names(
        root: &Path,
        env: &EnvSnapshot,
        merged: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut cmd = crate::git_provider::command();
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        cmd.current_dir(root)
            .env("PATH", &env.path)
            .arg("for-each-ref");
        if let Some(head) = merged {
            cmd.arg(format!("--merged={head}"));
        }
        cmd.args(["--format=%(refname:strip=2)", "refs/tags/v*"]);
        // A hang detector, as for the listing below.
        let out = io(aterm_uds::deadline::output_while_process_progressing(
            &mut cmd,
            Duration::from_secs(60),
            "the candidate's release tags",
        ))?;
        if !out.status.success() {
            return Err(format!(
                "git for-each-ref exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listed = String::from_utf8(out.stdout)
            .map_err(|_| "git for-each-ref returned non-UTF-8 text".to_string())?;
        Ok(listed.lines().map(str::to_owned).collect())
    }

    /// Every public release tag (`vMAJOR.MINOR.0`) this clone has fetched, whether `head` holds
    /// it or not, by version, oldest first.
    pub(super) fn public_tags(
        root: &Path,
        env: &EnvSnapshot,
        head: &str,
    ) -> Result<Vec<PublicTag>, String> {
        let held: std::collections::BTreeSet<String> =
            tag_names(root, env, Some(head))?.into_iter().collect();
        let mut tags: Vec<_> = tag_names(root, env, None)?
            .into_iter()
            .filter_map(|tag| {
                let key = release_version(&tag)?;
                Some((
                    key,
                    PublicTag {
                        version: tag.trim_start_matches('v').to_owned(),
                        held: held.contains(&tag),
                    },
                ))
            })
            .collect();
        tags.sort_unstable_by_key(|(key, _)| *key);
        Ok(tags.into_iter().map(|(_, tag)| tag).collect())
    }

    /// The versions of `tags` the candidate holds and that are no newer than its workspace
    /// version `current`: the releases it may be judged with, oldest first.
    fn held_releases<'a>(tags: &'a [PublicTag], current: &str) -> Result<Vec<&'a str>, String> {
        let current = release_version(current)
            .ok_or_else(|| format!("workspace version {current:?} is not MAJOR.MINOR.0"))?;
        Ok(tags
            .iter()
            .filter(|tag| tag.held && release_version(&tag.version).is_some_and(|v| v <= current))
            .map(|tag| tag.version.as_str())
            .collect())
    }

    /// Every fetched public release tag (`vMAJOR.MINOR.0`) no newer than the workspace version
    /// `current` and reachable from `head`, by version, oldest first.
    pub(super) fn public_releases(
        root: &Path,
        env: &EnvSnapshot,
        head: &str,
        current: &str,
    ) -> Result<Vec<String>, String> {
        let tags = public_tags(root, env, head)?;
        Ok(held_releases(&tags, current)?
            .into_iter()
            .map(str::to_owned)
            .collect())
    }

    fn git_files(root: &Path, env: &EnvSnapshot) -> Result<Vec<PathBuf>, String> {
        let mut cmd = crate::git_provider::command();
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        cmd.current_dir(root).env("PATH", &env.path).args([
            "ls-files",
            "--recurse-submodules",
            "-z",
        ]);
        // A hang detector, not a latency budget: the listing may take as long as a loaded Mac
        // makes it, and is ended only once git's output and its process tree's CPU and I/O stay
        // still for a minute. Every NUL-ended path is kept, within 16 MiB; an unended last one
        // is refused.
        let out = io(
            aterm_uds::deadline::output_while_process_progressing_complete_stdout_records_unless(
                &mut cmd,
                Duration::from_secs(60),
                "the candidate's git ls-files listing",
                b'\0',
                16 << 20,
                |_| true,
                &mut || false,
            ),
        )?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        let files: Vec<PathBuf> = String::from_utf8(out.stdout)
            .map_err(|e| e.to_string())
            .map(|s| {
                s.split('\0')
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })?;
        if !files.iter().any(|p| p == Path::new("Cargo.toml")) {
            return Err("candidate listing has no root Cargo.toml".into());
        }
        Ok(files)
    }

    fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
        io(fs::create_dir_all(to.parent().ok_or("file has no parent")?))?;
        let meta = io(fs::symlink_metadata(from))?;
        if meta.file_type().is_symlink() {
            io(std::os::unix::fs::symlink(io(fs::read_link(from))?, to))
        } else {
            io(fs::copy(from, to)).map(|_| ())
        }
    }

    /// Only installed store paths are rebased; do not regenerate or reinterpret release rules.
    fn relocate(text: &str, from: &Path, to: &Path) -> Result<String, String> {
        let from = from.to_str().ok_or("non-UTF-8 installed prefix")?;
        let to = to.to_str().ok_or("non-UTF-8 scratch prefix")?;
        if !text.contains(from) || to.contains(['\'', '\n', '\r']) {
            return Err("installed shim has no relocatable store path".into());
        }
        Ok(text.replace(from, to))
    }

    /// The version an installed app's `--version` line names when it is a public release:
    /// `aterm X.Y.0` with no development metadata.
    fn public_version(line: &str) -> Option<&str> {
        line.strip_prefix("aterm ")
            .map(str::trim)
            .filter(|version| !version.is_empty() && !version.contains(['+', ' ']))
    }

    /// The installed public release a judgment runs with.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct Judging {
        /// Its version, `X.Y.0`.
        pub(super) version: String,
        /// The newest public tag the candidate holds at or before its workspace version, where
        /// the installed app is the release immediately before it.
        pub(super) newer: Option<String>,
    }

    impl Judging {
        /// Which release judged, and that a newer one exists where it does.
        pub(super) fn who(&self) -> String {
            match &self.newer {
                None => format!("aterm {}", self.version),
                Some(newer) => format!(
                    "aterm {} (the release before v{newer}, the newest public tag the candidate \
                     holds at or before its workspace version; update the installed app to \
                     judge with v{newer})",
                    self.version
                ),
            }
        }
    }

    /// WHICH INSTALLED APP MAY JUDGE: a public release that is the newest public tag the
    /// candidate holds at or before its workspace version, or the one immediately before it.
    /// `installed` is the first line of the app's `--version`, `workspace` the candidate's
    /// workspace version, and `tags` every public tag this clone fetched, oldest first, each
    /// with whether the candidate holds it ([`public_tags`]). In the hours after a release is
    /// published a Mac that has fetched its tag and not yet updated still judges, and its row
    /// says with which release. Nothing else judges, and each refusal names its own cause and
    /// the act that clears it: a development build (install a public release); a release two or
    /// more behind that newest tag (update the installed app); and a release those tags do not
    /// place, by where its own tag stands: not fetched (fetch the tags), fetched outside the
    /// candidate's ancestry (merge origin/main where origin/main holds that tag; or else install
    /// the newest release the candidate holds at or before its workspace version, where it
    /// holds one), or in that ancestry past the candidate's workspace version (take
    /// origin/main's version back).
    ///
    /// # Errors
    /// As above, each with its remedy; and a workspace version that is not `MAJOR.MINOR.0`.
    pub(super) fn judging_release(
        installed: &str,
        workspace: &str,
        tags: &[PublicTag],
    ) -> Result<Judging, String> {
        let Some((version, key)) =
            public_version(installed).and_then(|v| Some((v, release_version(v)?)))
        else {
            return Err(format!(
                "installed {installed:?} is a development or unreadable build, not a public \
                 release; install the newest public release before judging bootstrap compatibility"
            ));
        };
        let releases = held_releases(tags, workspace)?;
        let newer: Vec<&str> = releases
            .iter()
            .copied()
            .filter(|tag| release_version(tag).is_some_and(|v| v > key))
            .collect();
        let placed = releases.iter().any(|tag| release_version(tag) == Some(key));
        match (placed, newer.as_slice()) {
            (true, []) => {
                return Ok(Judging {
                    version: version.to_owned(),
                    newer: None,
                });
            }
            (true, [newest]) => {
                return Ok(Judging {
                    version: version.to_owned(),
                    newer: Some((*newest).to_owned()),
                });
            }
            (_, [_, .., newest]) => {
                // Every fetched release after the installed one up to `newest` counts, held or
                // not: a release whose tag sits off the candidate's ancestry (cut from a release
                // branch) is still one the installed app is behind.
                let top = release_version(newest);
                let behind = tags
                    .iter()
                    .filter_map(|tag| release_version(&tag.version))
                    .filter(|v| *v > key && Some(*v) <= top)
                    .count();
                return Err(format!(
                    "installed aterm {version} is {behind} releases behind v{newest}, the newest \
                     public tag the candidate holds at or before its workspace version \
                     {workspace}; only that release or the one before it judges bootstrap \
                     compatibility: update the installed app"
                ));
            }
            (false, _) => {}
        }
        let holds = match releases.last() {
            Some(newest) => format!(
                "the newest public tag the candidate holds at or before its workspace version \
                 {workspace} is v{newest}"
            ),
            None => format!(
                "the candidate holds no public tag at or before its workspace version {workspace}"
            ),
        };
        Err(
            match tags
                .iter()
                .find(|tag| release_version(&tag.version) == Some(key))
            {
                None => format!(
                    "installed aterm {version}'s tag v{version} is not fetched in this clone \
                     ({holds}); fetch public tags (`git fetch origin --tags`)"
                ),
                // Main does not hold every release tag (v0.40.0, for one, is beside it), so
                // merging main clears this only where main holds the tag; installing the newest
                // release the candidate holds at or before its workspace version clears it
                // wherever the candidate holds one.
                Some(tag) if !tag.held => {
                    let install = releases.last().map_or_else(String::new, |newest| {
                        format!(", or else install aterm {newest}, which the candidate holds")
                    });
                    format!(
                        "installed aterm {version}'s tag v{version} is fetched but not in the \
                         candidate's ancestry ({holds}); merge origin/main into the candidate \
                         where origin/main holds v{version} (`git merge-base --is-ancestor \
                         v{version} origin/main`){install}"
                    )
                }
                Some(_) => format!(
                    "installed aterm {version}'s tag v{version} is in the candidate's ancestry, \
                     but the candidate's workspace version {workspace} is behind it, so the \
                     candidate moved its version back; take origin/main's workspace version"
                ),
            },
        )
    }

    /// A home and a call the shim places a build's intermediates by, named for the placement
    /// this tree's shim selects there. The shim selects and the helper must admit what it
    /// selected: aterm 0.199.0's shim took its in-tree fallback in [`Self::NewHome`] and its
    /// helper refused it, and a shim that walks to the first folder that stands refuses under
    /// [`Self::NoCache`] with a helper that owes that build a record it cannot lay.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Placement {
        /// The shared cache, in a home whose `Library/Caches` may be written: an account's.
        SharedCache,
        /// A home with no `Library` at all, as a new account's or a container's: the shared
        /// cache, made through the folders that home lacks.
        NewHome,
        /// The in-tree fallback, in a home whose `Library/Caches` stands and cannot be written.
        NoCache,
        /// Under a target directory the caller chose (`--target-dir`), in an account's home.
        ChosenTarget,
    }

    impl Placement {
        /// Every placement, in the order their rows are written.
        pub(super) const ALL: [Self; 4] = [
            Self::SharedCache,
            Self::NewHome,
            Self::NoCache,
            Self::ChosenTarget,
        ];

        /// What a row calls it.
        pub(super) const fn name(self) -> &'static str {
            match self {
                Self::SharedCache => "the shared cache (a home with a writable Library/Caches)",
                Self::NewHome => "a new home (no Library folder: the shared cache is made there)",
                Self::NoCache => {
                    "the no-cache fallback (a home whose Library/Caches cannot be written)"
                }
                Self::ChosenTarget => "a caller's --target-dir",
            }
        }

        /// The home's `Library/Caches` as this placement has it: its mode, or `None` where the
        /// home has no `Library`.
        const fn caches_mode(self) -> Option<u32> {
            match self {
                Self::SharedCache | Self::ChosenTarget => Some(0o700),
                Self::NewHome => None,
                Self::NoCache => Some(0o500),
            }
        }
    }

    /// Whose shim text and helper a rig runs.
    #[derive(Clone, Copy)]
    pub(super) enum Pair<'a> {
        /// The installed public release's: its store's shim with only the store path rebased,
        /// naming the helper in the installed `aterm.app`.
        Installed,
        /// This tree's: the shim this tree's atpkg lays for the store's `targo`
        /// (`atpkg::platform::canonical_build_hop_shim`), naming `helper`.
        Candidate { helper: &'a Path },
    }

    impl Pair<'_> {
        /// The check that judges this pair, for the line that names a rig it kept.
        const fn check(self) -> &'static str {
            match self {
                Self::Installed => "released-shim",
                Self::Candidate { .. } => "candidate-shim",
            }
        }
    }

    /// Why the store gives no pair to judge with ([`Store::read`], [`Store::release`]).
    #[derive(Debug)]
    pub(super) enum Unready {
        /// This machine has none to judge with: its package prefix holds no `targo` shim, or the
        /// shim's `aterm.app` is not installed. The text says where the missing piece comes
        /// from. The lanes and the tests of a pair end COULD NOT RUN on it.
        Absent(String),
        /// The pair is there and wrong: a shim that cannot be read or names no helper, a helper
        /// outside an `aterm.app` (a loose binary, a dev bundle) or missing from its installed
        /// app, or a development or unreadable app. A test of the pair fails on it; the lane
        /// still decides nothing, since the installed pair is no fault of the candidate.
        Defect(String),
    }

    impl std::fmt::Display for Unready {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Absent(why) | Self::Defect(why) => f.write_str(why),
            }
        }
    }

    /// This machine's package store: its prefix, and the text of its `targo` shim.
    pub(super) struct Store {
        prefix: PathBuf,
        text: String,
    }

    impl Store {
        /// The store of the package prefix the caller configured
        /// (`EnvSnapshot::test_package_prefix`, chosen before a test HOME replaces the caller's).
        ///
        /// # Errors
        /// [`Unready::Absent`] where the prefix holds no `bin/targo`; [`Unready::Defect`] where
        /// it is there and cannot be read.
        pub(super) fn read(env: &EnvSnapshot) -> Result<Self, Unready> {
            let prefix = env.test_package_prefix.clone().unwrap_or_else(|| {
                crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
            });
            let shim = prefix.join("bin/targo");
            let text = match fs::read_to_string(&shim) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Unready::Absent(format!(
                        "no targo shim at {}: this package prefix holds no installed Trust \
                         toolchain, which both bootstrap pairs build with; `aterm pkg install \
                         trust`, run with the installed public aterm.app's `aterm`, writes it \
                         there",
                        shim.display()
                    )));
                }
                Err(e) => return Err(Unready::Defect(format!("{}: {e}", shim.display()))),
            };
            Ok(Self { prefix, text })
        }

        /// The first line of the installed app's `--version`, where the store's shim names a
        /// helper in the installed public release.
        ///
        /// # Errors
        /// [`Unready::Absent`] where the shim's `aterm.app` is not installed; [`Unready::Defect`]
        /// for every other refusal ([`Unready`]).
        pub(super) fn release(&self) -> Result<String, Unready> {
            let shim = self.prefix.join("bin/targo");
            let helper =
                atpkg::build_cache::registered::shim_helper(&self.text).ok_or_else(|| {
                    Unready::Defect(format!(
                        "installed shim {} names no helper; repair the installed release",
                        shim.display()
                    ))
                })?;
            // A candidate binary or dev bundle is not evidence about the installed public
            // release: the shim must name an `aterm.app`'s helper. Only that app being gone is an
            // absence of this machine.
            let app = helper
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .filter(|app| app.file_name().is_some_and(|n| n == "aterm.app"))
                .ok_or_else(|| {
                    Unready::Defect(format!(
                        "the installed shim {} hands its builds to {}, which is no aterm.app's \
                         helper (a loose binary or a dev bundle); `aterm pkg install trust`, run \
                         with the installed public aterm.app's `aterm`, rewrites the shim",
                        shim.display(),
                        helper.display()
                    ))
                })?;
            if !app.exists() {
                return Err(Unready::Absent(format!(
                    "the installed shim {} hands its builds to {}, and no {} is installed: this \
                     machine has no installed public aterm release to judge with; install it \
                     (tools/install.sh), and `aterm pkg install trust` with its `aterm` rewrites \
                     the shim",
                    shim.display(),
                    helper.display(),
                    app.display()
                )));
            }
            if !helper.is_file() {
                return Err(Unready::Defect(format!(
                    "the installed shim's helper {} is not in its app; repair the installed \
                     release",
                    helper.display()
                )));
            }
            let mut version = aterm_uds::spawnfd::command(helper.with_file_name("aterm"));
            version.arg("--version");
            // A hang detector, as for the build: ended only once the app's output and its
            // process tree's CPU and I/O stay still for a minute.
            let version = aterm_uds::deadline::output_while_process_progressing(
                &mut version,
                Duration::from_secs(60),
                "the installed app's aterm --version",
            )
            .map_err(|e| Unready::Defect(format!("{}: {e}", app.display())))?;
            let said = String::from_utf8_lossy(&version.stdout);
            let line = said.lines().next().unwrap_or_default();
            if !version.status.success() || public_version(line).is_none() {
                return Err(Unready::Defect(format!(
                    "helper {} belongs to a development or unreadable app, not a public release",
                    helper.display()
                )));
            }
            Ok(line.to_owned())
        }

        /// The shim `pair` runs, for this store rebased to `local`.
        fn shim(&self, pair: Pair<'_>, local: &Path) -> Result<String, String> {
            match pair {
                Pair::Installed => relocate(&self.text, &self.prefix, local),
                Pair::Candidate { helper } => {
                    let installed = self.prefix.join("bin/targo");
                    let target = atpkg::platform::resolve_shim(&installed)
                        .ok_or("the store's targo shim names no program")?;
                    let target =
                        local.join(target.strip_prefix(&self.prefix).map_err(
                            |_| "the store's targo shim runs a program outside the store",
                        )?);
                    // The exports are the toolchain package's, not an aterm version's.
                    let exports = atpkg::platform::shim_env_of(&installed);
                    let laid = io(atpkg::platform::canonical_build_hop_shim(
                        &local.join("bin/targo"),
                        &target,
                        &exports,
                        helper,
                    ))?;
                    String::from_utf8(laid.body).map_err(|e| e.to_string())
                }
            }
        }
    }

    pub(super) struct Rig {
        _scratch: aterm_tempfile::TempDir,
        pub(super) home: PathBuf,
        pub(super) tree: PathBuf,
        shim: PathBuf,
        cargo: PathBuf,
        rustup: PathBuf,
        placement: Placement,
        /// The check whose pair this rig runs ([`Pair::check`]).
        check: &'static str,
        /// Who judges, for a row: the installed app's `--version` line, or this tree's pair.
        pub(super) label: String,
    }

    impl Rig {
        pub(super) fn new(
            env: &EnvSnapshot,
            store: &Store,
            pair: Pair<'_>,
            label: String,
            placement: Placement,
        ) -> Result<Self, String> {
            // The home is the build's TMPDIR too, and a toolchain that runs a job server binds
            // its socket in a folder it makes there (Trust build 20882's `tjs-<16 hex>/s`). A
            // socket's path holds 104 bytes on macOS and the account's temporary root takes
            // most of them, so that toolchain would bind under `/tmp`, outside this rig's
            // custody, where the sandbox refuses it. Under the short `/tmp` the socket fits
            // in the home.
            let scratch = io(aterm_tempfile::Builder::new()
                .prefix("rs-")
                .suffix(".noindex")
                .tempdir_in("/tmp"))?;
            let home = io(scratch.path().canonicalize())?;
            // Model the account cache parent the placement names, without any managed cache
            // or marker: the shim places a build by the first of the cache's folders that
            // stands and whether it may be written, and the helper makes the rest. A judgment
            // is one build, the first in its home, so it is of the placement named, and its
            // custody stays entirely in scratch.
            if let Some(mode) = placement.caches_mode() {
                let caches = home.join("Library/Caches");
                io(fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&caches))?;
                io(fs::set_permissions(
                    &caches,
                    fs::Permissions::from_mode(mode),
                ))?;
            }
            let prefix = &store.prefix;
            let local = home.join("pkg");
            let trust = local.join("store/trust");
            io(fs::create_dir_all(&trust))?;
            // Symlink the immutable trees, copy their sibling records: identity stays true and
            // the helper can take leases/mark locked builds without touching the store.
            for entry in io(fs::read_dir(prefix.join("store/trust")))? {
                let entry = io(entry)?;
                let name = entry.file_name();
                let source = entry.path();
                let target = trust.join(&name);
                if source.is_dir() {
                    io(std::os::unix::fs::symlink(
                        io(source.canonicalize())?,
                        target,
                    ))?;
                } else if source.is_file() {
                    copy_file(&source, &target)?;
                }
            }
            let shim = home.join("targo");
            io(fs::write(&shim, store.shim(pair, &local)?))?;
            let cargo = home.join(".cargo");
            io(fs::create_dir_all(&cargo))?;
            let installed_home = env.tool_home.as_deref().unwrap_or(&env.home);
            let real_cargo = env
                .cargo_home
                .clone()
                .unwrap_or_else(|| installed_home.join(".cargo"));
            for name in ["registry", "git"] {
                let real = real_cargo.join(name);
                if real.is_dir() {
                    io(std::os::unix::fs::symlink(real, cargo.join(name)))?;
                }
            }
            let rustup = home.join(".rustup");
            io(fs::create_dir_all(rustup.join("toolchains")))?;
            let real_rustup = env
                .rustup_home
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| installed_home.join(".rustup"));
            // Trust's installed shim uses its managed SDK directly; rustup is
            // optional. Preserve any actual installed toolchains without
            // requiring an account to have run rustup's installer first.
            let toolchains = match fs::read_dir(real_rustup.join("toolchains")) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            for entry in toolchains.into_iter().flatten() {
                let entry = io(entry)?;
                io(std::os::unix::fs::symlink(
                    io(entry.path().canonicalize())?,
                    rustup.join("toolchains").join(entry.file_name()),
                ))?;
            }
            let tree = home.join("candidate");
            io(fs::create_dir_all(&tree))?;
            Ok(Self {
                _scratch: scratch,
                home,
                tree,
                shim,
                cargo,
                rustup,
                placement,
                check: pair.check(),
                label,
            })
        }

        pub(super) fn sandbox(&self) -> Result<aterm_uds::spawnfd::Command, String> {
            let ledger = aterm_capacity::build_cpu::capacity_dir().map_err(|e| e.to_string())?;
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
            cmd.env_clear()
                .env("HOME", &self.home).env("PWD", &self.tree)
                .env("CARGO_HOME", &self.cargo).env("RUSTUP_HOME", &self.rustup)
                .env("TMPDIR", &self.home).env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .current_dir(&self.tree)
                // No network, and no write outside the home and the capacity ledger. A socket
                // whose path is in the home is the build's own (the toolchain's job server):
                // it reaches nothing outside this rig.
                .args(["-p", "(version 1)(allow default)(deny network*)(allow network-bind network-inbound network-outbound (subpath (param \"SCRATCH\")))(deny file-write*)(allow file-write* (subpath (param \"SCRATCH\")) (subpath (param \"LEDGER\")) (literal \"/dev/null\"))", "-D"])
                .arg(format!("SCRATCH={}", self.home.display()))
                .arg("-D").arg(format!("LEDGER={}", ledger.display()));
            Ok(cmd)
        }

        fn run_build(
            &self,
            command: &aterm_uds::spawnfd::Command,
            env: exec::ExecEnv<'_>,
        ) -> exec::MachineRun {
            let argv: Vec<_> = std::iter::once(command.get_program().to_owned())
                .chain(command.get_args().map(std::ffi::OsStr::to_owned))
                .collect();
            let envs = command
                .get_envs()
                .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
                .collect();
            let mut command = exec::Cmd::replayed(&argv, &self.tree, envs);
            command.hang_after = Some(Duration::from_secs(60));
            // The installed helper marks genuine admission waits. Keep both those marks and
            // captured diagnostics inside the sandbox's existing writable scratch root, and
            // suspend the inactivity detector during those waits. A progressing build has
            // no elapsed-work limit; one minute without output or CPU/I/O proves a hang.
            let env = exec::ExecEnv {
                scratch: &self.home,
                child_ceiling: None,
                ..env
            };
            exec::run_machine(&command, env, Some(self.check))
        }

        pub(super) fn build(
            &self,
            execution: exec::ExecEnv<'_>,
        ) -> Result<exec::MachineRun, String> {
            let mut cmd = self.sandbox()?;
            cmd.arg("/bin/sh").arg(&self.shim).args([
                "--unverified",
                "build",
                "--offline",
                "--locked",
                "-p",
                "aterm-time",
                "--lib",
                "--message-format=json",
            ]);
            if self.placement == Placement::ChosenTarget {
                cmd.arg("--target-dir").arg(self.home.join("chosen-target"));
            }
            Ok(self.run_build(&cmd, execution))
        }

        /// The candidate's tracked files, snapshotted into the rig, and the rig's one build of
        /// them. A build that exits 0 with no library under scratch decided nothing.
        fn first_build(
            self,
            root: &Path,
            env: &EnvSnapshot,
            execution: exec::ExecEnv<'_>,
        ) -> Result<exec::MachineRun, String> {
            let result = (|| {
                for path in git_files(root, env)? {
                    let from = root.join(&path);
                    // Deleted tracked inputs remain deleted in the candidate snapshot.
                    if fs::symlink_metadata(&from).is_ok() {
                        copy_file(&from, &self.tree.join(path))?;
                    }
                }
                let output = self.build(execution)?;
                if output.run.ok
                    && !built_artifact(output.stdout.as_deref()?.as_bytes(), &self.home)
                {
                    return Err(format!(
                        "{} exited successfully without an aterm-time library artifact",
                        self.label
                    ));
                }
                Ok(output)
            })();
            let check = self.check;
            match result {
                Err(why) => {
                    let retained = self._scratch.keep();
                    Err(format!(
                        "{why}; {check} failure inputs retained at {}",
                        retained.display()
                    ))
                }
                Ok(mut output) => {
                    if !output.run.ok {
                        let retained = self._scratch.keep();
                        output.run.output.push_str(&format!(
                            "\n{check} failure inputs retained at {}\n",
                            retained.display()
                        ));
                    }
                    Ok(output)
                }
            }
        }
    }

    pub(super) fn judge(
        root: &Path,
        env: &EnvSnapshot,
        execution: exec::ExecEnv<'_>,
    ) -> Result<(Judging, exec::MachineRun), String> {
        let manifest = io(fs::read_to_string(root.join("Cargo.toml")))?;
        let manifest: aterm_toml::Value =
            aterm_toml::from_str(&manifest).map_err(|e| e.to_string())?;
        let current = manifest
            .get("workspace")
            .and_then(|v| v.get("package"))
            .and_then(|v| v.get("version"))
            .and_then(aterm_toml::Value::as_str)
            .ok_or("workspace has no version")?;
        let head = crate::head_commit(root, &env.path).ok_or("candidate has no HEAD")?;
        let tags = public_tags(root, env, &head)?;
        let store = Store::read(env).map_err(|unready| unready.to_string())?;
        let installed = store.release().map_err(|unready| unready.to_string())?;
        let judging = judging_release(&installed, current, &tags)?;
        let rig = Rig::new(
            env,
            &store,
            Pair::Installed,
            installed,
            Placement::SharedCache,
        )?;
        let output = rig.first_build(root, env, execution)?;
        Ok((judging, output))
    }

    /// What a row calls this tree's pair.
    const CANDIDATE: &str = "this tree's shim and atpkg";

    /// This tree's pair under `placement`: a fresh home, one build.
    pub(super) fn judge_candidate(
        root: &Path,
        env: &EnvSnapshot,
        helper: &Path,
        placement: Placement,
        execution: exec::ExecEnv<'_>,
    ) -> Result<exec::MachineRun, String> {
        let store = Store::read(env).map_err(|unready| unready.to_string())?;
        let rig = Rig::new(
            env,
            &store,
            Pair::Candidate { helper },
            CANDIDATE.to_owned(),
            placement,
        )?;
        rig.first_build(root, env, execution)
    }

    /// `candidate-shim`'s rows: [`judge_candidate`] under each of [`Placement::ALL`].
    pub(super) fn candidate(
        root: &Path,
        env: &EnvSnapshot,
        helper: &Path,
        execution: exec::ExecEnv<'_>,
        report: &mut crate::ladder::Report,
    ) {
        for placement in Placement::ALL {
            let under = placement.name();
            match judge_candidate(root, env, helper, placement, execution) {
                Ok(output) if output.run.ok => {
                    report.raw(output.run.output);
                    report.pass(format!(
                        "candidate-shim: {CANDIDATE} built aterm-time under {under}"
                    ));
                }
                Ok(output) => {
                    report.raw(output.stdout.as_deref().unwrap_or("").to_owned());
                    report.raw(output.run.output.clone());
                    // A build ended with no exit code (a hang, a signal) decided nothing.
                    let Some(code) = output.run.code else {
                        report.cannot_run(format!(
                            "candidate-shim: {CANDIDATE} did not complete a first build under \
                             {under}; see child output"
                        ));
                        continue;
                    };
                    report.fail(format!(
                        "candidate-shim: {CANDIDATE} refuse a first build under {under} (exit \
                         status: {code}); {}; a release cut from this tree would refuse that \
                         build on every Mac it is installed on: fix the shim or the helper \
                         before it is released",
                        super::refusal(&output)
                    ));
                }
                Err(why) => report.cannot_run(format!("candidate-shim: under {under}: {why}")),
            }
        }
    }

    fn built_artifact(output: &[u8], home: &Path) -> bool {
        String::from_utf8_lossy(output).lines().any(|line| {
            let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
                return false;
            };
            row.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
                && row
                    .get("target")
                    .and_then(|t| t.get("name"))
                    .and_then(aterm_json::Value::as_str)
                    == Some("aterm_time")
                && row
                    .get("filenames")
                    .and_then(aterm_json::Value::as_array)
                    .is_some_and(|files| {
                        files
                            .iter()
                            .filter_map(aterm_json::Value::as_str)
                            .any(|file| {
                                let path = Path::new(file);
                                path.extension().is_some_and(|ext| ext == "rlib")
                                    && path.starts_with(home)
                                    && path.is_file()
                            })
                    })
        })
    }

    #[cfg(test)]
    mod tests;
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bootstrap compatibility is decided by the installed pair, never candidate atpkg code.
//! Relocation changes only the shim's baked store path. The helper stays the installed image.
//! All leases, ownership records, Cargo state and artifacts go to scratch; SBPL enforces that
//! the real toolchain, registry and build caches cannot be written, even by build scripts.
//! The installed helper retains its normal account-wide capacity-ledger bookkeeping.

use crate::{Ctx, Scope, ladder::Report};

fn bootstrap_input(path: &str) -> bool {
    path.split('/').any(|part| part == ".cargo")
        || matches!(
            path.rsplit('/').next().unwrap_or(path),
            "Cargo.toml" | "Cargo.lock" | "trust.lock" | "rust-toolchain" | "rust-toolchain.toml"
        )
        || [
            "crates/atpkg/",
            "crates/aterm-toolchain/",
            "crates/aterm-tempfile/",
            "crates/aterm-release/",
            "crates/aterm-verify/src/released_shim",
            "tools/install.sh",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// The line of a refused build that says why.
#[cfg(target_os = "macos")]
fn refusal(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find(|line| line.contains("refused") || line.starts_with("error:"))
        .unwrap_or("see child output")
        .to_owned()
}

pub(crate) fn run(ctx: &Ctx, report: &mut Report) {
    let inputs = match &ctx.scope {
        Scope::Changed(change) => {
            match crate::changed::changed_since(&ctx.root, &ctx.path_env, &change.base) {
                Ok(paths) => paths.into_iter().filter(|p| bootstrap_input(p)).collect(),
                Err(why) => {
                    report.cannot_run(why);
                    return;
                }
            }
        }
        _ => vec!["explicit selection (all bootstrap inputs)".to_owned()],
    };
    if inputs.is_empty() {
        report.pass("released-shim: no bootstrap inputs changed; no child build");
        return;
    }
    #[cfg(target_os = "macos")]
    match mac::judge(&ctx.root, &ctx.env) {
        Ok((judging, output)) if output.status.success() => {
            report.raw(String::from_utf8_lossy(&output.stderr));
            report.pass(format!(
                "released-shim: {} built aterm-time; inputs: {}",
                judging.who(),
                inputs.join(", ")
            ));
        }
        Ok((judging, output)) => {
            report.raw(String::from_utf8_lossy(&output.stdout));
            report.raw(String::from_utf8_lossy(&output.stderr));
            report.fail(format!("released-shim: {} refuses candidate ({}); incompatible bootstrap inputs: {}; {}; land shim support first, release it, then adopt the configuration", judging.who(), output.status, inputs.join(", "), refusal(&output)));
        }
        Err(why) => report.cannot_run(format!("released-shim: {why}")),
    }
    #[cfg(not(target_os = "macos"))]
    report.skip("released-shim: macOS installed-app bootstrap lane (run on a Mac)");
}

#[cfg(target_os = "macos")]
mod mac {
    use crate::EnvSnapshot;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    fn io<T, E: std::fmt::Display>(value: Result<T, E>) -> Result<T, String> {
        value.map_err(|e| e.to_string())
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
        /// The newest fetched public tag's version, where the installed app is the release
        /// immediately before it.
        pub(super) newer: Option<String>,
    }

    impl Judging {
        /// Which release judged, and that a newer one exists where it does.
        pub(super) fn who(&self) -> String {
            match &self.newer {
                None => format!("aterm {}", self.version),
                Some(newer) => format!(
                    "aterm {} (the release before v{newer}, the newest fetched public tag; \
                     update the installed app to judge with v{newer})",
                    self.version
                ),
            }
        }
    }

    /// WHICH INSTALLED APP MAY JUDGE: a public release that is the newest fetched public tag in
    /// the candidate's ancestry, or the one immediately before it. `installed` is the first line
    /// of the app's `--version`, `releases` those tags' versions, oldest first
    /// ([`crate::harness_debt::public_releases`]). In the hours after a release is published a
    /// Mac that has fetched its tag and not yet updated still judges, and its row says with
    /// which release; a development build, a release two or more behind and one no fetched tag
    /// names decide nothing.
    ///
    /// # Errors
    /// As above, each with its remedy.
    pub(super) fn judging_release(installed: &str, releases: &[String]) -> Result<Judging, String> {
        let Some(version) = public_version(installed) else {
            return Err(format!(
                "installed {installed:?} is a development or unreadable build, not a public \
                 release; install the newest public release before judging bootstrap compatibility"
            ));
        };
        let Some((newest, older)) = releases.split_last() else {
            return Err(
                "no fetched public vMAJOR.MINOR.0 tag at or before the workspace version is in \
                 the candidate; fetch public tags (`git fetch origin --tags`)"
                    .into(),
            );
        };
        if version == newest {
            return Ok(Judging {
                version: version.to_owned(),
                newer: None,
            });
        }
        if older.last().is_some_and(|previous| previous == version) {
            return Ok(Judging {
                version: version.to_owned(),
                newer: Some(newest.clone()),
            });
        }
        Err(match older.iter().rev().position(|tag| tag == version) {
            Some(behind) => format!(
                "installed aterm {version} is {} releases behind the newest fetched public tag \
                 v{newest}; only that release or the one before it judges bootstrap \
                 compatibility: update the installed app",
                behind + 1
            ),
            None => format!(
                "installed aterm {version} is no fetched public tag (the newest fetched is \
                 v{newest}); fetch public tags (`git fetch origin --tags`), then ensure the \
                 installed app is the newest public release or the one before it before judging \
                 bootstrap compatibility"
            ),
        })
    }

    /// This machine's package store: its prefix, and the text of its `targo` shim.
    pub(super) struct Store {
        prefix: PathBuf,
        text: String,
    }

    impl Store {
        pub(super) fn read(env: &EnvSnapshot) -> Result<Self, String> {
            let prefix = env.test_package_prefix.clone().unwrap_or_else(|| {
                crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
            });
            let text = io(fs::read_to_string(prefix.join("bin/targo")))?;
            Ok(Self { prefix, text })
        }

        /// The first line of the installed app's `--version`, where the store's shim names a
        /// helper in the installed public release.
        pub(super) fn release(&self) -> Result<String, String> {
            let helper = atpkg::build_cache::registered::shim_helper(&self.text)
                .ok_or("installed shim names no helper; repair the installed release")?;
            // A candidate binary or dev bundle is not evidence about the installed public release.
            let app = helper
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .ok_or("helper is outside an app")?;
            if app.file_name().is_none_or(|n| n != "aterm.app") {
                return Err(format!(
                    "helper {} is not the installed aterm.app",
                    helper.display()
                ));
            }
            let mut version = aterm_uds::spawnfd::command(helper.with_file_name("aterm"));
            version.arg("--version");
            // A hang detector, as for the build: ended only once the app's output and its
            // process tree's CPU and I/O stay still for a minute.
            let version = io(aterm_uds::deadline::output_while_process_progressing(
                &mut version,
                Duration::from_secs(60),
                "the installed app's aterm --version",
            ))?;
            let said = String::from_utf8_lossy(&version.stdout);
            let line = said.lines().next().unwrap_or_default();
            if !version.status.success() || public_version(line).is_none() {
                return Err(
                    "helper belongs to a development or unreadable app, not a public release"
                        .into(),
                );
            }
            Ok(line.to_owned())
        }
    }

    pub(super) struct Rig {
        _scratch: aterm_tempfile::TempDir,
        pub(super) home: PathBuf,
        pub(super) tree: PathBuf,
        shim: PathBuf,
        cargo: PathBuf,
        rustup: PathBuf,
        /// Who judges, for a row: the installed app's `--version` line.
        pub(super) label: String,
    }

    impl Rig {
        pub(super) fn new(env: &EnvSnapshot, store: &Store, label: String) -> Result<Self, String> {
            let scratch = io(aterm_tempfile::tempdir())?;
            let home = io(scratch.path().canonicalize())?;
            // Model the standard account cache parent without any managed cache or marker.
            // This lane judges the installed shim's configuration support, independently of
            // whether an older helper can bootstrap missing account directories. The judgment
            // is one build, the first in its home, and its custody stays entirely in scratch.
            io(fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(home.join("Library/Caches")))?;
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
            io(fs::write(
                &shim,
                relocate(&store.text, &store.prefix, &local)?,
            ))?;
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
                .args(["-p", "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath (param \"SCRATCH\")) (subpath (param \"LEDGER\")) (literal \"/dev/null\"))", "-D"])
                .arg(format!("SCRATCH={}", self.home.display()))
                .arg("-D").arg(format!("LEDGER={}", ledger.display()));
            Ok(cmd)
        }

        pub(super) fn build(&self) -> Result<std::process::Output, String> {
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
            // A hang detector, not a latency budget: the build may take as long as a loaded
            // Mac makes it, and is ended only once its output and its descendants' CPU and
            // I/O stay still for a minute.
            io(aterm_uds::deadline::output_while_process_progressing(
                &mut cmd,
                Duration::from_secs(60),
                "the installed shim's offline aterm-time build",
            ))
        }

        /// The candidate's tracked files, snapshotted into the rig, and the rig's one build of
        /// them. A build that exits 0 with no library under scratch decided nothing.
        fn first_build(
            self,
            root: &Path,
            env: &EnvSnapshot,
        ) -> Result<std::process::Output, String> {
            let result = (|| {
                for path in git_files(root, env)? {
                    let from = root.join(&path);
                    // Deleted tracked inputs remain deleted in the candidate snapshot.
                    if fs::symlink_metadata(&from).is_ok() {
                        copy_file(&from, &self.tree.join(path))?;
                    }
                }
                let output = self.build()?;
                if output.status.success() && !built_artifact(&output.stdout, &self.home) {
                    return Err(format!(
                        "{} exited successfully without an aterm-time library artifact",
                        self.label
                    ));
                }
                Ok(output)
            })();
            match result {
                Err(why) => {
                    let retained = self._scratch.keep();
                    Err(format!(
                        "{why}; released-shim failure inputs retained at {}",
                        retained.display()
                    ))
                }
                Ok(mut output) => {
                    if !output.status.success() {
                        let retained = self._scratch.keep();
                        output.stderr.extend_from_slice(
                            format!(
                                "\nreleased-shim failure inputs retained at {}\n",
                                retained.display()
                            )
                            .as_bytes(),
                        );
                    }
                    Ok(output)
                }
            }
        }
    }

    pub(super) fn judge(
        root: &Path,
        env: &EnvSnapshot,
    ) -> Result<(Judging, std::process::Output), String> {
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
        let releases: Vec<String> =
            crate::harness_debt::public_releases(root, &env.path, &head, current)?
                .into_iter()
                .map(|(version, _)| version)
                .collect();
        let store = Store::read(env)?;
        let installed = store.release()?;
        let judging = judging_release(&installed, &releases)?;
        let rig = Rig::new(env, &store, installed)?;
        let output = rig.first_build(root, env)?;
        Ok((judging, output))
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

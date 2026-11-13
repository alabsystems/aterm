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
        Ok((version, output)) if output.status.success() => {
            report.raw(String::from_utf8_lossy(&output.stderr));
            report.pass(format!(
                "released-shim: {version} built aterm-time; inputs: {}",
                inputs.join(", ")
            ));
        }
        Ok((version, output)) => {
            report.raw(String::from_utf8_lossy(&output.stdout));
            report.raw(String::from_utf8_lossy(&output.stderr));
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .find(|line| line.contains("refused") || line.starts_with("error:"))
                .unwrap_or("see child output");
            report.fail(format!("released-shim: {version} refuses candidate ({}); incompatible bootstrap inputs: {}; {reason}; land shim support first, release it, then adopt the configuration", output.status, inputs.join(", ")));
        }
        Err(why) => report.cannot_run(format!("released-shim: {why}")),
    }
    #[cfg(not(target_os = "macos"))]
    report.skip("released-shim: macOS installed-app bootstrap lane (run on a Mac)");
}

#[cfg(target_os = "macos")]
mod mac {
    use crate::EnvSnapshot;
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
        let out = io(
            aterm_uds::deadline::output_until_complete_stdout_with_limit(
                &mut cmd,
                std::time::Instant::now() + Duration::from_secs(60),
                16 << 20,
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

    struct Rig {
        _scratch: aterm_tempfile::TempDir,
        home: PathBuf,
        tree: PathBuf,
        shim: PathBuf,
        cargo: PathBuf,
        rustup: PathBuf,
        version: String,
    }

    impl Rig {
        fn new(env: &EnvSnapshot) -> Result<Self, String> {
            let scratch = io(aterm_tempfile::tempdir())?;
            let home = io(scratch.path().canonicalize())?;
            let prefix = env.test_package_prefix.clone().unwrap_or_else(|| {
                crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
            });
            let text = io(fs::read_to_string(prefix.join("bin/targo")))?;
            let helper = atpkg::build_cache::registered::shim_helper(&text)
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
            let version = io(aterm_uds::deadline::output_within(
                &mut version,
                Duration::from_secs(60),
            ))?;
            let said = String::from_utf8_lossy(&version.stdout);
            if !version.status.success()
                || !said.starts_with("aterm ")
                || said.lines().next().is_some_and(|s| s.contains('+'))
            {
                return Err(
                    "helper belongs to a development or unreadable app, not a public release"
                        .into(),
                );
            }
            let local = home.join("pkg");
            let store = local.join("store/trust");
            io(fs::create_dir_all(&store))?;
            // Symlink the immutable trees, copy their sibling records: identity stays true and
            // the release helper can take leases/mark locked builds without touching the store.
            for entry in io(fs::read_dir(prefix.join("store/trust")))? {
                let entry = io(entry)?;
                let name = entry.file_name();
                let source = entry.path();
                let target = store.join(&name);
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
            io(fs::write(&shim, relocate(&text, &prefix, &local)?))?;
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
            for entry in io(fs::read_dir(real_rustup.join("toolchains")))? {
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
                version: said.lines().next().unwrap_or_default().to_owned(),
            })
        }

        fn sandbox(&self) -> Result<aterm_uds::spawnfd::Command, String> {
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

        fn build(&self) -> Result<std::process::Output, String> {
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
    }

    pub(super) fn judge(
        root: &Path,
        env: &EnvSnapshot,
    ) -> Result<(String, std::process::Output), String> {
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
        let (released, _) = crate::harness_debt::public_baseline(root, &env.path, &head, current)?;
        let rig = Rig::new(env)?;
        if rig.version != format!("aterm {released}") {
            return Err(format!(
                "installed {} differs from newest fetched public tag v{released}; fetch public tags (`git fetch origin --tags`), then ensure the installed app matches the public baseline before judging bootstrap compatibility",
                rig.version
            ));
        }
        for path in git_files(root, env)? {
            let from = root.join(&path);
            // Deleted tracked inputs remain deleted in the candidate snapshot.
            if fs::symlink_metadata(&from).is_ok() {
                copy_file(&from, &rig.tree.join(path))?;
            }
        }
        let output = rig.build()?;
        if output.status.success() && !built_artifact(&output.stdout, &rig.home) {
            return Err(format!(
                "{} exited successfully without an aterm-time library artifact",
                rig.version
            ));
        }
        Ok((rig.version.clone(), output))
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

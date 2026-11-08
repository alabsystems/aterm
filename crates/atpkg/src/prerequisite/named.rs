// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named, signed prerequisite builds retained beside the selected default.

use super::*;

/// One exact named build and its operation-owned command/import namespace.
#[derive(Debug)]
pub struct HeldBuild {
    tool: HeldTool,
    namespace: aterm_tempfile::TempDir,
    elan_home: Option<PathBuf>,
}

impl HeldBuild {
    /// Run a cooperative controller until its original native group drains.
    /// Unknown custody retains this borrowed namespace and its lease.
    ///
    /// # Errors
    /// Admission, definite spawn failure, settled wait error, or changed held bytes.
    pub fn run(
        &self,
        program: &str,
        args: &[String],
        say: &mut dyn FnMut(&str),
    ) -> Result<crate::build_admit::ControllerOutcome, String> {
        let path = self.child_path(&std::env::var_os("PATH").unwrap_or_default())?;
        #[cfg(unix)]
        let mut command = aterm_uds::spawnfd::transparent(program).into_command();
        #[cfg(not(unix))]
        let mut command = aterm_uds::spawnfd::transparent(program);
        for (name, value) in self.tool.environment().entries() {
            if name == "LEAN_CC" && !crate::toolchain::is_executable_file(Path::new(value)) {
                return Err("the named Lean runtime requires an available host C compiler".into());
            }
        }
        command.args(args).env("PATH", path).envs(
            self.tool
                .environment()
                .entries()
                .iter()
                .map(|(key, value)| (key, value)),
        );
        if let Some(home) = self.elan_home() {
            command.env("ELAN_HOME", home);
        }
        let outcome = crate::build_admit::supervise_controller(&mut command, say);
        self.child_path(&std::env::var_os("PATH").unwrap_or_default())?;
        let modes =
            crate::store::declared_modes(self.tool.root()).map_err(|error| error.to_string())?;
        if crate::tree::tree_root_declared(self.tool.root(), &modes)
            .map_err(|error| error.to_string())?
            != self.tool.tree_root()
        {
            return Err("the named prerequisite tree changed during the controller".into());
        }
        outcome.map_err(|error| error.to_string())
    }

    pub(super) fn new(want: Prerequisite, tool: HeldTool) -> Result<Self, String> {
        let namespace = aterm_tempfile::Builder::new()
            .prefix("atpkg-named-prerequisite-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let bin = namespace.path().join("bin");
        std::fs::create_dir(&bin).map_err(|error| error.to_string())?;
        crate::platform::install_shim_to_env(
            &bin.join(want.executable()),
            tool.path(),
            tool.environment(),
        )
        .map_err(|error| error.to_string())?;
        let elan_home = if want == Prerequisite::Lake {
            let home = namespace.path().join("elan");
            let versions = home.join("toolchains");
            std::fs::create_dir_all(&versions).map_err(|error| error.to_string())?;
            crate::platform::atomic_symlink(
                tool.root(),
                &versions.join(format!("leanprover--lean4---v{}", tool.version())),
            )
            .map_err(|error| error.to_string())?;
            Some(home)
        } else {
            None
        };
        Ok(Self {
            tool,
            namespace,
            elan_home,
        })
    }

    /// The signed package, version, physical root and live lease.
    #[must_use]
    pub const fn tool(&self) -> &HeldTool {
        &self.tool
    }

    /// Exact Lean import-world namespace, without installing or invoking Elan.
    #[must_use]
    pub fn elan_home(&self) -> Option<&Path> {
        self.elan_home.as_deref()
    }

    /// Validate the held bytes and create this operation's executable search path.
    ///
    /// # Errors
    /// The lease, executable or import-world link changed, or PATH is unrepresentable.
    pub fn child_path(&self, remaining: &std::ffi::OsStr) -> Result<std::ffi::OsString, String> {
        self.tool.validate()?;
        if let Some(home) = &self.elan_home {
            let link = home
                .join("toolchains")
                .join(format!("leanprover--lean4---v{}", self.tool.version()));
            if link.canonicalize().ok().as_deref() != Some(self.tool.root()) {
                return Err("the leased Lean import-world link changed".into());
            }
        }
        std::env::join_paths(
            std::iter::once(self.namespace.path().join("bin"))
                .chain(std::env::split_paths(remaining)),
        )
        .map_err(|error| error.to_string())
    }
}

/// Authenticate an already-staged named build without changing current or global shims.
///
/// # Errors
/// Configuration, signed index/package, recipe, installed tree or lease admission fails.
pub fn configured_build(want: Prerequisite, build: u64, who: &str) -> Result<HeldBuild, String> {
    let config = crate::config::cached();
    if config.unreadable {
        return Err("the package configuration is unreadable".into());
    }
    let layout =
        crate::store::resolve_from(config).ok_or("the managed package prefix is unavailable")?;
    let fetcher = crate::source::resolve_fetcher(&layout, None);
    let session = Session::resolve(
        fetcher.as_ref(),
        &layout,
        &crate::source::effective_anchor(&layout),
        crate::source::build_floor(&layout),
        crate::flow::now_unix(),
        crate::cli::current_triple(),
    )
    .map_err(|error| error.to_string())?;
    let session = session.with_description(who);
    let tool = session.acquire_build(want, build)?;
    let held = HeldBuild::new(want, tool)?;
    crate::source::advance_floors(
        &layout,
        session.index.index_build,
        session.index.roster_seq(),
    );
    Ok(held)
}

impl Session<'_> {
    /// Admit a named installed build under this operation's verified index.
    ///
    /// # Errors
    /// The package/version is unsupported, the signed floor excludes the build,
    /// or its installed bytes and lease cannot be bound to the signed artifact.
    pub fn acquire_build(&self, want: Prerequisite, build: u64) -> Result<HeldTool, String> {
        if build == 0 || !want.supports(self.triple) {
            return Err("invalid or unsupported named prerequisite build".into());
        }
        let prepared = self.read_package(want, build, true)?;
        let program = want.program();
        let artifact = prepared
            .package
            .artifact_for(self.triple)
            .ok_or("the named prerequisite has no native artifact")?;
        let lease = crate::lease::take(
            &self.layout.prefix,
            &Subject::build(program, build).ok_or("invalid prerequisite identity")?,
            self.description,
            crate::lease::DEFAULT_WAIT,
        )
        .map_err(|error| error.to_string())?;
        let placed = self.layout.build_dir(program, build);
        if !crate::store::build_is_complete(&placed) {
            return Err(format!(
                "{program} {build} is not staged; install its named build first"
            ));
        }
        let root = crate::lookup::payload_root(self.layout, program, build)
            .ok_or("the named prerequisite has unowned path indirection")?;
        let identity = crate::trust_lock::tree_identity(&root)
            .ok_or("the named prerequisite has no stable tree identity")?;
        let command = crate::store::ToolName::new(want.executable())
            .ok_or("the named prerequisite has an invalid exposed command")?;
        let physical = root.join("bin").join(command.exe_file());
        if physical.canonicalize().ok().as_ref() != Some(&physical) {
            return Err("the named prerequisite executable is redirected".into());
        }
        let (image, metadata) =
            aterm_uds::regular_file::open(&physical).map_err(|error| error.to_string())?;
        let modes = crate::store::declared_modes(&placed).map_err(|error| error.to_string())?;
        let actual =
            crate::tree::tree_root_declared(&root, &modes).map_err(|error| error.to_string())?;
        if actual != artifact.tree_root
            || placed.canonicalize().ok().as_ref() != Some(&root)
            || crate::trust_lock::tree_identity(&root).as_deref() != Some(identity.as_str())
        {
            return Err("named prerequisite bytes differ from the signed tree".into());
        }
        let tool = HeldTool {
            executable: physical.clone(),
            physical,
            image,
            image_identity: ImageIdentity::of(&metadata),
            environment: crate::shim_env::ShimEnv::admit(&prepared.package.shim_env)?,
            lease,
            program,
            build,
            tree_root: actual,
            root,
            version: prepared.package.version,
        };
        tool.validate()?;
        Ok(tool)
    }
}

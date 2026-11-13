// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pinned `targo` command builder for performance lanes.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::exec::check_command;
use aterm_uds::spawnfd::Command;

/// The pinned toolchain's `targo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoDriver {
    pub(crate) program: PathBuf,
}

const LANE_VERBS: &[&str] = &["build", "check", "test", "run", "bench", "doc", "rustc"];

/// Keep an outer `targo run`'s package metadata out of nested build fingerprints.
fn clear_run_env(command: &mut Command) {
    let explicit: Vec<_> = command.get_envs().map(|(key, _)| key.to_owned()).collect();
    for name in std::env::vars_os().map(|(key, _)| key).chain(explicit) {
        if name
            .to_str()
            .is_some_and(aterm_toolchain::build_cache::said_by_cargo_run)
        {
            command.env_remove(name);
        }
    }
}

impl CargoDriver {
    /// The pinned Cargo process with launcher metadata removed, ready for its arguments.
    pub(super) fn base_command(&self) -> Command {
        let mut command = check_command(&self.program);
        clear_run_env(&mut command);
        command
    }

    /// `--unverified` for a compiling verb, nothing otherwise.
    pub(crate) fn lane_args(verb: &str) -> &'static [&'static str] {
        if LANE_VERBS.contains(&verb) {
            &["--unverified"]
        } else {
            &[]
        }
    }

    /// `<targo> [--unverified] <verb>`, ready for the caller's own arguments.
    pub(crate) fn command(&self, verb: &str) -> Command {
        let mut cmd = self.base_command();
        cmd.args(Self::lead(verb, None));
        cmd
    }

    /// [`Self::command`] configured with [`workspace_build_dir`] for compiling verbs.
    pub(crate) fn workspace_command(&self, verb: &str) -> Result<Command, String> {
        prepare_workspace_build_dir(verb, workspace_build_dir())?;
        let mut cmd = self.base_command();
        cmd.args(Self::lead(verb, workspace_build_dir()));
        apply_workspace_layout(&mut cmd)?;
        Ok(cmd)
    }

    fn lead(verb: &str, build_dir: Option<&str>) -> Vec<String> {
        let mut lead = Vec::new();
        if let Some(value) = build_dir.filter(|_| LANE_VERBS.contains(&verb)) {
            lead.extend(["--config".to_string(), value.to_string()]);
        }
        lead.extend(Self::lane_args(verb).iter().map(|s| (*s).to_string()));
        lead.push(verb.to_string());
        lead
    }

    /// The command line [`Self::workspace_command`] builds, for the `$ …` log line.
    pub(crate) fn display_workspace(&self, verb: &str, args: &[&str]) -> String {
        self.display_led(verb, workspace_build_dir(), args)
    }

    fn display_led(&self, verb: &str, build_dir: Option<&str>, args: &[&str]) -> String {
        let mut parts: Vec<String> = vec![self.program.display().to_string()];
        parts.extend(Self::lead(verb, build_dir));
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }
}

/// Resolves the `--config` build-dir argument for performance builds in this workspace.
pub(crate) fn workspace_build_dir() -> Option<&'static str> {
    workspace_layout().selected.as_deref()
}

struct WorkspaceLayout {
    plan: Result<crate::NativeBuildPlan, String>,
    selected: Option<String>,
}

impl WorkspaceLayout {
    fn resolve(env: crate::EnvSnapshot, root: PathBuf) -> Self {
        let plan = crate::NativeBuildPlan::capture(&env, &root).map_err(|error| error.to_string());
        let selected = plan
            .as_ref()
            .ok()
            .and_then(|plan| plan.build_dir().map(str::to_owned));
        Self { plan, selected }
    }

    fn prepare(&self, verb: &str, selected: Option<&str>) -> Result<(), String> {
        if !LANE_VERBS.contains(&verb) {
            return Ok(());
        }
        let plan = self.plan.as_ref().map_err(Clone::clone)?;
        if selected == self.selected.as_deref() {
            plan.prepare()
        } else {
            plan.prepare_target()
        }
        .map_err(|error| error.to_string())
    }
}

fn workspace_layout() -> &'static WorkspaceLayout {
    static LAYOUT: OnceLock<WorkspaceLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| {
        let env = crate::EnvSnapshot::capture();
        let root = super::workspace_root();
        WorkspaceLayout::resolve(env, root)
    })
}

/// The workspace and bench builders share this preparation immediately before a compiling
/// command is constructed. A bench's explicit fixture directory is its own layout choice.
pub(super) fn prepare_workspace_build_dir(
    verb: &str,
    selected: Option<&str>,
) -> Result<(), String> {
    workspace_layout().prepare(verb, selected)
}

pub(super) fn apply_workspace_layout(command: &mut Command) -> Result<(), String> {
    let plan = workspace_layout().plan.as_ref().map_err(Clone::clone)?;
    plan.apply_to_command(command)
        .map_err(|error| error.to_string())
}

/// The captured workspace target shared by artifact readers and native compiler commands.
pub(crate) fn target_dir() -> Result<PathBuf, String> {
    workspace_layout()
        .plan
        .as_ref()
        .map(|plan| plan.target().to_path_buf())
        .map_err(Clone::clone)
}

/// The driver a discovered toolchain may be driven by, or why not.
pub(crate) fn driver_of(toolchain: &crate::Toolchain) -> Result<CargoDriver, String> {
    if toolchain.have_targo() {
        Ok(CargoDriver {
            program: toolchain.targo.clone(),
        })
    } else {
        Err(toolchain.missing_targo_label())
    }
}

/// The driver every perf lane spawns, resolved once per process.
pub(crate) fn cargo_driver() -> Result<&'static CargoDriver, &'static str> {
    static DRIVER: OnceLock<Result<CargoDriver, String>> = OnceLock::new();
    DRIVER
        .get_or_init(|| {
            let root = super::workspace_root();
            driver_of(&crate::run_toolchain(&crate::EnvSnapshot::capture(), &root))
        })
        .as_ref()
        .map_err(String::as_str)
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

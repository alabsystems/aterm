// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pinned `targo` command builder for performance lanes, and the executables its builds report.

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
    #[cfg(unix)]
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

/// Capture a prepared compiler call under ordinary build admission where that boundary is
/// available. The command keeps its environment, handles and configured stdio. Runtime
/// measurements use their own entry policy after this compiler's owning wait has ended.
pub(super) fn compiler_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    #[cfg(unix)]
    {
        atpkg::build_admit::output(command, atpkg::build_cache::Tool::Targo)
    }
    #[cfg(not(unix))]
    {
        command.output()
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
#[cfg(unix)]
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

/// Cargo's exact executable artifact, matched by both target name and target kind.
pub(crate) fn built_executable(messages: &str, name: &str, kind: &str) -> Result<PathBuf, String> {
    built_executable_checked(messages, name, kind, |_| Ok(()))
}

/// A normal debug executable whose compiler profile enables the checkout's lab identity.
/// Test harnesses and missing or disabled debug assertions cannot supply that fixture.
#[cfg(any(unix, test))]
pub(crate) fn built_debug_app(messages: &str, name: &str) -> Result<PathBuf, String> {
    built_executable_checked(messages, name, "bin", |message| {
        let profile = message.get("profile");
        if profile
            .and_then(|p| p.get("debug_assertions"))
            .and_then(aterm_json::Value::as_bool)
            != Some(true)
            || profile
                .and_then(|p| p.get("test"))
                .and_then(aterm_json::Value::as_bool)
                != Some(false)
        {
            return Err(format!(
                "`{name}` lab fixture requires a normal executable with debug assertions enabled"
            ));
        }
        Ok(())
    })
}

fn built_executable_checked(
    messages: &str,
    name: &str,
    kind: &str,
    accept: impl Fn(&aterm_json::Value) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let mut selected: Option<PathBuf> = None;
    for line in messages.lines() {
        let Ok(wire) = aterm_json::from_str::<crate::compiler_message::UnitMessage>(line) else {
            continue;
        };
        let message = match wire {
            crate::compiler_message::UnitMessage::Unit(value) => value,
            crate::compiler_message::UnitMessage::Other => continue,
            crate::compiler_message::UnitMessage::Ambiguous { .. } => {
                return Err("compiler reported ambiguous message fields".into());
            }
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(path) = message
            .get("executable")
            .and_then(aterm_json::Value::as_str)
        else {
            continue;
        };
        let kinds = crate::compiler_message::artifact_target_kinds(&message)?;
        let target = message.get("target");
        let named = target
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            == Some(name);
        let bin = kinds.contains(&kind);
        if named && bin {
            accept(&message)?;
            if selected
                .as_ref()
                .is_some_and(|prior| prior.as_os_str() != std::ffi::OsStr::new(path))
            {
                return Err(format!(
                    "compiler reported conflicting executables for `{name}`"
                ));
            }
            selected.get_or_insert_with(|| PathBuf::from(path));
        }
    }
    selected.ok_or_else(|| format!("cargo reported no `{name}` binary"))
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

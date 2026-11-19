// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-command Apple SDK input for direct native compiler hops.

use super::Tool;
use std::ffi::OsString;

/// Preserve inherited SDK selection; discover only a building command's missing root.
/// This returns a child environment overlay and never changes process-global state.
pub(super) fn environment(tool: Tool, args: &[OsString]) -> Vec<(OsString, OsString)> {
    #[cfg(target_os = "macos")]
    {
        planner::live(tool, args)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (tool, args);
        Vec::new()
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
mod planner {
    use super::{OsString, Tool};
    use crate::build_cache::{self, Machine};
    use std::path::{Path, PathBuf};

    pub(super) const SELECT: &str = "/usr/bin/xcode-select";
    pub(super) const XCRUN: &str = "/usr/bin/xcrun";
    pub(super) const OUTPUT_LIMIT: usize = 4096;

    pub(super) trait Probe {
        fn executable(&mut self, program: &Path) -> bool;
        /// Complete bounded stdout from a successful, already-reaped child.
        fn output(&mut self, program: &Path, args: &[&str]) -> Option<Vec<u8>>;
        fn directory(&mut self, path: &Path) -> bool;
    }

    fn building(tool: Tool, args: &[String], machine: &Machine<'_>) -> Option<Vec<String>> {
        let reading = build_cache::registered::command_reading(args, tool, machine)?;
        if reading.query {
            return None;
        }
        if tool == Tool::Tippy || reading.miri.setup {
            return Some(reading.line);
        }
        let verb = build_cache::subcommand_at(&reading.line)?;
        let verb = reading.line[verb].as_str();
        (verb == "trust" || build_cache::BUILD_VERBS.contains(&verb) && verb != "clean")
            .then_some(reading.line)
    }

    pub(super) fn environment_with(
        tool: Tool,
        args: &[OsString],
        machine: &Machine<'_>,
        probe: &mut dyn Probe,
    ) -> Vec<(OsString, OsString)> {
        if (machine.env)("SDKROOT").is_some_and(|root| !root.is_empty()) {
            return Vec::new();
        }
        // A native pass-through must not reinterpret or replace non-UTF-8 words.
        let Some(words) = args
            .iter()
            .map(|word| word.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
        else {
            return Vec::new();
        };
        let Some(line) = building(tool, &words, machine) else {
            return Vec::new();
        };
        if !build_cache::registered::macos_sdk_platform(&words, &line, tool, machine) {
            return Vec::new();
        }
        let select = Path::new(SELECT);
        let xcrun = Path::new(XCRUN);
        if !probe.executable(select) || !probe.executable(xcrun) {
            return Vec::new();
        }
        // xcrun without selected tools can open Apple's installation dialog.
        let Some(selected) = probe.output(select, &["-p"]) else {
            return Vec::new();
        };
        if selected.len() > OUTPUT_LIMIT {
            return Vec::new();
        }
        let Some(bytes) = probe.output(xcrun, &["--show-sdk-path"]) else {
            return Vec::new();
        };
        if bytes.len() > OUTPUT_LIMIT {
            return Vec::new();
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Vec::new();
        };
        // Shell substitution removes trailing newlines, not spaces in SDK paths.
        let text = text.trim_end_matches('\n');
        if text.len() < 2 || text.contains(['\0', '\r', '\n']) {
            return Vec::new();
        }
        let root = PathBuf::from(text);
        if !root.is_absolute() || !probe.directory(&root) {
            return Vec::new();
        }
        vec![(OsString::from("SDKROOT"), root.into_os_string())]
    }

    #[cfg(target_os = "macos")]
    pub(super) fn live(tool: Tool, args: &[OsString]) -> Vec<(OsString, OsString)> {
        let cwd = std::env::current_dir().ok();
        build_cache::registered::with_live_machine(cwd.as_deref(), |machine| {
            environment_with(tool, args, machine, &mut Live)
        })
    }

    #[cfg(target_os = "macos")]
    struct Live;

    #[cfg(target_os = "macos")]
    impl Probe for Live {
        fn executable(&mut self, program: &Path) -> bool {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(program).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        }

        fn output(&mut self, program: &Path, args: &[&str]) -> Option<Vec<u8>> {
            let mut command = aterm_uds::spawnfd::command(program);
            command.args(args);
            let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
                &mut command,
                std::time::Duration::from_secs(60),
                "selected Apple SDK input",
                OUTPUT_LIMIT,
            )
            .ok()?;
            output.status.success().then_some(output.stdout)
        }

        fn directory(&mut self, path: &Path) -> bool {
            path.is_dir()
        }
    }
}

#[cfg(all(test, unix))]
#[path = "build_admit_sdk_tests.rs"]
mod tests;

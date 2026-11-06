// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Protocol-qualified native routes. The actual mandatory execution flag keeps
//! an updater replacing the probed image with an older image fail-closed.
use super::*;
use aterm_types::resource_admission::Submission;
use std::{
    ffi::{OsStr, OsString},
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::PermissionsExt as _,
    },
    path::Path,
};

pub(super) enum Kind {
    Ay,
    Targo,
}
pub(super) struct Recipe {
    pub(super) kind: Kind,
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<OsString>,
    pub(super) environment: Vec<(OsString, OsString)>,
    pub(super) cwd: PathBuf,
    pub(super) width: u32,
}
impl Recipe {
    pub(super) fn probe(
        request: &Submission,
        physical: u32,
    ) -> io::Result<std::sync::mpsc::Receiver<io::Result<Self>>> {
        // This copy is private input for recipe selection, never a peer or
        // admission credential. Only the queue head may own an active probe.
        let copied = Submission {
            binding: request.binding.clone(),
            peer_pid: request.peer_pid,
            peer_birth: request.peer_birth,
            argv: request.argv.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            cpu_units: request.cpu_units,
            memory_bytes: request.memory_bytes,
            label: request.label.clone(),
            timeout_ms: request.timeout_ms,
        };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("resource-recipe-probe".into())
            .spawn(move || {
                let _ = send.send(Self::qualify(&copied, physical));
            })?;
        Ok(receive)
    }
    pub(super) fn qualify(request: &Submission, physical: u32) -> io::Result<Self> {
        if request.cpu_units > physical {
            return Err(refused("requested CPU width exceeds this host"));
        }
        let mut program = resolve(request)?;
        let executable = program.file_name().and_then(|name| name.to_str());
        let requested = Path::new(OsStr::from_bytes(&request.argv[0]))
            .file_name()
            .and_then(|name| name.to_str());
        let mut arguments = request.argv[1..]
            .iter()
            .map(|arg| OsString::from_vec(arg.clone()))
            .collect::<Vec<_>>();
        let mut environment = request
            .env
            .iter()
            .filter(|(name, value)| !stale_fd_var(name, value))
            .map(|(name, value)| {
                (
                    OsString::from_vec(name.clone()),
                    OsString::from_vec(value.clone()),
                )
            })
            .collect::<Vec<_>>();
        environment
            .retain(|(key, _)| key != "ATERM_SESSION_ID" && key != "ATERM_PARENT_SESSION_ID");
        for key in ["ATERM_SESSION_ID", "ATERM_PARENT_SESSION_ID"] {
            environment.push((key.into(), request.binding.session.clone().into()));
        }
        let cwd = PathBuf::from(OsString::from_vec(request.cwd.clone()));
        let mut kind = None;
        // Never execute an arbitrary argv merely because its basename claims
        // to be a native capability query. Partial overlap requires the actual
        // manager-selected complete artifact, not PATH or stdout as authority.
        let managed = requested
            .filter(|name| matches!(*name, "ay" | "targo"))
            .and_then(|name| managed_artifact(name, &program));
        if managed.is_some()
            && [requested, executable]
                .iter()
                .any(|name| matches!(name, Some("ay" | "targo")))
        {
            let mut probe = spawnfd::command(&program);
            probe
                .arg0(OsStr::from_bytes(&request.argv[0]))
                .arg("--resource-capabilities")
                .env_clear()
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .current_dir(&cwd)
                .stderr(spawnfd::Stdio::null());
            if let Ok(output) = aterm_uds::deadline::output_until_complete_stdout_with_limit(
                &mut probe,
                Instant::now() + Duration::from_secs(2),
                8192,
            ) && output.status.success()
                && let Ok(capability) = aterm_json::from_slice::<aterm_json::Value>(&output.stdout)
            {
                let words = arguments
                    .iter()
                    .map(|word| word.to_str())
                    .collect::<Vec<_>>();
                let words = if words.first() == Some(&Some("--resource-worker-v3")) {
                    &words[1..]
                } else {
                    &words[..]
                };
                let words = if words.first() == Some(&Some("--resource-subgroups-v3")) {
                    &words[1..]
                } else {
                    words
                };
                if requested == Some("ay") && ay(&capability, words) {
                    kind = Some(Kind::Ay);
                }
                if requested == Some("targo") && targo(&capability, words) {
                    kind = Some(Kind::Targo);
                }
            }
        }
        // Declaring a budget does not enforce it on arbitrary foreign code.
        // CPU exclusivity/boot-retained closure alone supplies no memory
        // bound, so only actual maintained enforcement recipes may execute.
        let kind = kind.ok_or_else(|| refused("command has no qualified resource recipe"))?;
        let managed = managed.ok_or_else(|| refused("qualified artifact unavailable"))?;
        program = managed.target;
        for (key, value) in managed.env.entries() {
            environment.retain(|(existing, _)| existing != key.as_str());
            environment.push((key.as_str().into(), value.as_str().into()));
        }
        let width = request.cpu_units;
        if arguments
            .first()
            .is_none_or(|arg| arg != "--resource-worker-v3")
        {
            arguments.insert(0, "--resource-worker-v3".into());
        }
        if arguments
            .get(1)
            .is_none_or(|arg| arg != "--resource-subgroups-v3")
        {
            arguments.insert(1, "--resource-subgroups-v3".into());
        }
        Ok(Self {
            kind,
            program,
            arguments,
            environment,
            cwd,
            width,
        })
    }
    pub(super) fn command(&self, request: &Submission) -> spawnfd::Command {
        let mut command = spawnfd::command(&self.program);
        command
            .arg0(OsStr::from_bytes(&request.argv[0]))
            .args(&self.arguments)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(&self.cwd)
            .start_suspended(true);
        command
    }
}

fn ay(value: &aterm_json::Value, words: &[Option<&str>]) -> bool {
    let command = match words {
        [Some("bench"), Some(command), ..] => format!("bench {command}"),
        _ => return false,
    };
    value["version"].as_u64() == Some(3)
        && value["native_campaign"].as_bool() == Some(true)
        && value["admission"].as_str() == Some("resource-inspect-v3")
        && value["worker_argument"].as_str() == Some("--resource-worker-v3")
        && value["subgroup_custody"].as_str() == Some("host-owned-v3")
        && value["subgroup_argument"].as_str() == Some("--resource-subgroups-v3")
        && ["bench run", "bench chc-gate", "bench sat-delta"].contains(&command.as_str())
        && value["commands"].as_array().is_some_and(|commands| {
            commands
                .iter()
                .any(|value| value.as_str() == Some(command.as_str()))
        })
}
fn targo(value: &aterm_json::Value, words: &[Option<&str>]) -> bool {
    matches!(words, [Some("--unverified"), Some("build" | "check"), ..])
        && value["version"].as_u64() == Some(3)
        && value["cpu_workers"].as_bool() == Some(true)
        && value["memory_custody"].as_str() == Some("resource-inspect-v3")
        && value["worker_argument"].as_str() == Some("--resource-worker-v3")
        && value["subgroup_custody"].as_str() == Some("host-owned-v3")
        && value["subgroup_argument"].as_str() == Some("--resource-subgroups-v3")
}
pub(super) fn stale_fd_var(name: &[u8], value: &[u8]) -> bool {
    matches!(
        name,
        b"TRUST_CPU_HANDOFF"
            | b"ATERM_RESOURCE_INSPECTOR"
            | b"ATERM_RESOURCE_JOB_V3"
            | b"TRUST_JOBSERVER_HANDOFF"
            | b"CARGO_JOBSERVER_HANDOFF"
            | b"BUILD_SLOT_FENCE_FDS"
            | b"BUILD_SLOT_CPU_LEASE_FD"
            | b"BUILD_SLOT_CPU_PROTOCOL"
            | b"BUILD_SLOT_HELD"
    ) || (matches!(name, b"CARGO_MAKEFLAGS" | b"MAKEFLAGS" | b"MFLAGS")
        && value
            .windows(b"--jobserver-".len())
            .any(|word| word == b"--jobserver-"))
}
pub(super) fn resolve(request: &Submission) -> io::Result<PathBuf> {
    let word = OsStr::from_bytes(&request.argv[0]);
    let cwd = Path::new(OsStr::from_bytes(&request.cwd));
    let mut candidates = Vec::new();
    if request.argv[0].contains(&b'/') {
        candidates.push(if Path::new(word).is_absolute() {
            PathBuf::from(word)
        } else {
            cwd.join(word)
        });
    } else {
        let path = request
            .env
            .iter()
            .find(|(key, _)| key == b"PATH")
            .map(|(_, value)| OsStr::from_bytes(value))
            .unwrap_or_else(|| OsStr::new("/usr/bin:/bin"));
        for directory in std::env::split_paths(path) {
            candidates.push(if directory.is_absolute() {
                directory.join(word)
            } else {
                cwd.join(directory).join(word)
            });
        }
    }
    for path in candidates {
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            return path.canonicalize();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "resource command executable unavailable",
    ))
}

pub(super) fn managed_artifact(name: &str, selected: &Path) -> Option<atpkg::ops::Managed> {
    let layout = atpkg::store::resolve_configured()?;
    let managed = atpkg::ops::managed(&layout, name)?;
    let target = managed.target.canonicalize().ok()?;
    let (program, build) = atpkg::ops::store_build_of(&layout.prefix, &target)?;
    if !atpkg::store::build_is_complete(&layout.build_dir(&program, build)) {
        return None;
    }
    let shim = layout
        .shim(&atpkg::store::ToolName::new(name)?)
        .canonicalize()
        .ok()?;
    if selected != shim && selected != target {
        return None;
    }
    Some(atpkg::ops::Managed {
        target,
        env: managed.env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_root_discards_ancestor_jobserver_endpoints() {
        assert!(stale_fd_var(
            b"CARGO_JOBSERVER_HANDOFF",
            b"private-parent-ad"
        ));
        for name in [b"MAKEFLAGS".as_slice(), b"CARGO_MAKEFLAGS", b"MFLAGS"] {
            assert!(stale_fd_var(
                name,
                b"-j --jobserver-auth=fifo:/private/ancestor"
            ));
            assert!(!stale_fd_var(name, b"-k"));
        }
        assert!(!stale_fd_var(b"CARGO_BUILD_JOBS", b"3"));
    }
    #[test]
    fn arbitrary_command_budget_is_not_a_qualified_memory_enforcement_recipe() {
        let pid = std::process::id();
        let request = Submission {
            binding: Binding {
                session: "s-a".into(),
                attempt: [1; 16],
            },
            peer_pid: pid,
            peer_birth: aterm_uds::process::read_birth(i32::try_from(pid).unwrap())
                .unwrap()
                .0,
            argv: vec![b"/bin/echo".to_vec(), b"unqualified".to_vec()],
            cwd: std::env::current_dir()
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: 1024 * 1024,
            label: "unqualified".into(),
            timeout_ms: None,
        };
        let error = Recipe::qualify(&request, 2)
            .err()
            .expect("arbitrary argv must refuse");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
}

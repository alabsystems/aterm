// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release owns its signed prerequisite leases through every child wait.

use std::path::Path;
use std::sync::Arc;

use aterm_uds::spawnfd::{Command, Stdio, command};
use atpkg::prerequisite::{HeldSet, Prerequisite};

/// A publication operation can run guards, Python hooks and Lean proof checks.
/// Names and pins remain owned by the shared package recipes.
pub(crate) const PUBLICATION: &[Prerequisite] = &[
    Prerequisite::Python,
    Prerequisite::Gitleaks,
    Prerequisite::OpenSslSigning,
    Prerequisite::Lake,
];

pub(crate) const REMEDY: &str = "run `aterm pkg install <package>` for each of cpython, gitleaks, openssl-signing and lean4, then retry";

#[derive(Clone, Debug)]
pub(crate) enum Tools {
    Store(Arc<HeldSet>),
    /// A release backend test substitutes child effects explicitly. It must not
    /// inspect the operator's package configuration or acquire real leases.
    #[cfg(test)]
    Simulated,
}

impl Tools {
    pub(crate) fn acquire(wants: &[Prerequisite], who: &str) -> Result<Self, String> {
        atpkg::prerequisite::configured(wants, who).map(|held| Self::Store(Arc::new(held)))
    }

    /// Preserve an explicitly selected child PATH (for example its rustup
    /// directory), putting only the fixed, signed prerequisite names before it.
    pub(crate) fn apply(&self, command: &mut Command) -> Result<(), String> {
        match self {
            Self::Store(held) => {
                let remaining = command
                    .get_envs()
                    .find(|(name, _)| *name == "PATH")
                    .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned))
                    .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
                held.apply(command, &remaining)?;
                command.env_remove("ENV").env_remove("BASH_ENV");
                if let Some(openssl) = held.tool(Prerequisite::OpenSslSigning) {
                    command.env("PUBLISH_OPENSSL", openssl.path());
                }
                if let Some(python) = held.tool(Prerequisite::Python) {
                    command.env("PUBLISH_PYTHON", python.path());
                }
                Ok(())
            }
            #[cfg(test)]
            Self::Simulated => Ok(()),
        }
    }

    /// The engine is a Python script with a shell launcher. Running the held
    /// interpreter directly prevents that launcher's PATH fallback from
    /// selecting another Python when its first probe fails. Isolated imports
    /// ignore inherited PYTHONHOME/PYTHONPATH and the working directory.
    pub(crate) fn engine_command(&self, script: &Path) -> Result<Command, String> {
        match self {
            Self::Store(held) => {
                let python = held
                    .tool(Prerequisite::Python)
                    .ok_or("the operation did not acquire its publication interpreter")?;
                require_engine_pin(script)?;
                let mut command = python.command()?;
                command.arg("-I").arg(script);
                Ok(command)
            }
            #[cfg(test)]
            Self::Simulated => Ok(aterm_uds::spawnfd::command(script)),
        }
    }

    /// The interpreter's `-I <script>` precedes the engine operation.
    pub(crate) const fn engine_prefix_len(&self) -> usize {
        match self {
            Self::Store(_) => 2,
            #[cfg(test)]
            Self::Simulated => 0,
        }
    }

    pub(crate) fn openssl(&self) -> Result<&Path, String> {
        match self {
            Self::Store(held) => {
                let tool = held
                    .tool(Prerequisite::OpenSslSigning)
                    .ok_or("the operation did not acquire its source-signing prerequisite")?;
                tool.validate()?;
                Ok(tool.path())
            }
            #[cfg(test)]
            Self::Simulated => Ok(Path::new("/fixture/openssl")),
        }
    }
}

/// Ask only the launcher's interpreter-independent capability. An old launcher
/// gets an empty PATH, so its fallback loop cannot probe unmanaged Python.
fn require_engine_pin(script: &Path) -> Result<(), String> {
    let helper = script.with_file_name("pub-python");
    let mut probe = command(&helper);
    probe
        .arg("--capabilities")
        .env("PATH", "")
        .env_remove("ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null());
    let output = aterm_uds::deadline::output_within(&mut probe, std::time::Duration::from_secs(60))
        .map_err(|error| {
            format!(
                "publication interpreter contract {}: {error}",
                helper.display()
            )
        })?;
    if !output.status.success() || output.stdout.trim_ascii() != b"publication-pinned-python-v1" {
        return Err(format!(
            "publication engine {} lacks publication-pinned-python-v1; use a current engine whose pub-python honors the sole PUBLISH_PYTHON pin",
            script.display()
        ));
    }
    Ok(())
}

pub(crate) type Acquire = fn(&[Prerequisite], &str) -> Result<Tools, String>;

#[cfg(test)]
pub(crate) fn simulated(_: &[Prerequisite], _: &str) -> Result<Tools, String> {
    Ok(Tools::Simulated)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn publication_pin_contract_is_required_without_a_path_interpreter_probe() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let script = dir.path().join("pub");
        let helper = script.with_file_name("pub-python");
        for body in [
            "#!/bin/sh\nexec python3.14 \"$@\"\n",
            "#!/bin/sh\nprintf old-contract\\n\n",
            "#!/bin/sh\nprintf '%s\\n' publication-pinned-python-v1\nexit 1\n",
        ] {
            std::fs::write(&helper, body).unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(require_engine_pin(&script).is_err());
        }
        std::fs::write(&helper, "#!/bin/sh\n[ -z \"$PATH\" ] || exit 1\n[ \"$#:$1\" = '1:--capabilities' ] || exit 1\nprintf '%s\\n' publication-pinned-python-v1\n").unwrap();
        require_engine_pin(&script).unwrap();
        std::fs::remove_file(&helper).unwrap();
        assert!(require_engine_pin(&script).is_err());
    }
}

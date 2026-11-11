// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signed package authority and the existing closed native-runtime observation
//! remain separate owners. Neither a current file digest nor probe JSON grants resources.
use super::*;
use std::{ffi::OsString, path::Path};
use trust_resource_provenance::program::HeldProgram;

pub(super) struct Selection {
    package: atpkg::prerequisite::HeldTool,
    runtime: HeldProgram,
}

impl Selection {
    pub(super) fn capture(package: atpkg::prerequisite::HeldTool) -> io::Result<Self> {
        let deadline = native_inputs::deadline(Instant::now());
        package.validate().map_err(|error| refused(&error))?;
        let observed = atpkg::sha256_file(package.physical_path())?;
        let mut digest = [0; 32];
        if !aterm_codec::hex::decode_into(observed.as_bytes(), &mut digest) {
            return Err(refused("AY executable digest is malformed"));
        }
        // The digest checks equality with the already signed selection. Source
        // authority comes from package admission, not this observation.
        let runtime =
            HeldProgram::open(package.root(), package.physical_path(), &digest, deadline)?;
        package.validate().map_err(|error| refused(&error))?;
        Ok(Self { package, runtime })
    }

    pub(super) fn physical_path(&self) -> &Path {
        self.package.physical_path()
    }

    pub(super) fn environment(&self) -> &atpkg::shim_env::ShimEnv {
        self.package.environment()
    }

    pub(super) fn runtime_environment(&self) -> io::Result<Vec<(OsString, OsString)>> {
        self.runtime.runtime_environment()
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        self.package.validate().map_err(|error| refused(&error))?;
        native_inputs::validate(&self.runtime, Instant::now())
    }

    pub(super) fn validate_loaded(&self, pid: u32) -> io::Result<()> {
        self.validate()?;
        native_inputs::validate_loaded(
            &self.runtime,
            trust_resource_provenance::process::read_identity(pid)?,
            Instant::now(),
        )
    }

    pub(super) fn configure(&self, command: &mut spawnfd::Command) -> io::Result<()> {
        // Both current callers install this original backing before their
        // other rights. Never replace an already configured guard table.
        if command.guard_targets().next().is_some() {
            return Err(refused(
                "AY backing must be configured before other inherited guards",
            ));
        }
        self.validate()?;
        let lease = aterm_uds::kept_out(|| self.package.lease_descriptor())?;
        command.inherit_guards(vec![lease])?;
        Ok(())
    }
}

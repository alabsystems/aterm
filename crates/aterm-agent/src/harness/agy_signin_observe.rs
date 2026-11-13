// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only proof of an existing consumer grant, independent of new-login authority.

use super::*;

pub(in crate::harness) struct NativeProfile {
    home: PathBuf,
    bytes: SecretBytes,
    profile: super::super::google_profile::GoogleProfile,
}

fn directories(home: &Path) -> Result<(), &'static str> {
    if !home.is_absolute() {
        return Err("Native Google home is not absolute");
    }
    for path in [
        home.to_owned(),
        home.join(".gemini"),
        home.join(".gemini/antigravity-cli"),
    ] {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| "Native Google directory is unavailable")?;
        use std::os::unix::fs::MetadataExt as _;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != aterm_uds::peer::our_uid()
            || meta.mode() & 0o022 != 0
        {
            return Err("Native Google directory is not safely owned");
        }
    }
    Ok(())
}

impl NativeProfile {
    pub(in crate::harness) fn profile(&self) -> &super::super::google_profile::GoogleProfile {
        &self.profile
    }

    pub(in crate::harness) fn check_current(&self) -> Result<(), &'static str> {
        directories(&self.home)?;
        let bytes = grant_bytes(
            &self
                .home
                .join(".gemini/antigravity-cli/antigravity-oauth-token"),
        )?;
        if bytes.as_slice() != self.bytes.as_slice() {
            return Err("Native Google grant changed during observation");
        }
        Ok(())
    }
}

#[cfg(not(test))]
pub(in crate::harness) fn read_native_profile(
    home: &Path,
    expected: &str,
    deadline: Instant,
) -> Result<NativeProfile, super::super::google_profile::Error> {
    read_using(home, expected, deadline, super::super::google_profile::read)
}

fn read_using(
    home: &Path,
    expected: &str,
    deadline: Instant,
    read: impl FnOnce(
        &str,
        &str,
        Duration,
    ) -> Result<
        super::super::google_profile::GoogleProfile,
        super::super::google_profile::Error,
    >,
) -> Result<NativeProfile, super::super::google_profile::Error> {
    if super::super::accounts::email_refusal(expected).is_some() {
        return Err("Native Google observation has no valid expected identity".into());
    }
    directories(home)?;
    let bytes = grant_bytes(&home.join(".gemini/antigravity-cli/antigravity-oauth-token"))?;
    let token = native_access_token(bytes.as_slice())?;
    let wait = deadline
        .checked_duration_since(Instant::now())
        .filter(|wait| !wait.is_zero())
        .ok_or("Google verification deadline expired")?
        .min(Duration::from_secs(30));
    let profile = read(
        token.as_str().ok_or("Native Google token is not text")?,
        expected,
        wait,
    )?;
    if !profile.email.eq_ignore_ascii_case(expected) {
        return Err("Native Google identity does not match the expected account".into());
    }
    let proof = NativeProfile {
        home: home.to_owned(),
        bytes,
        profile,
    };
    proof.check_current()?;
    Ok(proof)
}

#[cfg(test)]
#[path = "agy_signin_observe_tests.rs"]
mod tests;

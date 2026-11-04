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
mod tests {
    use super::*;
    #[test]
    fn native_default_observation_is_read_only_and_binds_exact_grant() {
        for case in 0..7 {
            let root = aterm_tempfile::tempdir().unwrap();
            let dir = root.path().join(".gemini/antigravity-cli");
            aterm_secrets::fsx::ensure_private_dir(&root.path().join(".gemini")).unwrap();
            aterm_secrets::fsx::ensure_private_dir(&dir).unwrap();
            let path = dir.join("antigravity-oauth-token");
            let bytes =
                br#"{"auth_method":"consumer","token":{"access_token":"synthetic-access"}}"#;
            aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
            if case == 4 {
                aterm_secrets::fsx::write_atomic(
                    &path,
                    br#"{"auth_method":"gcp","token":{"access_token":"synthetic-access"}}"#,
                    0o600,
                )
                .unwrap();
            }
            if case == 5 {
                let saved = dir.join("saved");
                std::fs::rename(&path, &saved).unwrap();
                std::os::unix::fs::symlink(&saved, &path).unwrap();
            }
            if case == 6 {
                let saved = root.path().join("saved-dir");
                std::fs::rename(&dir, &saved).unwrap();
                std::os::unix::fs::symlink(&saved, &dir).unwrap();
            }
            let before = std::fs::read(&path).unwrap();
            let called = std::cell::Cell::new(false);
            let result = read_using(
                root.path(),
                "owner@example.com",
                Instant::now() + Duration::from_secs(60),
                |token, expected, _| {
                    assert!(!matches!(case, 4..=6));
                    called.set(true);
                    assert_eq!(token, "synthetic-access");
                    assert_eq!(expected, "owner@example.com");
                    if case == 1 {
                        return Err(super::super::super::google_profile::Error::Unavailable);
                    }
                    if case == 2 {
                        aterm_secrets::fsx::write_atomic(
                            &path,
                            b"changed-during-provider-check",
                            0o600,
                        )
                        .unwrap();
                    }
                    Ok(super::super::super::google_profile::GoogleProfile {
                        email: if case == 3 {
                            "another@example.com"
                        } else {
                            expected
                        }
                        .into(),
                        principal: [7; 32],
                    })
                },
            );
            assert_eq!(result.is_ok(), case == 0, "case {case}");
            assert_eq!(called.get(), case < 4);
            if case != 2 {
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            if let Ok(proof) = result {
                assert_eq!(proof.profile().principal, [7; 32]);
                proof.check_current().unwrap();
                aterm_secrets::fsx::write_atomic(&path, b"changed-after-provider-check", 0o600)
                    .unwrap();
                assert!(proof.check_current().is_err());
            }
        }
    }
}

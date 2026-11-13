// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed mailbox inputs from one root-bound secret. No ambient configuration
//! is merged and no OAuth session, environment or command is accepted.

use super::super::super::{accounts, headless_oauth::MailboxConfig, login_drive};
use aterm_json::Value;
use std::path::{Path, PathBuf};

const INPUT_LIMIT: usize = 64 * 1024;
#[cfg(unix)]
const INPUT_FILE: &str = "mailbox.json";
#[cfg(unix)]
const INPUT_PREFIX: &str = "aterm-approved-mailbox-";

pub struct MailboxInputs {
    mailbox: MailboxConfig,
    #[cfg(unix)]
    bytes: aterm_secrets::SecretBytes,
}

#[cfg(unix)]
struct PrivateInputBuffer(Vec<u8>);

#[cfg(unix)]
impl Drop for PrivateInputBuffer {
    fn drop(&mut self) {
        aterm_uds::secret::scrub(&mut self.0);
    }
}

/// One flow's private input file. Dropping it removes the file and directory.
pub struct StagedMailbox {
    _directory: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl StagedMailbox {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl MailboxInputs {
    /// Parse `mailbox-v1` only after the caller verifies the root-bound entry.
    pub fn decode(bytes: &[u8], target: &str) -> Result<Self, String> {
        if bytes.len() > INPUT_LIMIT {
            return Err("approved mailbox inputs exceed their limit".into());
        }
        let value: Value = aterm_json::from_slice(bytes)
            .map_err(|_| "approved mailbox inputs are not valid JSON")?;
        let mailbox = Self::decode_value(&value, target);
        super::wipe_json(value);
        Ok(Self {
            mailbox: mailbox?,
            #[cfg(unix)]
            bytes: aterm_secrets::SecretBytes::from(bytes),
        })
    }

    fn decode_value(value: &Value, target: &str) -> Result<MailboxConfig, String> {
        let object = value
            .as_object()
            .ok_or("approved mailbox inputs must be an object")?;
        if object.keys().any(|key| {
            ![
                "schema",
                "kind",
                "target",
                "transport",
                "endpoint",
                "user",
                "password",
                "bearer_token",
            ]
            .contains(&key.as_str())
        }) || value["schema"].as_u64() != Some(1)
            || value["kind"].as_str() != Some("mailbox")
        {
            return Err("unsupported approved mailbox input format".into());
        }
        let word = |key: &str, max: usize| -> Result<Option<String>, String> {
            value
                .get(key)
                .map(|item| {
                    item.as_str()
                        .filter(|text| {
                            !text.is_empty()
                                && text.len() <= max
                                && !text.chars().any(char::is_control)
                        })
                        .map(str::to_owned)
                        .ok_or_else(|| "invalid approved mailbox input field".into())
                })
                .transpose()
        };
        let named = word("target", 254)?.ok_or("approved mailbox inputs have no target")?;
        if accounts::email_refusal(&named).is_some() || !accounts::email_matches(&named, target) {
            return Err("approved mailbox inputs name another account".into());
        }
        let endpoint = word("endpoint", 2048)?.ok_or("approved mailbox has no endpoint")?;
        let user = word("user", 254)?;
        let password = word("password", 8192)?;
        let bearer_token = word("bearer_token", 8192)?;
        let mut mailbox = MailboxConfig {
            user,
            password,
            routed: true,
            ..Default::default()
        };
        match value["transport"].as_str() {
            Some("imaps") => {
                let server = endpoint
                    .strip_prefix("imaps://")
                    .ok_or("approved mailbox must use TLS")?;
                let (authority, folder) =
                    server.split_once('/').map_or((server, ""), |(a, p)| (a, p));
                let host = authority
                    .split_once(':')
                    .map_or(authority, |(host, _)| host);
                if host.is_empty()
                    || host.starts_with(['.', '-'])
                    || host.ends_with(['.', '-'])
                    || !host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                    || authority
                        .split_once(':')
                        .is_some_and(|(_, p)| !crate::parse_uint::<u16>(p).is_some_and(|p| p > 0))
                    || folder.len() > 64
                    || !folder
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    || mailbox.user.is_none()
                    || mailbox.password.is_none()
                    || bearer_token.is_some()
                {
                    return Err("invalid approved TLS mailbox route".into());
                }
                mailbox.imap_url = Some(endpoint);
            }
            Some("https") => {
                if login_drive::https_host(&endpoint).is_none()
                    || endpoint.contains(['?', '#'])
                    || (bearer_token.is_some()
                        == (mailbox.user.is_some() || mailbox.password.is_some()))
                    || (bearer_token.is_none()
                        && (mailbox.user.is_none() || mailbox.password.is_none()))
                {
                    return Err("invalid approved HTTPS mailbox route".into());
                }
                mailbox.http_url = Some(endpoint);
                mailbox.bearer_token = bearer_token;
            }
            _ => return Err("approved mailbox transport is unsupported".into()),
        }
        Ok(mailbox)
    }

    /// Provider-specific sender/freshness restrictions are added by the driver.
    #[must_use]
    pub fn mailbox(&self) -> MailboxConfig {
        self.mailbox.clone()
    }

    /// Carry exactly the approved input to the native opener, without secret
    /// command arguments or environment values. The staging home stays private.
    pub fn stage_in(&self, staging: &Path) -> Result<StagedMailbox, String> {
        #[cfg(unix)]
        {
            aterm_uds::private_file::check_private_dir(staging)
                .map_err(|_| "approved mailbox staging is not private")?;
            let directory = aterm_tempfile::Builder::new()
                .prefix(INPUT_PREFIX)
                .tempdir_in(staging)
                .map_err(|_| "approved mailbox input directory could not be made")?;
            aterm_uds::private_file::write_private(
                directory.path(),
                INPUT_FILE,
                self.bytes.as_slice(),
            )
            .map_err(|_| "approved mailbox input could not be written privately")?;
            let path = directory.path().join(INPUT_FILE);
            Ok(StagedMailbox {
                _directory: directory,
                path,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = staging;
            Err("private approved mailbox staging is unavailable".into())
        }
    }

    /// Read only this flow's explicit private input. Any refusal is final:
    /// callers must not replace it with ambient mailbox discovery.
    pub fn read_staged(path: &Path, target: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let directory = path
                .parent()
                .ok_or("approved mailbox input has no directory")?;
            let staging = directory
                .parent()
                .ok_or("approved mailbox input has no staging home")?;
            if !path.is_absolute()
                || path.file_name().and_then(|n| n.to_str()) != Some(INPUT_FILE)
                || !directory
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(INPUT_PREFIX))
            {
                return Err("approved mailbox input is outside its flow directory".into());
            }
            aterm_uds::private_file::check_private_dir(staging)
                .map_err(|_| "approved mailbox staging is not private")?;
            let mut bytes = PrivateInputBuffer(vec![0; INPUT_LIMIT]);
            let read = aterm_uds::private_file::read_private_into(
                directory,
                INPUT_FILE,
                INPUT_LIMIT,
                &mut bytes.0,
            )
            .map_err(|_| "approved mailbox input could not be read privately")?;
            Self::decode(&bytes.0[..read.len], target)
        }
        #[cfg(not(unix))]
        {
            let _ = (path, target);
            Err("private approved mailbox inputs are unavailable".into())
        }
    }
}

#[cfg(test)]
#[path = "cli_shared_inputs_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local identity of repository inputs. It observes only the parsed GitHub profile,
//! its selected credential and default inventory, so unrelated init settings do not stop work.

use super::{Asked, Github, TOKEN_FILE, asked, inventory, profile, read_token};
use crate::model::User;
use crate::port::System;
use std::path::Path;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Stamp([u8; 32]);

impl Stamp {
    pub(super) fn matches_config(
        sys: &dyn System,
        user: &User,
        expected: &Github,
    ) -> Result<bool, String> {
        Ok(
            match asked(profile(sys, user)?.and_then(|profile| profile.github), user)? {
                Asked::Scoped(current) | Asked::Profile(current) => current == *expected,
                Asked::Default | Asked::Off => false,
            },
        )
    }

    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let asked = asked(profile(sys, user)?.and_then(|profile| profile.github), user)?;
        let (kind, source, default) = match asked {
            Asked::Off => return Err("repository setup is disabled in init.toml".into()),
            Asked::Default => (
                b"default".as_slice(),
                Github {
                    account: String::new(),
                    owner: String::new(),
                    token_file: Path::new(&user.home).join(TOKEN_FILE),
                    token_key: None,
                },
                true,
            ),
            Asked::Scoped(source) => (b"scoped".as_slice(), source, false),
            Asked::Profile(source) => (b"profile".as_slice(), source, false),
        };
        let token = read_token(sys, user, &source).map_err(|why| why.why)?;
        let inventory = if default {
            sys.read_owned_file(
                &Path::new(&user.home).join(inventory::FILE),
                user.uid,
                inventory::MAX_BYTES,
                true,
            )
            .map_err(|_| "the private repository inventory is not ready".to_owned())?
        } else {
            Vec::new()
        };
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm repository inputs/1\0");
        let account = source.account.to_ascii_lowercase();
        let owner = source.owner.to_ascii_lowercase();
        for bytes in [
            kind,
            account.as_bytes(),
            owner.as_bytes(),
            source.token_file.as_os_str().as_encoded_bytes(),
            source.token_key.as_deref().unwrap_or("").as_bytes(),
            token.bytes(),
            inventory.as_slice(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(Self(digest.finalize()))
    }

    pub(super) fn is_current(&self, sys: &dyn System, user: &User) -> Result<bool, String> {
        Self::read(sys, user).map(|now| now == *self)
    }
}

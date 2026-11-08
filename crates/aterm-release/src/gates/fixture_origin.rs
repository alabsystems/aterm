// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit read authority for policy tests over file repositories or scripted
//! Git. Production gates receive the native `Origin`; this module is test-only.

use crate::ledger::{GitRunner, Origin, RefWrite, git_ok, rev_parse};
use std::path::Path;

pub(crate) struct FixtureOrigin<'a>(pub &'a dyn GitRunner);

impl Origin for FixtureOrigin<'_> {
    fn refresh_main(&self) -> Result<String, String> {
        git_ok(self.0, &["fetch", "origin", "main"]).map_err(|e| e.to_string())?;
        rev_parse(self.0, "origin/main").map_err(|e| e.to_string())
    }

    fn get(&self, reference: &str) -> Result<Option<String>, String> {
        let out = git_ok(self.0, &["ls-remote", "--tags", "origin", reference])
            .map_err(|e| e.to_string())?;
        Ok(out
            .stdout_utf8()
            .split_whitespace()
            .next()
            .map(str::to_owned))
    }

    fn import(&self, oid: &str) -> Result<(), String> {
        git_ok(
            self.0,
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--recurse-submodules=no",
                "origin",
                oid,
            ],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn tag_catalog(&self) -> Result<String, String> {
        git_ok(self.0, &["ls-remote", "--tags", "origin"])
            .map(|out| out.stdout_utf8())
            .map_err(|e| e.to_string())
    }

    fn tag_commit(&self, tag: &str) -> Result<Option<String>, String> {
        let name = format!("refs/tags/{tag}");
        let out = git_ok(
            self.0,
            &[
                "ls-remote",
                "--tags",
                "origin",
                &name,
                &format!("{name}^{{}}"),
            ],
        )
        .map_err(|e| e.to_string())?;
        let found = super::tag_commits(&out.stdout_utf8()).remove(tag);
        if let Some(commit) = &found {
            self.import(commit)?;
        }
        Ok(found)
    }

    fn submodule_tip(&self, path: &Path, reference: &str) -> Result<String, String> {
        let path = path.to_str().ok_or("fixture submodule path is not UTF-8")?;
        git_ok(
            self.0,
            &[
                "-C",
                path,
                "fetch",
                "--quiet",
                "--no-tags",
                "origin",
                reference,
            ],
        )
        .map_err(|e| e.to_string())?;
        let out = git_ok(
            self.0,
            &["-C", path, "rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        )
        .map_err(|e| e.to_string())?;
        let tip = out.stdout_utf8().trim().to_owned();
        if !agh::path::is_sha(&tip) {
            return Err("fixture submodule snapshot is not an object ID".into());
        }
        Ok(tip)
    }

    fn peel(&self, _: &str) -> Result<Option<String>, String> {
        Err("this gate fixture supplies no token peels".into())
    }

    fn tag_object(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("this gate fixture supplies no token bodies".into())
    }

    fn write(&self, _: &str, _: Option<&str>, _: &str) -> Result<RefWrite, String> {
        Err("gate fixtures do not write remote refs".into())
    }

    fn delete(&self, _: &str, _: &str) -> Result<RefWrite, String> {
        Err("gate fixtures do not delete remote refs".into())
    }

    fn control_transaction(
        &self,
        _: crate::ledger::ControlRefTransaction,
    ) -> Result<RefWrite, String> {
        Err("gate fixtures do not mutate release control refs".into())
    }
}

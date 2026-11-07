// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact native ref writes. An uncertain outcome is never a lost lease.

use super::{BUDGET, Remote, origin_slug};
use agh::git::GitRun;
use agh::github::refs::{AtomicRefBatch, RefUpdate};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RefWrite {
    Written,
    Conflict,
}

impl Remote {
    /// Publish one exact local object with the caller's expected old ref.
    /// Native agh owns authentication, ancestry checks and the durable intent.
    pub(crate) fn write_ref(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<RefWrite, String> {
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native ref write".into());
        }
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: reference.into(),
            before: before
                .unwrap_or("0000000000000000000000000000000000000000")
                .into(),
            after: after.into(),
        }])
        .map_err(|error| self.client.redactor().apply(&error.why))?;
        let run = GitRun::push_refs(&self.slug, &self.repo, batch, false)
            .with_named(self.named.clone())
            .with_budget(BUDGET);
        match self.client.run_git(&run) {
            Ok(_) => Ok(RefWrite::Written),
            Err(error) if is_conflict(&error) => Ok(RefWrite::Conflict),
            Err(error) => Err(self.client.redactor().apply(&format!(
                "native ref write: {} ({}; side_effect={}); {}",
                error.why,
                error.kind.slug(),
                error.side_effect.slug(),
                error.next
            ))),
        }
    }
}

fn is_conflict(error: &agh::Error) -> bool {
    error.kind == agh::error::Kind::Conflict && error.side_effect == agh::error::SideEffect::None
}

/// A scratch-only writer route for actual native local-bare claim controls.
#[cfg(all(test, unix))]
pub(crate) fn claim_fixture(repo: &std::path::Path, bare: &std::path::Path) -> Remote {
    use std::os::unix::fs::PermissionsExt;
    let state = repo.join(".git/claim-fixture");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let dirs = agh::config::Dirs::under(&state);
    aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    let token = dirs.home.join("token");
    std::fs::write(&token, "fixture-claim-not-a-real-credential\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = r#"
        [profiles.fixture]
        token_file = "~/token"
        tier = "dev"
        login = "fixture"
        [[owners]]
        name = "fixture"
        tier = "dev"
        read = ["fixture"]
        write = "fixture"
        [default]
        read = ["fixture"]
        "#;
    std::fs::write(dirs.config_file(), config).unwrap();
    std::fs::set_permissions(dirs.config_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let router = agh::policy::Router::from_validated(agh::config::load(&dirs).unwrap().unwrap());
    let env = agh::identity::MapEnv::new().with("PATH", &std::env::var("PATH").unwrap());
    let client = agh::GithubBuilder::new(router)
        .env(std::sync::Arc::new(env))
        .journal(dirs)
        // Local-bare transport exchanges no credentials. Any accidental helper
        // invocation must fail rather than run this integration test as a CLI.
        .git_helper_for_tests(agh::git::HelperCommand {
            exe: "/usr/bin/false".into(),
            args: Vec::new(),
        })
        .git_remote_for_tests("fixture/app", bare)
        .build();
    Remote::new(client, agh::Slug::parse("fixture/app").unwrap(), repo).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agh::error::{Kind, SideEffect};

    #[test]
    fn an_uncertain_native_write_is_never_a_lost_lease() {
        assert!(is_conflict(&agh::Error::new(Kind::Conflict, "changed")));
        for error in [
            agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Unknown),
            agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Applied),
            agh::Error::new(Kind::OutcomeUnknown, "non-fast-forward; [rejected]")
                .with_side_effect(SideEffect::Unknown),
            agh::Error::new(Kind::CouldNotRun, "fetch first"),
        ] {
            assert!(!is_conflict(&error));
        }
    }
}

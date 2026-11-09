// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact native ref writes. An uncertain outcome is never a lost lease.

use super::{Remote, STALL_WINDOW, origin_slug};
use agh::git::GitRun;
use agh::github::refs::{AtomicRefBatch, RefUpdate};
use agh::policy::{RouteRequest, RouteTarget};

#[derive(Debug, PartialEq, Eq)]
pub enum RefWrite {
    Written,
    Conflict,
    /// A transport/receive failure with no observed ref effects. The caller
    /// must re-read its exact preconditions before repeating the transaction.
    Transient(String),
}

impl Remote {
    /// The canonical control protocol has its own validated operation; ordinary
    /// immutable tags and commit-ancestry updates never acquire this authority.
    pub(crate) fn control_refs(
        &self,
        transaction: agh::git::ControlRefTransaction,
    ) -> Result<RefWrite, String> {
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native control transaction".into());
        }
        let request = RouteRequest::write(RouteTarget::Repo(self.slug.clone()))
            .with_named(self.named.clone());
        let permit = self
            .client
            .permit(&request)
            .map_err(|error| self.native_write_error(&error))?;
        let intents = self
            .client
            .journal(&self.slug)
            .map_err(|error| self.native_write_error(&error))?;
        let result = self
            .client
            .refs(&self.slug)
            .push_control_with_progress_window(
                &permit,
                &self.repo,
                &transaction,
                &intents,
                STALL_WINDOW,
            )
            .and_then(agh::write::Outcome::into_result);
        match result {
            Err(error) if is_transient(&error) => {
                Ok(RefWrite::Transient(self.native_write_error(&error)))
            }
            result => self.ref_result(result),
        }
    }

    /// Publish one exact local object with the caller's expected old ref.
    /// Native agh owns authentication, ancestry checks and the durable intent.
    pub(crate) fn write_ref(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<RefWrite, String> {
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: reference.into(),
            before: before
                .unwrap_or("0000000000000000000000000000000000000000")
                .into(),
            after: after.into(),
        }])
        .map_err(|error| self.client.redactor().apply(&error.why))?;
        self.write_batch(batch)
    }

    /// Preserve a caller's one atomic expected-OID transaction.
    pub(crate) fn write_batch(&self, batch: AtomicRefBatch) -> Result<RefWrite, String> {
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native ref write".into());
        }
        let request = RouteRequest::write(RouteTarget::Repo(self.slug.clone()))
            .with_named(self.named.clone());
        let permit = self
            .client
            .permit(&request)
            .map_err(|error| self.native_write_error(&error))?;
        let intents = self
            .client
            .journal(&self.slug)
            .map_err(|error| self.native_write_error(&error))?;
        // run_git is only the isolated runner. The resource boundary opens and
        // settles the repository-wide intent, preserving unknown outcomes.
        let result = self
            .client
            .refs(&self.slug)
            .push_atomic_with_progress_window(&permit, &self.repo, &batch, &intents, STALL_WINDOW)
            .and_then(agh::write::Outcome::into_result);
        self.ref_result(result)
    }

    /// Delete one exact owned ref. A grouped delete needs its own atomic native
    /// operation and must never be decomposed into calls to this method.
    pub(crate) fn delete_ref(
        &self,
        reference: &str,
        expected_oid: &str,
    ) -> Result<RefWrite, String> {
        self.run(GitRun::check_local(&self.slug, &self.repo))?;
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native ref delete".into());
        }
        let run = GitRun::push_delete(&self.slug, reference, expected_oid)
            .with_named(self.named.clone())
            .with_progress_window(STALL_WINDOW);
        // Ordinary exact deletes converge on absence. Mutable control refs use
        // the separate journaled control transaction, including grouped unlock.
        self.ref_result(self.client.run_git(&run))
    }

    fn ref_result<T>(&self, result: agh::Result<T>) -> Result<RefWrite, String> {
        match result {
            Ok(_) => Ok(RefWrite::Written),
            Err(error) if is_conflict(&error) => {
                eprintln!("{}", self.native_write_error(&error));
                Ok(RefWrite::Conflict)
            }
            Err(error) => Err(self.native_write_error(&error)),
        }
    }

    fn native_write_error(&self, error: &agh::Error) -> String {
        self.client.redactor().apply(&format!(
            "native ref write: {} ({}; side_effect={}); {}",
            error.why,
            error.kind.slug(),
            error.side_effect.slug(),
            error.next
        ))
    }
}

fn is_conflict(error: &agh::Error) -> bool {
    error.side_effect == agh::error::SideEffect::None && error.kind == agh::error::Kind::Conflict
}

fn is_transient(error: &agh::Error) -> bool {
    error.kind == agh::error::Kind::CouldNotRun && error.side_effect == agh::error::SideEffect::None
}

/// A scratch-only writer route for actual native local-bare claim controls.
#[cfg(test)]
pub(crate) fn engine_fixture(
    repo: &std::path::Path,
    source: &std::path::Path,
) -> Result<Remote, String> {
    let git = crate::ledger::GitCli::new(source);
    let recorded = crate::ledger::git_ok(&git, &["config", "--get", "remote.fixture.url"])
        .or_else(|_| crate::ledger::git_ok(&git, &["remote", "get-url", "origin"]))
        .map_err(|e| e.to_string())?
        .stdout_utf8();
    let bare = std::path::Path::new(recorded.trim());
    if !bare.is_absolute() || !bare.is_dir() {
        return Err("engine fixture needs its explicit owned bare repository".into());
    }
    if repo != source {
        let git = crate::ledger::GitCli::new(repo);
        for (key, value) in [
            ("remote.fixture.url", recorded.trim()),
            ("remote.origin.url", "https://github.com/fixture/app.git"),
        ] {
            crate::ledger::git_ok(&git, &["config", key, value]).map_err(|e| e.to_string())?;
        }
    }
    Ok(claim_fixture(repo, bare))
}

#[cfg(test)]
pub(crate) fn claim_fixture(repo: &std::path::Path, bare: &std::path::Path) -> Remote {
    let mut command = crate::ledger::object_git(repo);
    command.args(["rev-parse", "--absolute-git-dir"]);
    let out = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
        &mut command,
        STALL_WINDOW,
        "git rev-parse absolute metadata directory",
    )
    .unwrap();
    assert!(
        out.status.success(),
        "fixture needs a Git metadata directory"
    );
    let git_dir = std::path::PathBuf::from(std::str::from_utf8(&out.stdout).unwrap().trim());
    assert!(git_dir.is_absolute());
    let state = git_dir.join("claim-fixture");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let dirs = agh::config::Dirs::under(&state);
    aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    aterm_uds::private_file::write_private(
        &dirs.home,
        "token",
        b"fixture-claim-not-a-real-credential\n",
    )
    .unwrap();
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
    aterm_uds::private_file::write_private(
        &dirs.config,
        dirs.config_file().file_name().unwrap().to_str().unwrap(),
        config.as_bytes(),
    )
    .unwrap();
    let router = agh::policy::Router::from_validated(agh::config::load(&dirs).unwrap().unwrap());
    let env = agh::identity::MapEnv::new().with("PATH", &std::env::var("PATH").unwrap());
    let transport = std::sync::Arc::new(agh::wire::scripted::ScriptedTransport::new());
    // Only repository/identity guard reads use this HTTP fixture. The ref wire
    // remains the real local-bare native transport, not a scripted write.
    let metadata = br#"{"id":1,"login":"fixture","name":"app","full_name":"fixture/app","owner":{"login":"fixture","id":1},"private":true,"permissions":{"push":true}}"#;
    for _ in 0..128 {
        transport.answer(200, &[], metadata);
    }
    #[cfg(unix)]
    let helper = agh::git::HelperCommand {
        exe: "/usr/bin/false".into(),
        args: Vec::new(),
    };
    #[cfg(windows)]
    let helper = agh::git::HelperCommand {
        exe: std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe"),
        args: ["/D", "/C", "exit", "1"]
            .into_iter()
            .map(Into::into)
            .collect(),
    };
    let client = agh::GithubBuilder::new(router)
        .env(std::sync::Arc::new(env))
        .transport(transport)
        .fx(std::sync::Arc::new(agh::Fx::simulated(1_760_000_000)))
        .journal(dirs)
        // Local-bare transport exchanges no credentials. Any accidental helper
        // invocation must fail rather than run this integration test as a CLI.
        .git_helper_for_tests(helper)
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
            agh::Error::new(Kind::Refused, "cannot lock ref 'refs/heads/main'"),
        ] {
            assert!(!is_conflict(&error));
        }
    }

    #[test]
    fn only_a_settled_transport_failure_is_retryable() {
        assert!(is_transient(&agh::Error::could_not_run("HTTP 503")));
        for error in [
            agh::Error::could_not_run("HTTP 503").with_side_effect(SideEffect::Unknown),
            agh::Error::new(Kind::Conflict, "stale info"),
            agh::Error::refused("protected"),
        ] {
            assert!(!is_transient(&error));
        }
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private-FS and effect-model controls. No provider, compiler, copied executable or service.

use super::*;

fn config(home: &Path) -> Config {
    Config {
        schema: 1,
        program: "claude-code-alab".into(),
        checkout: home.join("source"),
        out: home.join("output"),
        prefix: home.join("prefix"),
        target: crate::triple::Triple::host()
            .expect("native test host")
            .as_str()
            .into(),
        policy: LocalPolicy::Unverified,
        profile: LocalProfile::Release,
        jobs: 1,
        poll_seconds: DEFAULT_POLL_SECONDS,
        toolchain: None,
        memory_mib: 8192,
        vm_mib: 4_194_304,
        pass_seconds: 21_600,
        disk_mib: 65_536,
    }
}

fn version(config: &Config) -> Version {
    Version {
        program: config.program.clone(),
        command: "claude-alab".into(),
        source: "a".repeat(40),
        compiler: "b".repeat(40),
        version: "1.2.0+gaaaaaaaaa".into(),
        build: 7,
        target: config.target.clone(),
        policy: "unverified".into(),
        profile: "release".into(),
    }
}

fn local(version: &Version) -> LocalIdentity {
    LocalIdentity {
        program: version.program.clone(),
        command: version.command.clone(),
        source: version.source.clone(),
        compiler: version.compiler.clone(),
        version: version.version.clone(),
        build: version.build,
        target: version.target.clone(),
        policy: version.policy.clone(),
        profile: version.profile.clone(),
    }
}

struct Effects {
    calls: Vec<&'static str>,
    desired: Version,
    selected: Option<Version>,
    fail_stage: bool,
    fail_plan: bool,
    lose_ack: bool,
    fail_readback: bool,
    conflict: bool,
    reject_selection: bool,
    selection_refusal_current_unknown: bool,
    unknown: bool,
    expected: Option<ExpectedCurrent>,
}

impl Effects {
    fn new(config: &Config) -> Self {
        Self {
            calls: Vec::new(),
            desired: version(config),
            selected: None,
            fail_stage: false,
            fail_plan: false,
            lose_ack: false,
            fail_readback: false,
            conflict: false,
            reject_selection: false,
            selection_refusal_current_unknown: false,
            unknown: false,
            expected: None,
        }
    }
}

impl Driver for Effects {
    type Plan = ();
    type Candidate = ();
    fn current(&mut self, _: &Config) -> Result<CurrentLocal> {
        self.calls.push("current");
        if self.unknown || (self.fail_readback && self.expected.is_some()) {
            self.fail_readback = false;
            return Err(Error::refused("injected unknown current"));
        }
        Ok(self
            .selected
            .as_ref()
            .map_or(CurrentLocal::Absent, |identity| {
                CurrentLocal::Selected(local(identity))
            }))
    }
    fn update_source(&mut self, _: &Config) -> Result<String> {
        self.calls.push("fetch");
        Ok("a".repeat(40))
    }
    fn plan(&mut self, _: &Config, _: &str) -> Result<()> {
        self.calls.push("plan");
        if self.fail_plan {
            return Err(Error::refused("current source no longer matches its plan"));
        }
        Ok(())
    }
    fn identity(&self, _: &Config, _: &()) -> Result<Version> {
        Ok(self.desired.clone())
    }
    fn build(&mut self, _: &Config, _: &()) -> Result<PathBuf> {
        self.calls.push("build");
        Ok("/not-opened/complete".into())
    }
    fn stage(&mut self, _: &Config, _: &(), _: &Path) -> Result<()> {
        self.calls.push("stage");
        if self.fail_stage {
            Err(Error::refused("partial stage preserved"))
        } else {
            Ok(())
        }
    }
    fn select(
        &mut self,
        _: &Config,
        _: &(),
        _: &(),
        expected: ExpectedCurrent,
    ) -> std::result::Result<(), SelectionError> {
        self.calls.push("select");
        self.expected = Some(expected);
        if self.reject_selection {
            // Another selector can already have selected this identity. That
            // observation cannot authorize this manager's rejected source.
            self.selected = Some(self.desired.clone());
            self.unknown = self.selection_refusal_current_unknown;
            return Err(SelectionError::Rejected(Error::refused(
                "source changed before selection",
            )));
        }
        if self.conflict {
            let mut other = self.desired.clone();
            other.source = "c".repeat(40);
            other.build = 9;
            self.selected = Some(other);
            return Err(SelectionError::Uncertain(Error::refused(
                "competing current changed the CAS token",
            )));
        }
        self.selected = Some(self.desired.clone());
        if self.lose_ack {
            Err(SelectionError::Uncertain(Error::cannot_run(
                "lost selection ACK",
            )))
        } else {
            Ok(())
        }
    }
}

fn world() -> (aterm_tempfile::TempDir, PathBuf, Config) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("harness-manager-source-")
        .tempdir()
        .expect("private temp");
    let home = tmp.path().canonicalize().expect("canonical fixture home");
    aterm_uds::private_file::create_private_dir(&home.join("source")).expect("owned source");
    let config = config(&home);
    (tmp, home, config)
}

#[test]
fn first_pass_is_immediate_and_orders_current_cas_and_readback() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    assert_eq!(status.retry_seconds, 0);
    let mut effects = Effects::new(&config);
    let mut saved = Vec::new();
    pass(&config, &mut status, &mut effects, |state| {
        saved.push(state.clone());
        Ok(())
    })
    .expect("one pass");
    assert_eq!(
        effects.calls,
        [
            "current", "fetch", "plan", "build", "stage", "select", "current"
        ]
    );
    assert_eq!(effects.expected, Some(ExpectedCurrent::Absent));
    assert_eq!(status.selected, status.compiled);
    assert_eq!(status.selected, status.source_version);
    assert_eq!(status.pending, None);
    assert_eq!(status.retry_seconds, DEFAULT_POLL_SECONDS);
    assert!(!status.live_adoption);
    assert!(
        saved
            .iter()
            .any(|state| state.pending == Some(Operation::Select))
    );
}

#[test]
fn unchanged_checked_identity_does_not_build_or_select_again() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.selected = Some(effects.desired.clone());
    pass(&config, &mut status, &mut effects, |_| Ok(())).expect("up to date");
    assert_eq!(effects.calls, ["current", "fetch", "plan"]);
    assert!(
        status.compiled.is_none(),
        "observed current is not proof this manager compiled it"
    );
    assert!(status.selected.is_some());
}

#[test]
fn lost_ack_is_reconciled_without_repeating_the_selection() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.lose_ack = true;
    pass(&config, &mut status, &mut effects, |_| Ok(())).expect("checked current proves selection");
    assert_eq!(
        effects
            .calls
            .iter()
            .filter(|&&call| call == "select")
            .count(),
        1
    );
    assert!(status.reason.contains("uncertain acknowledgement"));
    assert_eq!(status.pending, None);
}

#[test]
fn uncertain_readback_retains_select_marker_then_rechecks_source_without_reselecting() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.fail_readback = true;
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert_eq!(status.pending, Some(Operation::Select));
    effects.calls.clear();
    pass(&config, &mut status, &mut effects, |_| Ok(()))
        .expect("next checked readback settles ACK");
    assert_eq!(effects.calls, ["current", "fetch", "plan"]);
    assert_eq!(status.pending, None);
}

#[test]
fn resumed_matching_selection_cannot_skip_fresh_source_validation() {
    let config = config(Path::new("/not-opened"));
    let mut effects = Effects::new(&config);
    effects.selected = Some(effects.desired.clone());
    effects.fail_plan = true;
    let mut status = Status {
        pending: Some(Operation::Select),
        compiled: Some(effects.desired.clone()),
        ..Status::default()
    };
    let error = pass(&config, &mut status, &mut effects, |_| Ok(()))
        .expect_err("historical selection cannot establish source freshness");
    assert!(
        error
            .to_string()
            .contains("current source no longer matches")
    );
    assert_eq!(effects.calls, ["current", "fetch", "plan"]);
    assert_eq!(
        status.pending, None,
        "prior rename was conclusively observed"
    );
    assert_eq!(status.failures, 1);
    assert_eq!(status.selected.as_ref(), Some(&effects.desired));
}

#[test]
fn matching_current_cannot_override_a_known_source_refusal() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.reject_selection = true;
    let error = pass(&config, &mut status, &mut effects, |_| Ok(()))
        .expect_err("matching readback cannot accept rejected source");
    assert!(
        error
            .to_string()
            .contains("source changed before selection")
    );
    assert_eq!(status.pending, None, "no selection write was attempted");
    assert_eq!(status.selected.as_ref(), Some(&effects.desired));
    assert_eq!(status.failures, 1);
    assert_eq!(effects.calls.last(), Some(&"current"));
}

#[test]
fn unknown_readback_after_known_refusal_cannot_be_reconciled_as_a_lost_ack() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.reject_selection = true;
    effects.selection_refusal_current_unknown = true;
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert_eq!(
        status.pending, None,
        "no possible rename needs reconciliation"
    );
    assert_eq!(status.selected, None, "old observations are not current");
    assert!(status.reason.contains("current is unknown"));
    assert_eq!(status.failures, 1);
    effects.calls.clear();
    effects.unknown = false;
    effects.selected = None;
    effects.reject_selection = false;
    pass(&config, &mut status, &mut effects, |_| Ok(()))
        .expect("a fresh pass can select current source");
    assert!(effects.calls.contains(&"fetch"));
    assert!(effects.calls.contains(&"select"));
}

#[test]
fn a_known_cas_conflict_backs_off_then_retries_against_the_new_current() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.conflict = true;
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert_eq!(
        status.pending, None,
        "checked different current settles the prior attempt"
    );
    assert_eq!(
        status.selected.as_ref().expect("competing current").build,
        9
    );
    effects.conflict = false;
    effects.calls.clear();
    pass(&config, &mut status, &mut effects, |_| Ok(())).expect("retry with new CAS token");
    assert_eq!(effects.expected, Some(ExpectedCurrent::Build(9)));
    assert!(effects.calls.contains(&"fetch"));
}

#[test]
fn interrupted_build_is_preserved_without_a_second_fetch_or_compiler() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status {
        pending: Some(Operation::Build),
        ..Status::default()
    };
    let mut effects = Effects::new(&config);
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert_eq!(effects.calls, ["current"]);
    assert_eq!(status.pending, Some(Operation::Build));
    assert!(status.reason.contains("preserved interrupted"));
}

#[test]
fn a_failed_stage_keeps_compiled_and_selected_versions_distinct_and_backs_off() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.fail_stage = true;
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert!(status.compiled.is_some());
    assert!(status.selected.is_none());
    assert_eq!(status.failures, 1);
    assert_eq!(status.retry_seconds, MIN_POLL_SECONDS);
    assert!(!effects.calls.contains(&"select"));
}

#[test]
fn unknown_current_is_never_treated_as_absence() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.unknown = true;
    assert!(pass(&config, &mut status, &mut effects, |_| Ok(())).is_err());
    assert_eq!(effects.calls, ["current"]);
    assert!(effects.expected.is_none());
    assert!(status.reason.starts_with("current is unknown"));
}

#[test]
fn plan_scope_cannot_change_source_command_policy_or_profile() {
    let config = config(Path::new("/not-opened"));
    for field in ["source", "command", "policy", "profile", "compiler"] {
        let mut status = Status::default();
        let mut effects = Effects::new(&config);
        match field {
            "source" => effects.desired.source = "c".repeat(40),
            "command" => effects.desired.command = "claude-code-alab".into(),
            "policy" => effects.desired.policy = "repository".into(),
            "profile" => effects.desired.profile = "dev".into(),
            "compiler" => effects.desired.compiler = "short".into(),
            _ => unreachable!(),
        }
        assert!(
            pass(&config, &mut status, &mut effects, |_| Ok(())).is_err(),
            "{field}"
        );
        assert!(!effects.calls.contains(&"build"), "{field}");
    }
}

#[test]
fn status_persistence_failure_prevents_the_next_effect() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    assert!(
        pass(&config, &mut status, &mut effects, |_| Err(
            Error::cannot_run("disk full")
        ))
        .is_err()
    );
    assert_eq!(effects.calls, ["current"]);
}

#[test]
fn retries_are_bounded_and_success_resets_the_poll() {
    let config = config(Path::new("/not-opened"));
    assert_eq!(retry_delay(&config, 0).as_secs(), DEFAULT_POLL_SECONDS);
    assert_eq!(retry_delay(&config, 1).as_secs(), 60);
    assert_eq!(retry_delay(&config, 2).as_secs(), 120);
    assert_eq!(
        retry_delay(&config, u32::MAX).as_secs(),
        DEFAULT_POLL_SECONDS
    );
}

struct Clock {
    waits: Vec<Duration>,
    stop_after: usize,
}
impl WatchControl for Clock {
    fn stopping(&self) -> bool {
        false
    }
    fn wait(&mut self, delay: Duration) -> bool {
        self.waits.push(delay);
        self.waits.len() >= self.stop_after
    }
}

#[test]
fn watch_passes_immediately_and_continues_after_failure_with_bounded_waits() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    effects.fail_stage = true;
    let mut clock = Clock {
        waits: Vec::new(),
        stop_after: 2,
    };
    watch(&config, &mut status, &mut effects, &mut clock, |_| Ok(()))
        .expect("bounded stopped watch");
    assert_eq!(
        clock.waits,
        [Duration::from_secs(60), Duration::from_secs(120)]
    );
    assert_eq!(
        effects
            .calls
            .iter()
            .filter(|&&call| call == "build")
            .count(),
        2
    );
    assert!(!effects.calls.contains(&"select"));
}

#[test]
fn watch_does_not_retry_after_a_failed_journal_write() {
    let config = config(Path::new("/not-opened"));
    let mut status = Status::default();
    let mut effects = Effects::new(&config);
    let mut clock = Clock {
        waits: Vec::new(),
        stop_after: 1,
    };
    assert!(
        watch(&config, &mut status, &mut effects, &mut clock, |_| Err(
            Error::cannot_run("disk full")
        ))
        .is_err()
    );
    assert_eq!(effects.calls, ["current"]);
    assert!(clock.waits.is_empty());
}

#[test]
fn explicit_native_and_unverified_flags_are_required_without_duplicates() {
    let base = [
        "claude-code-alab",
        "--checkout",
        "/x",
        "--out",
        "/y",
        "--prefix",
        "/z",
        "--native",
        "--unverified",
    ];
    let args: Vec<String> = base.into_iter().map(String::from).collect();
    assert!(Config::parse(&args).is_ok());
    assert!(Config::parse(&args[..args.len() - 1]).is_err());
    let mut duplicate = args.clone();
    duplicate.push("--native".into());
    assert!(Config::parse(&duplicate).is_err());
    let mut unknown = args;
    unknown.extend(["--signed".into(), "yes".into()]);
    assert!(Config::parse(&unknown).is_err());
}

#[test]
fn producer_request_cannot_link_or_claim_proofs() {
    let config = config(Path::new("/not-opened"));
    let request = config.request();
    assert_eq!(request.policy, Policy::Unverified);
    assert!(!request.link);
    assert!(!request.plan);
}

#[test]
fn typed_config_rejects_unknown_fields_and_signed_policy() {
    let config = config(Path::new("/not-opened"));
    let text = aterm_json::to_string(&config).expect("config JSON");
    assert!(aterm_json::from_str::<Config>(&text.replace("\"unverified\"", "\"signed\"")).is_err());
    let unknown = text.replacen('{', "{\"approval\":true,", 1);
    assert!(aterm_json::from_str::<Config>(&unknown).is_err());
}

#[test]
fn private_lifetime_lock_and_stable_config_exclude_a_second_manager() {
    let (_tmp, home, config) = world();
    let session = fs::Session::take(&home, &config).expect("first exact lock");
    assert!(fs::Session::take(&home, &config).is_err());
    drop(session);
    let mut changed = config.clone();
    changed.jobs = 2;
    assert!(
        fs::Session::take(&home, &changed).is_err(),
        "stable config is never silently replaced"
    );
    assert!(fs::Session::take(&home, &config).is_ok());
}

#[test]
fn source_lock_excludes_the_same_checkout_under_a_different_prefix() {
    let (_tmp, home, config) = world();
    let session = fs::Session::take(&home, &config).expect("first source owner");
    let mut second = config.clone();
    second.prefix = home.join("another-prefix");
    second.out = home.join("another-output");
    assert!(fs::Session::take(&home, &second).is_err());
    drop(session);
    assert!(fs::Session::take(&home, &second).is_ok());
}

#[test]
fn overlapping_and_linked_source_paths_are_refused_without_mutation() {
    let (_tmp, home, mut config) = world();
    config.out = config.checkout.join("output");
    assert!(config.validate(&home).is_err());
    assert!(!config.out.exists());
    let source = config.checkout.clone();
    config.out = home.join("output");
    let link = home.join("source-link");
    std::os::unix::fs::symlink(&source, &link).expect("source link");
    config.checkout = link;
    assert!(config.validate(&home).is_err());
}

#[test]
fn replaced_exact_lock_prevents_status_writes() {
    let (_tmp, home, config) = world();
    let session = fs::Session::take(&home, &config).expect("manager");
    let root = config
        .prefix
        .join(".local-harness-manager")
        .join(&config.program);
    std::fs::rename(root.join("manager.lock"), root.join("preserved-lock"))
        .expect("preserve old inode");
    aterm_uds::private_file::write_private(&root, "manager.lock", b"").expect("different inode");
    assert!(session.save(&Status::default()).is_err());
    assert!(!root.join("status.json").exists());
}

#[test]
fn worker_lease_requires_both_exact_held_inodes_and_the_same_stable_configuration() {
    let (_tmp, home, config) = world();
    let session = fs::Session::take(&home, &config).expect("actual private lease");
    let proof = session.lease().expect("inode proof");
    fs::verify_lease(&home, &config, &proof).expect("both parent-held locks");
    let mut changed = config.clone();
    changed.jobs = 2;
    assert!(fs::verify_lease(&home, &changed, &proof).is_err());
    drop(session);
    assert!(
        fs::verify_lease(&home, &config, &proof).is_err(),
        "JSON and matching inodes cannot stand in for held locks"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fake {
    home: PathBuf,
    base: Option<bool>,
    develop: Vec<(&'static str, State)>,
    openssl: Option<String>,
    site: Option<PathBuf>,
    site_key: Option<PathBuf>,
    bootstrap_error: Option<Bootstrap>,
    source_signing_error: Option<SigningGap>,
    source_signing_calls: std::sync::atomic::AtomicUsize,
    unreached: Vec<&'static str>,
    needs: std::sync::Mutex<Vec<NeedStep>>,
}

impl ReleaseHost for Fake {
    fn base(&self) -> Result<Option<aterm_converge::Report>, String> {
        Ok(self.base.map(|complete| {
            aterm_converge::Report::new(aterm_converge::ReportMode::Check, complete, Vec::new())
        }))
    }
    fn develop(&self) -> Result<Vec<(&'static str, State)>, String> {
        Ok(self.develop.clone())
    }
    fn openssl(&self) -> Option<Result<String, String>> {
        self.openssl.clone().map(Ok)
    }
    fn publication(&self) -> PathBuf {
        self.home.join("publication")
    }
    /// The registry file [`registry`] wrote stands in for the engine's commit: reading a
    /// commit is `aterm_verify::engine`'s, tested there.
    fn engine_registry(&self) -> Result<(String, String), String> {
        let path = self.home.join("publication").join(REGISTRY);
        std::fs::read_to_string(&path)
            .map(|text| (path.display().to_string(), text))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
    fn publication_bootstrap(&self) -> Result<(), Bootstrap> {
        self.bootstrap_error.clone().map_or(Ok(()), Err)
    }
    fn source_signing(&self) -> Result<(), SigningGap> {
        self.source_signing_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.source_signing_error.clone().map_or(Ok(()), Err)
    }
    fn site(&self) -> Result<Option<PathBuf>, String> {
        Ok(self.site.clone())
    }
    fn site_key(&self) -> Result<Option<PathBuf>, String> {
        Ok(self.site_key.clone())
    }
    fn catalog_site_key(&self) -> Option<PathBuf> {
        Some(self.home.join("deployer.json"))
    }
    fn site_key_need(&self, step: NeedStep) -> Result<Option<String>, String> {
        let mut needs = self.needs.lock().unwrap();
        needs.push(step);
        let raised = needs.contains(&NeedStep::Raise);
        Ok(match step {
            NeedStep::Raise => Some("posted to the lab".into()),
            NeedStep::Read if raised => Some("posted to the lab".into()),
            NeedStep::Satisfied if raised => Some("fulfilled".into()),
            NeedStep::Read | NeedStep::Satisfied => None,
        })
    }
    fn reach(&self, url: &str) -> Result<(), String> {
        if self.unreached.contains(&url) {
            Err(format!("ssh: Could not resolve hostname for {url}"))
        } else {
            Ok(())
        }
    }
}

const DEV: &str = "git@gh-alias:owner/aterm.git";
const STAGING: &str = "git@gh-alias:owner/aterm-staging.git";
const RELEASE: &str = "git@github.com:public/aterm.git";

fn registry(home: &Path, text: &str) {
    let path = home.join("publication").join(REGISTRY);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn aterm_entry() -> String {
    format!(
        "[repos.other]\nrelease = \"git@github.com:public/other.git\"\n\n[repos.aterm]\n\
         path = \"~/aterm\"\ndev = \"{DEV}\"\nstaging = \"{STAGING}\"\nrelease = \"{RELEASE}\"\n"
    )
}

fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_deployer_key(home: &Path) {
    std::fs::write(
        home.join("deployer.json"),
        crate::site::hosting::key::tests::key_json("https://oauth2.googleapis.com/token"),
    )
    .unwrap();
}

fn whole(home: &Path) -> Fake {
    std::fs::create_dir_all(home.join("publication/.git")).unwrap();
    executable(&home.join("publication/bin/pub"));
    registry(home, &aterm_entry());
    std::fs::create_dir_all(home.join("site")).unwrap();
    std::fs::write(home.join("site/firebase.json"), "{}").unwrap();
    write_deployer_key(home);
    whole_keeping(home)
}

fn closing(home: &Path, checks: &[(&'static str, Check)]) -> crate::ledger::Result<()> {
    closing_as(home, checks, false, true)
}

fn closing_as(
    home: &Path,
    checks: &[(&'static str, Check)],
    check_only: bool,
    key_exists: bool,
) -> crate::ledger::Result<()> {
    let (fails, waiting) = super::super::tally(checks);
    super::super::close(super::super::Closing {
        id: "fake-host",
        home: home.to_str().unwrap(),
        check_only,
        report: None,
        profile: None,
        open: super::super::open_labels(checks),
        authority: None,
        roster_kept: None,
        key_exists,
        fails,
        waiting,
        host_cannot_cut: false,
    })
}

fn line_of(fake: Fake, id: &str) -> Check {
    let checks = audit(&(Arc::new(fake) as Arc<dyn ReleaseHost>));
    checks.into_iter().find(|(i, _)| *i == id).unwrap().1
}

#[test]
fn a_present_engine_without_its_runtime_is_not_ready_and_recovers() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut fake = whole(dir.path());
    fake.bootstrap_error = Some(Bootstrap::Failed("launcher runtime unavailable".into()));
    assert!(matches!(
        line_of(fake, "release.publication"),
        Check::CouldNotRun(why) if why.contains("launcher runtime unavailable")
    ));
    // A PATH interpreter does not satisfy signed package admission.
    let mut fake = whole(dir.path());
    fake.bootstrap_error = Some(Bootstrap::Prerequisites(
        "cpython has no signed package".into(),
    ));
    match line_of(fake, "release.publication") {
        Check::Fail { what, fix } => {
            assert!(what.contains("cpython has no signed package"), "{what}");
            assert_eq!(fix, crate::managed_prerequisites::REMEDY);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        line_of(whole_keeping(dir.path()), "release.publication"),
        Check::Pass(_)
    ));
}

#[test]
fn each_release_items_gap_fails_naming_what_is_missing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();

    let mut fake = whole(home);
    fake.bootstrap_error = Some(Bootstrap::Prerequisites(
        "gitleaks has no signed package".into(),
    ));
    match line_of(fake, "release.publication") {
        Check::Fail { what, .. } => {
            assert!(what.contains("gitleaks has no signed package"), "{what}")
        }
        other => panic!("{other:?}"),
    }

    let mut fake = whole(home);
    fake.site = None;
    match line_of(fake, "release.site") {
        Check::Fail { what, .. } => assert!(what.contains("no site checkout"), "{what}"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(home.join("site/firebase.json")).unwrap();
    match line_of(whole_keeping(home), "release.site") {
        Check::Fail { what, .. } => assert!(what.contains("has no firebase.json"), "{what}"),
        other => panic!("{other:?}"),
    }

    let mut fake = whole(home);
    fake.site_key = None;
    match line_of(fake, "release.site-key") {
        Check::Fail { what, fix } => {
            assert!(what.contains("no service-account key"), "{what}");
            assert!(fix.contains("firebase_sa_key"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(home.join("deployer.json")).unwrap();
    match line_of(whole_keeping(home), "release.site-key") {
        Check::Fail { what, .. } => assert!(what.contains("deployer.json"), "{what}"),
        other => panic!("{other:?}"),
    }
    let mut fake = whole(home);
    fake.site_key = Some(home.join("site"));
    match line_of(fake, "release.site-key") {
        Check::Fail { what, .. } => assert!(what.contains("site"), "{what}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        line_of(whole(home), "release.site-key"),
        Check::Pass(_)
    ));
}

fn whole_keeping(home: &Path) -> Fake {
    Fake {
        home: home.to_path_buf(),
        base: Some(true),
        develop: vec![("fish", State::Present(home.join("fish")))],
        openssl: Some("LibreSSL 3.3.6".into()),
        site: Some(home.join("site")),
        site_key: Some(home.join("deployer.json")),
        bootstrap_error: None,
        source_signing_error: None,
        source_signing_calls: std::sync::atomic::AtomicUsize::new(0),
        unreached: Vec::new(),
        needs: std::sync::Mutex::new(Vec::new()),
    }
}

#[test]
fn apple_tools_do_not_hide_an_unavailable_source_signer() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut fake = whole(dir.path());
    fake.source_signing_error = Some(SigningGap::Unusable(
        "source signing needs OpenSSL 3".into(),
    ));
    assert!(matches!(
        line_of(fake, "release.source-signing"),
        Check::CouldNotRun(_)
    ));
    // m58: the vault's roster only at the legacy path, which step 1 refuses: a gap with
    // the fix the refusal names, not a check that could not run.
    let mut fake = whole(dir.path());
    fake.source_signing_error = Some(SigningGap::Ring(
        "release roster ~/.aterm/release-ring/roster/aterm-machines.toml: missing".into(),
    ));
    match line_of(fake, "release.source-signing") {
        Check::Fail { what, fix } => {
            assert!(what.starts_with("source signing ring: "), "{what}");
            assert_eq!(
                fix,
                "targo --unverified ship provision --id <this machine's roster id>"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        line_of(whole_keeping(dir.path()), "release.source-signing"),
        Check::Pass(_)
    ));
}

#[test]
fn identity_dependent_signing_is_checked_after_provisioning_not_as_its_prerequisite() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut fake = whole(dir.path());
    fake.source_signing_error = Some(SigningGap::Ring("identity not created yet".into()));
    let mut fake = Arc::new(fake);
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert!(
        super::super::blockers(&audit(&host)) > 0,
        "the original order blocks creation"
    );
    let calls = fake.source_signing_calls.load(SeqCst);
    assert_eq!(super::super::blockers(&audit_before_identity(&host)), 0);
    assert_eq!(
        fake.source_signing_calls.load(SeqCst),
        calls,
        "no premature signing call"
    );
    assert!(
        super::super::blockers(&audit_source_signing(&host)) > 0,
        "readiness still fails before the identity exists"
    );
    drop(host);
    Arc::get_mut(&mut fake).unwrap().source_signing_error = None;
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert_eq!(super::super::blockers(&audit_source_signing(&host)), 0);
    drop(host);
    Arc::get_mut(&mut fake).unwrap().develop =
        vec![("openssl-signing", State::Missing("not installed".into()))];
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert!(
        super::super::blockers(&audit_before_identity(&host)) > 0,
        "signing tools are still prerequisites"
    );
}

#[test]
fn deployer_key_discovery_recognizes_an_equivalent_missing_destination() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join("site/inner")).unwrap();
    let fake = Arc::new(Fake {
        site_key: Some(home.join("site/inner/../../deployer.json")),
        ..whole_keeping(home)
    });
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert_eq!(settle_site_key(&host).as_deref(), Some("posted to the lab"));
    assert_eq!(*fake.needs.lock().unwrap(), [NeedStep::Raise]);
    assert!(!home.join("deployer.json").exists());
}

#[cfg(unix)]
#[test]
fn deployer_key_discovery_respects_symlinks_before_parent_components() {
    use std::os::unix::fs::symlink;
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join("elsewhere/deep")).unwrap();
    symlink(home.join("elsewhere/deep"), home.join("alias")).unwrap();
    let fake = Arc::new(Fake {
        site_key: Some(home.join("alias/../deployer.json")),
        ..whole_keeping(home)
    });
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert!(settle_site_key(&host).unwrap().contains("is not asked"));
    assert!(fake.needs.lock().unwrap().is_empty());
    assert!(!same_missing_destination(
        &home.join("absent/../deployer.json"),
        &home.join("deployer.json"),
    ));
    symlink(home.join("missing-key"), home.join("deployer.json")).unwrap();
    assert!(!same_missing_destination(
        &home.join("deployer.json"),
        &home.join("deployer.json"),
    ));
}

/// A key missing at the catalog path is asked of the lab only by a provision run, never by
/// an audit; a key named elsewhere is never asked for; a valid key meets the need.
#[test]
fn the_deployer_key_row_asks_the_lab_only_for_the_catalog_path_and_only_on_a_run() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let row = |host: &Arc<dyn ReleaseHost>| {
        audit(host)
            .into_iter()
            .find(|(id, _)| *id == "release.site-key")
            .map(|(_, check)| check)
            .unwrap()
    };
    let fake = Arc::new(whole_keeping(home));
    let host: Arc<dyn ReleaseHost> = fake.clone();
    assert!(matches!(row(&host), Check::Fail { what, .. } if !what.contains("the lab")));
    assert_eq!(
        *fake.needs.lock().unwrap(),
        [NeedStep::Read],
        "an audit only reads"
    );
    assert_eq!(settle_site_key(&host).as_deref(), Some("posted to the lab"));
    match row(&host) {
        Check::Fail { what, .. } => assert!(what.contains("the lab: posted"), "{what}"),
        other => panic!("{other:?}"),
    }
    write_deployer_key(home);
    assert_eq!(settle_site_key(&host).as_deref(), Some("fulfilled"));
    assert_eq!(
        *fake.needs.lock().unwrap(),
        [
            NeedStep::Read,
            NeedStep::Raise,
            NeedStep::Read,
            NeedStep::Satisfied
        ]
    );

    std::fs::remove_file(home.join("deployer.json")).unwrap();
    let elsewhere = Arc::new(Fake {
        site_key: Some(home.join("other.json")),
        ..whole_keeping(home)
    });
    let host: Arc<dyn ReleaseHost> = elsewhere.clone();
    let line = settle_site_key(&host).unwrap();
    assert!(
        line.contains("not at") && line.contains("is not asked"),
        "{line}"
    );
    assert!(
        elsewhere.needs.lock().unwrap().is_empty(),
        "a key named away from the catalog path is never raised"
    );
    let unnamed = Arc::new(Fake {
        site_key: None,
        ..whole_keeping(home)
    });
    let host: Arc<dyn ReleaseHost> = unnamed.clone();
    assert!(settle_site_key(&host).is_some(), "the row says why");
    assert!(
        unnamed.needs.lock().unwrap().is_empty(),
        "a deploy that names no key is never raised for: nothing would read the delivery"
    );
}

#[test]
fn malformed_deployer_keys_neither_pass_the_audit_nor_fulfill_the_need() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let fake = Arc::new(whole(home));
    let host: Arc<dyn ReleaseHost> = fake.clone();
    fake.needs.lock().unwrap().push(NeedStep::Raise);
    for bytes in [
        "{}".to_owned(),
        "not-json-private-marker".to_owned(),
        crate::site::hosting::key::tests::key_json("https://untrusted.invalid/token"),
        crate::site::hosting::key::tests::key_json("https://oauth2.googleapis.com/token").replace(
            concat!("-----BEGIN ", "PRIVATE KEY-----"),
            "invalid-private-marker",
        ),
    ] {
        std::fs::write(home.join("deployer.json"), bytes).unwrap();
        let checks = audit(&host);
        let check = &checks
            .iter()
            .find(|(id, _)| *id == "release.site-key")
            .unwrap()
            .1;
        assert!(matches!(check, Check::Fail { .. }), "{check:?}");
        let why = settle_site_key(&host).unwrap();
        assert!(why.contains("not marked fulfilled"), "{why}");
        assert!(!format!("{check:?}{why}").contains("private-marker"));
        assert!(!fake.needs.lock().unwrap().contains(&NeedStep::Satisfied));
    }
    write_deployer_key(home);
    assert!(matches!(
        audit(&host)
            .iter()
            .find(|(id, _)| *id == "release.site-key")
            .unwrap()
            .1,
        Check::Pass(_)
    ));
    assert_eq!(settle_site_key(&host).as_deref(), Some("fulfilled"));
    assert!(fake.needs.lock().unwrap().contains(&NeedStep::Satisfied));
}

/// THE PROBE'S REGISTRY IS THE ENGINE'S COMMIT, NEVER ITS WORKING TREE: an address a session
/// left uncommitted in the named engine's registry is not read, and the reading names the
/// commit. NEGATIVE CONTROL: the working tree holds that address.
#[test]
fn the_registry_is_read_on_the_engines_commit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    let repo = dir.path().join("repo");
    crate::ledger::testing::init(&repo, &[]);
    crate::ledger::testing::init(
        &engine,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
        ],
    );
    let registry = engine.join(REGISTRY);
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, aterm_entry()).unwrap();
    use crate::ledger::GitRunner as _;
    let git = crate::ledger::GitCli::new(&engine);
    git.git(&["add", "-A"]).unwrap();
    git.git(&["commit", "-q", "-m", "engine"]).unwrap();
    let uncommitted = aterm_entry().replace(STAGING, "git@elsewhere:x/aterm-staging.git");
    std::fs::write(&registry, &uncommitted).unwrap();
    let named = engine.clone().into_os_string();
    let var = move |name: &str| (name == "PUBLICATION_ENGINE").then(|| named.clone());
    let (shown, text) = registry_on_commit(&repo, &var).unwrap();
    assert_eq!(text, aterm_entry());
    assert!(shown.contains("at HEAD"), "{shown}");
    assert!(
        std::fs::read_to_string(&registry)
            .unwrap()
            .contains("git@elsewhere"),
        "NEGATIVE CONTROL: the working tree holds the uncommitted address"
    );
}

#[test]
fn the_engine_reach_names_each_registry_address_git_cannot_list() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();

    match line_of(whole(home), "release.engine-reach") {
        Check::Pass(now) => {
            for url in [DEV, STAGING, RELEASE] {
                assert!(now.contains(url), "{now}");
            }
            assert!(
                !now.contains("other.git"),
                "only aterm's entry is read: {now}"
            );
        }
        other => panic!("{other:?}"),
    }

    let mut fake = whole(home);
    fake.unreached = vec![STAGING, RELEASE];
    match line_of(fake, "release.engine-reach") {
        Check::Fail { what, fix } => {
            assert!(what.contains(&format!("staging {STAGING}")), "{what}");
            assert!(what.contains(&format!("release {RELEASE}")), "{what}");
            assert!(
                !what.contains(DEV),
                "a reached address is not named: {what}"
            );
            assert!(
                what.contains("Could not resolve hostname"),
                "git's words: {what}"
            );
            assert!(fix.contains("insteadOf"), "{fix}");
        }
        other => panic!("{other:?}"),
    }

    registry(home, "[repos.aterm]\ndev = \"git@x:a/aterm.git\"\n");
    assert!(
        matches!(
            line_of(whole_keeping(home), "release.engine-reach"),
            Check::CouldNotRun(ref why) if why.contains("names no staging address")
        ),
        "an entry missing an address is never a pass"
    );
    std::fs::remove_file(home.join("publication").join(REGISTRY)).unwrap();
    assert!(
        matches!(
            line_of(whole_keeping(home), "release.engine-reach"),
            Check::Fail { ref what, .. } if what.contains("cannot read the engine's registry")
        ),
        "no registry is never a pass"
    );
}

#[test]
fn an_unminted_check_with_a_gap_fails_naming_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let fake: Arc<dyn ReleaseHost> = Arc::new(whole(home));
    assert!(closing_as(home, &audit(&fake), true, false).is_ok());
    std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
    let said = closing_as(home, &audit(&fake), true, false)
        .expect_err("an open item fails the audit")
        .to_string();
    assert!(
        said.contains("unminted") && said.contains("release.publication (gap)"),
        "{said}"
    );
}

#[test]
fn a_missing_release_item_fails_the_run_naming_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let fake: Arc<dyn ReleaseHost> = Arc::new(whole(home));
    let checks = audit(&fake);
    assert!(
        checks.iter().all(|(_, c)| matches!(c, Check::Pass(_))),
        "{checks:#?}"
    );
    assert!(
        closing(home, &checks).is_ok(),
        "the whole layer closes READY TO CUT"
    );

    std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
    let checks = audit(&fake);
    let err = closing(home, &checks).expect_err("a missing release item fails the run");
    let said = err.to_string();
    assert!(said.contains("release.publication (gap)"), "{said}");
    assert!(
        !said.contains("release.site"),
        "only the missing item is named: {said}"
    );
    let (_, check) = checks
        .iter()
        .find(|(id, _)| *id == "release.publication")
        .unwrap();
    match check {
        Check::Fail { what, fix } => {
            assert!(what.contains("no publication engine at"), "{what}");
            assert!(fix.contains("PUBLICATION_ENGINE"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn base_and_develop_name_their_commands() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let mut fake = whole(home);
    fake.base = Some(false);
    fake.develop = vec![
        ("fish", State::Missing("fish 4.9.3: not in …".into())),
        ("pwsh", State::Present(home.join("pwsh"))),
    ];
    fake.openssl = None;
    let checks = audit(&(Arc::new(fake) as Arc<dyn ReleaseHost>));
    let find = |id: &str| &checks.iter().find(|(i, _)| *i == id).unwrap().1;
    match find("base") {
        Check::Fail { fix, .. } => assert_eq!(fix, "aterm init --only base"),
        other => panic!("{other:?}"),
    }
    match find("develop") {
        Check::Fail { what, fix } => {
            assert!(what.contains("missing: fish"), "{what}");
            assert_eq!(fix, "targo --unverified verify provision --release");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(find("release.openssl"), Check::Pass(p) if p.starts_with("not needed")));
    let said = closing(home, &checks).unwrap_err().to_string();
    assert!(said.contains("base (gap), develop (gap)"), "{said}");
}

#[test]
fn the_report_is_the_last_json_line_init_prints() {
    let report = aterm_converge::Report::new(aterm_converge::ReportMode::Check, true, Vec::new());
    let json = aterm_json::to_string(&report).unwrap();
    let earlier = aterm_converge::Report::new(aterm_converge::ReportMode::Check, false, Vec::new());
    assert_ne!(earlier, report);
    let earlier = aterm_json::to_string(&earlier).unwrap();
    assert_eq!(
        report_in(&format!("{earlier}\nnoise\n{{not json\n{json}\n")),
        Some(report)
    );
    assert_eq!(report_in("aterm init: unknown argument\n"), None);
}

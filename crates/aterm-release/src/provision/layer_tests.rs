// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fake {
    home: PathBuf,
    base: Option<bool>,
    develop: Vec<(&'static str, State)>,
    openssl: Option<String>,
    site: Option<PathBuf>,
    bootstrap_error: Option<Bootstrap>,
    source_signing_error: Option<SigningGap>,
    source_signing_calls: std::sync::atomic::AtomicUsize,
    unreached: Vec<&'static str>,
    needs: std::sync::Mutex<Vec<NeedStep>>,
    site_access_error: Option<String>,
    site_push_error: Option<String>,
    push_gaps: Vec<(&'static str, PushGap)>,
    asked: std::sync::Mutex<Vec<String>>,
    posted: std::sync::atomic::AtomicUsize,
    post_error: Option<String>,
    /// The line a run the lab's full table refused says.
    post_refused: Option<String>,
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
    /// The real lookup, over the store [`write_deployer_key`] seeds under the fake's home.
    fn site_key(&self) -> Result<aterm_uds::secret::Secret, String> {
        crate::site::deployer_key(Some(&self.home))
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
    fn site_access(&self, _key: &aterm_uds::secret::Secret) -> Result<String, String> {
        self.asked.lock().unwrap().push("access store".into());
        self.site_access_error.clone().map_or_else(
            || Ok("Hosting site s in project p reports firebasehosting.sites.update".into()),
            Err,
        )
    }
    fn site_push(&self, site: &Path) -> Result<(), String> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("push {}", site.display()));
        self.site_push_error.clone().map_or(Ok(()), Err)
    }
    fn can_push(&self, slug: &str) -> Result<(), PushGap> {
        self.asked.lock().unwrap().push(format!("can {slug}"));
        self.push_gaps
            .iter()
            .find(|(gap, _)| *gap == slug)
            .map_or(Ok(()), |(_, gap)| Err(gap.clone()))
    }
    fn post_site_key_need(&self) -> Result<Posted, String> {
        self.posted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(why) = &self.post_error {
            return Err(why.clone());
        }
        Ok(self
            .post_refused
            .clone()
            .map_or(Posted::Ran, Posted::Refused))
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

/// This Mac's store under `home` holds the deployer's key, a valid one.
fn write_deployer_key(home: &Path) {
    crate::site::tests::registry_under(
        home,
        Some(&crate::site::hosting::key::tests::key_json(
            "https://oauth2.googleapis.com/token",
        )),
    );
}

/// This Mac's store under `home` holds the deployer key's entry as `value`, or none.
fn deployer(home: &Path, value: Option<&str>) {
    crate::site::tests::registry_under(home, value);
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

fn closing(checks: &[(&'static str, Check)]) -> crate::ledger::Result<()> {
    closing_as(checks, false, true)
}

fn closing_as(
    checks: &[(&'static str, Check)],
    check_only: bool,
    key_exists: bool,
) -> crate::ledger::Result<()> {
    let (fails, waiting) = super::super::tally(checks);
    super::super::close(super::super::Closing {
        id: "fake-host",
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

    let fake = whole(home);
    deployer(home, None);
    match line_of(fake, "release.site-key") {
        Check::Fail { what, fix } => {
            assert!(what.contains("holds no site-deployer-key entry"), "{what}");
            assert_eq!(fix, "`aterm secrets need site-deployer-key`");
        }
        other => panic!("{other:?}"),
    }
    deployer(home, Some("not a service-account key"));
    match line_of(whole_keeping(home), "release.site-key") {
        // A held value no need replaces names no need.
        Check::Fail { what, fix } => {
            assert!(what.contains("is unusable"), "{what}");
            assert!(!fix.contains("aterm secrets need"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    write_deployer_key(home);
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
        bootstrap_error: None,
        source_signing_error: None,
        source_signing_calls: std::sync::atomic::AtomicUsize::new(0),
        unreached: Vec::new(),
        needs: std::sync::Mutex::new(Vec::new()),
        site_access_error: None,
        site_push_error: None,
        push_gaps: Vec::new(),
        asked: std::sync::Mutex::new(Vec::new()),
        posted: std::sync::atomic::AtomicUsize::new(0),
        post_error: None,
        post_refused: None,
    }
}

fn row<'r>(rows: &'r [(&'static str, Check)], id: &str) -> &'r Check {
    &rows.iter().find(|(row, _)| *row == id).unwrap().1
}

/// A DEPLOYER KEY THIS MAC'S REGISTRY DOES NOT HOLD IS FILED WITH THE LAB BY THE RELEASE'S
/// STEP 1, and its row names the filing: the 0.199.0 release met this gap at step 9. A dry
/// run raises and posts nothing and says a real run would.
/// NEGATIVE CONTROLS: a key that is there raises nothing and passes; a key the registry holds
/// that does not parse is not filed, since a delivery would not replace it.
#[test]
fn step_one_files_a_deployer_key_this_mac_lacks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let missing = || {
        let fake = whole(home);
        deployer(home, None);
        Arc::new(fake)
    };
    let posted = |fake: &Fake| fake.posted.load(std::sync::atomic::Ordering::SeqCst);

    let fake = missing();
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    match row(&rows, "release.site-key") {
        Check::Fail { what, fix } => {
            assert!(what.contains("holds no site-deployer-key entry"), "{what}");
            assert!(
                what.contains("filed with the lab (posted to the lab)"),
                "{what}"
            );
            assert!(
                fix.contains("answers the filed request") && fix.contains("aterm secrets need ls"),
                "{fix}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(fake.needs.lock().unwrap().contains(&NeedStep::Raise));
    assert_eq!(posted(&fake), 1);
    assert!(
        matches!(row(&rows, "release.site-access"), Check::Fail { what, .. } if what.contains("not asked of Google")),
        "Google is not asked about a key that is not there: {rows:#?}"
    );
    assert!(
        !fake
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|a| a.starts_with("access")),
        "{:?}",
        fake.asked.lock().unwrap()
    );

    // A pass that cannot be run now leaves the need on file and says so.
    let mut fake = whole(home);
    deployer(home, None);
    fake.post_error = Some("no installed aterm".into());
    let rows = distribution(&(Arc::new(fake) as Arc<dyn ReleaseHost>), false);
    assert!(
        matches!(row(&rows, "release.site-key"), Check::Fail { what, .. }
            if what.contains("filed with the lab")
                && what.contains("posted at this Mac's next pass (no installed aterm)")),
        "{rows:#?}"
    );

    let fake = missing();
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), true);
    match row(&rows, "release.site-key") {
        Check::Fail { what, fix } => {
            assert!(
                what.contains("a real run files it with the lab (no need is on file yet)"),
                "{what}"
            );
            assert_eq!(fix, "`aterm secrets need site-deployer-key`");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !fake.needs.lock().unwrap().contains(&NeedStep::Raise),
        "a dry run raises nothing"
    );
    assert_eq!(posted(&fake), 0, "a dry run posts nothing");

    let fake = Arc::new(whole(home));
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    assert!(
        rows.iter()
            .all(|(_, check)| matches!(check, Check::Pass(_))),
        "{rows:#?}"
    );
    assert!(!fake.needs.lock().unwrap().contains(&NeedStep::Raise));
    assert_eq!(posted(&fake), 0);

    let fake = Arc::new(whole(home));
    deployer(home, Some("{}"));
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    assert!(
        matches!(row(&rows, "release.site-key"), Check::Fail { what, .. }
            if what.contains("the need is not marked fulfilled")),
        "{rows:#?}"
    );
    assert!(!fake.needs.lock().unwrap().contains(&NeedStep::Raise));
    assert_eq!(posted(&fake), 0);
}

/// The line `aterm secrets need site-deployer-key` says where the lab's full table refuses it.
const REFUSAL: &str = "the lab's credential-requests table is full (8192 operations, all it \
                       holds): it takes no request, answer or sweep, and 1 need waits unposted \
                       in this Mac's ledger; site-deployer-key could not be posted, and stays \
                       in this Mac's ledger until the table takes it";

/// THE FULL TABLE'S EXIT IS A REFUSAL, NOT A FAILURE (ATERM-KI-1116-3474): a run of `aterm
/// secrets need` that exits its capacity refusal's status ([`aterm_secrets::converge::STANDING`])
/// is the refusal, in the run's own first line (or, where it said none, in words of its own); a
/// run that exits 0 ran; any other exit or a signal is a failure, said with it.
/// NEGATIVE CONTROL: with the refusal's exit read as any other, `posted` returns a failure.
#[test]
fn the_full_tables_exit_is_a_refusal_not_a_failure() {
    let stderr = format!("aterm secrets need: {REFUSAL}\nsite-deployer-key  local  …\n");
    let full = i32::from(aterm_secrets::converge::STANDING);
    assert_eq!(
        posted(Some(full), stderr.as_bytes()),
        Ok(Posted::Refused(REFUSAL.to_owned()))
    );
    assert!(
        matches!(posted(Some(full), b""), Ok(Posted::Refused(line))
            if line.contains("refused it for its capacity") && line.contains("unposted")),
        "a refusal that said nothing is still said as one"
    );
    assert_eq!(posted(Some(0), b""), Ok(Posted::Ran));
    assert_eq!(
        posted(Some(1), b"aterm secrets need: no vault\n"),
        Err("aterm secrets need exited Some(1)".to_owned())
    );
    assert_eq!(
        posted(None, b""),
        Err("aterm secrets need exited None".to_owned())
    );
}

/// A DEPLOYER KEY'S NEED THE LAB'S FULL TABLE REFUSED IS SAID SO (ATERM-KI-1116-3474): the need is
/// filed and stays in this Mac's ledger, unposted, and the row says the run's own line for it,
/// never that it posts at this Mac's next pass, and its fix that the request posts once the table
/// takes it.
/// NEGATIVE CONTROL: with the row saying a refusal as a failed run, it says the need posts at the
/// next pass.
#[test]
fn a_site_key_need_the_full_table_refused_is_said_so() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let mut fake = whole(home);
    deployer(home, None);
    fake.post_refused = Some(REFUSAL.to_owned());
    let fake = Arc::new(fake);
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    match row(&rows, "release.site-key") {
        Check::Fail { what, fix } => {
            assert!(what.contains("filed with the lab"), "{what}");
            assert!(what.contains(REFUSAL), "{what}");
            assert!(!what.contains("next pass"), "{what}");
            assert!(
                fix.contains("posts at a pass the lab's table takes it")
                    && fix.contains("aterm secrets need ls"),
                "{fix}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.posted.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// EACH WRITE THE LAST STEPS MAKE IS PROVED AT STEP 1 BY A READ OR A DRY RUN: Google's grant
/// to the deployer's key, the site checkout's push, and the routed identities' push to the
/// repositories a publish writes and to the tap. A denial is a gap that names the repository;
/// a question GitHub could not be asked is no verdict, never a pass; and a key Google
/// accepted marks its need met on a real run only.
/// NEGATIVE CONTROL: the whole host passes every row, asking exactly these.
#[test]
fn step_one_proves_each_distribution_write_by_a_read() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path();
    let fake = Arc::new(whole(home));
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    assert_eq!(
        rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [
            "release.site",
            "release.site-key",
            "release.site-access",
            "release.site-push",
            "release.writes",
            "release.tap"
        ]
    );
    assert!(
        rows.iter()
            .all(|(_, check)| matches!(check, Check::Pass(_))),
        "{rows:#?}"
    );
    let mut asked = fake.asked.lock().unwrap().clone();
    asked.sort();
    assert_eq!(
        asked,
        [
            "access store".to_owned(),
            "can alabsystems/homebrew-tap".into(),
            "can owner/aterm".into(),
            "can owner/aterm-staging".into(),
            "can public/aterm".into(),
            "push ".to_owned() + home.join("site").to_str().unwrap(),
        ]
    );
    assert!(
        fake.needs.lock().unwrap().contains(&NeedStep::Satisfied),
        "a key Google accepted meets its need"
    );
    let dry = Arc::new(whole(home));
    distribution(&(Arc::clone(&dry) as Arc<dyn ReleaseHost>), true);
    assert!(
        !dry.needs.lock().unwrap().contains(&NeedStep::Satisfied),
        "a dry run moves no need"
    );

    let mut fake = whole(home);
    fake.site_access_error = Some("403 PERMISSION_DENIED".into());
    fake.site_push_error = Some("remote: Permission to o/site.git denied".into());
    fake.push_gaps = vec![
        (
            "alabsystems/homebrew-tap",
            PushGap::Denied("as release, push is not granted".into()),
        ),
        (
            "owner/aterm-staging",
            PushGap::Unasked("no route to host".into()),
        ),
    ];
    let fake = Arc::new(fake);
    let rows = distribution(&(Arc::clone(&fake) as Arc<dyn ReleaseHost>), false);
    match row(&rows, "release.site-access") {
        Check::Fail { what, fix } => {
            assert!(what.contains("403 PERMISSION_DENIED"), "{what}");
            assert!(fix.contains("ship site --check-access"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    match row(&rows, "release.site-push") {
        Check::Fail { what, fix } => {
            assert!(what.contains("Permission to o/site.git denied"), "{what}");
            assert!(fix.contains("git push --dry-run origin HEAD:"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    match row(&rows, "release.tap") {
        Check::Fail { what, fix } => {
            assert!(
                what.contains("alabsystems/homebrew-tap (as release, push is not granted)"),
                "{what}"
            );
            assert!(fix.contains("agh can OWNER/REPO push"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(row(&rows, "release.writes"), Check::CouldNotRun(why)
            if why.contains("aterm's staging owner/aterm-staging (no route to host)")
                && !why.contains("owner/aterm ")),
        "an unanswered question is no verdict, and names only its repository: {rows:#?}"
    );
    assert!(
        !fake.needs.lock().unwrap().contains(&NeedStep::Satisfied),
        "a key Google refused meets no need"
    );

    let mut fake = whole(home);
    fake.push_gaps = vec![
        ("owner/aterm", PushGap::Denied("no writer is routed".into())),
        (
            "owner/aterm-staging",
            PushGap::Unasked("no route to host".into()),
        ),
    ];
    assert!(
        matches!(line_of_distribution(fake, "release.writes"), Check::Fail { what, .. }
            if what.contains("aterm's dev owner/aterm (no writer is routed)")),
        "a denial is a gap whatever else went unanswered"
    );

    assert_eq!(address_slug(DEV).as_deref(), Some("owner/aterm"));
    assert_eq!(
        address_slug("https://github.com/public/aterm.git/").as_deref(),
        Some("public/aterm")
    );
    assert_eq!(address_slug("aterm"), None);
}

fn line_of_distribution(fake: Fake, id: &str) -> Check {
    let rows = distribution(&(Arc::new(fake) as Arc<dyn ReleaseHost>), false);
    rows.into_iter().find(|(row, _)| *row == id).unwrap().1
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

/// A key this Mac's registry does not hold is filed only by a provision run, never by an audit;
/// a valid key meets the need.
#[test]
fn the_deployer_key_is_filed_only_on_a_run() {
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

    let held_none = Arc::new(whole_keeping(home));
    deployer(home, None);
    let host: Arc<dyn ReleaseHost> = held_none.clone();
    assert_eq!(settle_site_key(&host).as_deref(), Some("posted to the lab"));
    assert_eq!(*held_none.needs.lock().unwrap(), [NeedStep::Raise]);
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
        deployer(home, Some(&bytes));
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
    assert!(closing_as(&audit(&fake), true, false).is_ok());
    std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
    let said = closing_as(&audit(&fake), true, false)
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
        closing(&checks).is_ok(),
        "the whole layer closes READY TO CUT"
    );

    std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
    let checks = audit(&fake);
    let err = closing(&checks).expect_err("a missing release item fails the run");
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
    let said = closing(&checks).unwrap_err().to_string();
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! APPLE tier hook, identity, notarytool, self-check, and call-site tests
//! ([`sign::AppleTools`], [`publish::Packager`], [`publish::notarize_and_package`],
//! [`publish::selfcheck_signing`], [`publish::resume_apple_tier`]).

use crate::{dmg, ledger, publish, sign};

use publish::Journal;
use sign::{AppleSelfcheck, AppleTier, AppleTools, DevIdIdentity, GatekeeperKind, NotaryAuth};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The shape of an Apple Team ID, and unmistakably not one.
const FAKE_TEAM: &str = "TEAMIDXXXX";
const OTHER_TEAM: &str = "OTHERTEAM1";
const FAKE_SHA1: &str = "1111111111111111111111111111111111111111";
const OTHER_SHA1: &str = "2222222222222222222222222222222222222222";

fn placeholder_identity() -> DevIdIdentity {
    DevIdIdentity {
        sha1: FAKE_SHA1.to_string(),
        common_name: format!("Developer ID Application: Placeholder Org ({FAKE_TEAM})"),
    }
}

fn active_tier() -> AppleTier {
    AppleTier::Active {
        identity: placeholder_identity(),
        auth: NotaryAuth::KeychainProfile("placeholder-profile".into()),
    }
}

/// Ordered transcript shared by both seams [`publish::notarize_and_package`] drives.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

fn record(log: &Log, what: &str, target: &Path) {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    log.borrow_mut().push(format!("{what}:{name}"));
}

/// Records the Apple operations in order, and can be told to fail any one or report any verdict.
struct FakeTools {
    log: Log,
    fail_on: Option<&'static str>,
    /// Narrows [`FakeTools::fail_on`] to one artifact.
    fail_on_target: Option<&'static str>,
    /// `codesign --verify --deep --strict` verdict.
    strict_ok: bool,
    /// What `codesign -dv` reports for the `.app`.
    dv: String,
    /// File name (e.g. `"aterm.dmg"`) whose ticket is missing; `None` = both
    /// artifacts are stapled.
    unstapled: Option<&'static str>,
    /// File name Gatekeeper rejects.
    gatekeeper_rejects: Option<&'static str>,
}

impl FakeTools {
    /// A machine where everything Apple-side is healthy.
    fn healthy(log: &Log) -> Self {
        Self {
            log: Rc::clone(log),
            fail_on: None,
            fail_on_target: None,
            strict_ok: true,
            dv: good_dv().to_string(),
            unstapled: None,
            gatekeeper_rejects: None,
        }
    }
    fn failing(log: &Log, what: &'static str) -> Self {
        Self {
            fail_on: Some(what),
            ..Self::healthy(log)
        }
    }
    fn record(&self, what: &str, target: &Path) -> Result<(), String> {
        record(&self.log, what, target);
        let this_target = self.fail_on_target.is_none() || Self::is(target, self.fail_on_target);
        if self.fail_on == Some(what) && this_target {
            return Err(format!("{what} refused by the fake"));
        }
        Ok(())
    }
    fn is(target: &Path, name: Option<&str>) -> bool {
        name.is_some_and(|n| target.file_name().is_some_and(|f| f == n))
    }
}

impl AppleTools for FakeTools {
    fn sign_dmg(&self, dmg: &Path, id: &DevIdIdentity) -> Result<(), String> {
        assert_eq!(
            id.sha1, FAKE_SHA1,
            "sign_dmg must receive the resolved cert"
        );
        self.record("sign_dmg", dmg)
    }
    fn check_devid_signed(&self, target: &Path) -> Result<(), String> {
        self.record("preflight", target)
    }
    fn notarize(&self, artifact: &Path, auth: &NotaryAuth) -> Result<(), String> {
        assert!(
            !auth.args().is_empty(),
            "notarize must receive a usable credential"
        );
        self.record("notarize", artifact)
    }
    fn codesign_verify_strict(&self, target: &Path) -> Result<(), String> {
        self.record("verify_strict", target)?;
        if self.strict_ok {
            Ok(())
        } else {
            Err("fake codesign: a sealed resource is missing or invalid".into())
        }
    }
    fn codesign_dv(&self, target: &Path) -> Result<String, String> {
        self.record("dv", target)?;
        Ok(self.dv.clone())
    }
    fn stapler_validate(&self, target: &Path) -> Result<bool, String> {
        self.record("stapled?", target)?;
        Ok(!Self::is(target, self.unstapled))
    }
    fn gatekeeper_ok(&self, target: &Path, kind: GatekeeperKind) -> Result<bool, String> {
        let expected = if target.extension().is_some_and(|e| e == "dmg") {
            GatekeeperKind::Dmg
        } else {
            GatekeeperKind::App
        };
        assert_eq!(
            kind,
            expected,
            "{} must be assessed as {expected:?}",
            target.display()
        );
        self.record("gatekeeper?", target)?;
        Ok(!Self::is(target, self.gatekeeper_rejects))
    }
}

/// Records packaging.
struct FakePackager {
    log: Log,
}

/// The digest `dmg::create` mints, before the Dev-ID signature and the staple rewrite the file.
const DMG_SHA_BEFORE: &str = "pre-hook-digest";
/// The digest a re-read of the same path yields afterwards.
const DMG_SHA_AFTER: &str = "post-hook-digest";
const ZIP_SHA: &str = "zip-digest";

impl publish::Packager for FakePackager {
    fn dmg(&self, app: &Path, _dist: &Path, _version: &str) -> ledger::Result<dmg::Packaged> {
        record(&self.log, "create_dmg", app);
        Ok(dmg::Packaged {
            path: dmg(),
            sha256: DMG_SHA_BEFORE.into(),
            size_bytes: 1_000,
        })
    }
    fn zip(&self, app: &Path, _dist: &Path, _version: &str) -> ledger::Result<dmg::Packaged> {
        record(&self.log, "create_zip", app);
        Ok(dmg::Packaged {
            path: PathBuf::from("/tmp/aterm-tier-fixture/aterm-mac.zip"),
            sha256: ZIP_SHA.into(),
            size_bytes: 900,
        })
    }
    fn sha256(&self, path: &Path) -> ledger::Result<String> {
        record(&self.log, "rehash", path);
        Ok(DMG_SHA_AFTER.into())
    }
    fn size(&self, path: &Path) -> ledger::Result<u64> {
        record(&self.log, "resize", path);
        Ok(2_000)
    }
}

fn app() -> PathBuf {
    PathBuf::from("/tmp/aterm-tier-fixture/aterm.app")
}
fn dmg() -> PathBuf {
    PathBuf::from("/tmp/aterm-tier-fixture/aterm.dmg")
}
fn dist() -> PathBuf {
    PathBuf::from("/tmp/aterm-tier-fixture")
}

/// A `codesign -dv` report for a properly Dev-ID-signed.
fn good_dv() -> &'static str {
    concat!(
        "Identifier=com.aterm.aterm\n",
        "CodeDirectory v=20500 flags=0x10000(runtime)\n",
        "Authority=Developer ID Application: Placeholder Org (TEAMIDXXXX)\n",
        "TeamIdentifier=TEAMIDXXXX\n",
    )
}

#[test]
fn the_shipped_anchor_is_armed_and_an_empty_anchor_still_resolves_inert() {
    assert!(aterm_update_core::pins::anchor_active(
        aterm_update_core::pins::APPLE_TEAM_ID
    ));
    assert!(
        !aterm_update_core::pins::anchor_active(""),
        "an empty anchor must never read as active"
    );
    for done in [&[][..], &["sign"][..]] {
        let tier = publish::resume_apple_tier("", &journal_with(done), None)
            .expect("an unpinned resume resolves without credentials");
        assert!(tier.identity().is_none(), "done = {done:?}");
    }
}

#[test]
fn an_empty_anchor_resolves_to_the_inactive_tier_without_touching_a_keychain() {
    let tier = sign::resolve_apple_tier("", None).expect("an unpinned build must resolve");
    assert!(
        tier.identity().is_none(),
        "the inactive tier has no signing identity"
    );
}

#[test]
fn the_inactive_hooks_do_nothing_and_claim_nothing() {
    let log = log();
    let tools = FakeTools::healthy(&log);
    let notarized_app = sign::notarize_app(&app(), &AppleTier::Inactive, &tools)
        .expect("the inactive app hook is a no-op, not a failure");
    let notarized_dmg = sign::sign_and_notarize_dmg(&dmg(), &AppleTier::Inactive, &tools)
        .expect("the inactive dmg hook is a no-op, not a failure");
    assert!(
        !notarized_app,
        "nothing was notarized, so nothing is claimed"
    );
    assert!(
        !notarized_dmg,
        "nothing was notarized, so nothing is claimed"
    );
    assert!(
        entries(&log).is_empty(),
        "the inactive tier must not spawn a single Apple tool, got {:?}",
        entries(&log)
    );
}

#[test]
fn the_active_app_hook_preflights_then_notarizes_the_bundle() {
    let log = log();
    let did = sign::notarize_app(&app(), &active_tier(), &FakeTools::healthy(&log))
        .expect("active app hook");
    assert!(did, "the active hook reports that it acted");
    assert_eq!(
        entries(&log),
        vec!["preflight:aterm.app", "notarize:aterm.app"],
        "the bundle must be verified and THEN notarized, in that order"
    );
}

#[test]
fn the_active_dmg_hook_signs_verifies_and_notarizes() {
    let log = log();
    let did = sign::sign_and_notarize_dmg(&dmg(), &active_tier(), &FakeTools::healthy(&log))
        .expect("dmg hook");
    assert!(did, "the active hook reports that it acted");
    assert_eq!(
        entries(&log),
        vec![
            "sign_dmg:aterm.dmg",
            "preflight:aterm.dmg",
            "notarize:aterm.dmg"
        ],
        "sign, then verify the signature, then notarize"
    );
}

#[test]
fn a_failed_notarization_fails_the_release() {
    let dmg_log = log();
    let err = sign::sign_and_notarize_dmg(
        &dmg(),
        &active_tier(),
        &FakeTools::failing(&dmg_log, "notarize"),
    )
    .expect_err("a failed notarization must not return Ok");
    assert!(err.contains("notarize"), "{err}");
    let app_log = log();
    sign::notarize_app(
        &app(),
        &active_tier(),
        &FakeTools::failing(&app_log, "notarize"),
    )
    .expect_err("a failed bundle notarization must not return Ok");
}

#[test]
fn a_failed_verification_fails_the_release_before_apple_is_asked() {
    let log = log();
    let err = sign::notarize_app(
        &app(),
        &active_tier(),
        &FakeTools::failing(&log, "preflight"),
    )
    .expect_err("a failed preflight must not return Ok");
    assert!(err.contains("preflight"), "{err}");
    assert!(
        !entries(&log).iter().any(|c| c.starts_with("notarize:")),
        "a failed preflight must abort BEFORE submission, got {:?}",
        entries(&log)
    );
}

#[test]
fn a_failed_dmg_signature_fails_the_release() {
    let log = log();
    sign::sign_and_notarize_dmg(
        &dmg(),
        &active_tier(),
        &FakeTools::failing(&log, "sign_dmg"),
    )
    .expect_err("a failed codesign must not return Ok");
    assert_eq!(
        entries(&log),
        vec!["sign_dmg:aterm.dmg"],
        "nothing may proceed past a failed signature"
    );
}

#[test]
fn the_keychain_profile_spelling_puts_no_secret_on_the_command_line() {
    let auth = NotaryAuth::KeychainProfile("placeholder-profile".into());
    assert_eq!(
        auth.args(),
        vec!["--keychain-profile", "placeholder-profile"],
        "notarytool takes the profile NAME, and nothing else"
    );
    assert_eq!(auth.args().len(), 2, "no third argument can carry a secret");
}

#[test]
fn the_apple_id_spelling_produces_the_full_notarytool_triple() {
    let auth = NotaryAuth::AppleId {
        apple_id: "placeholder@example.invalid".into(),
        team_id: String::new(),
        password: "placeholder-app-specific-password".into(),
    }
    .with_team_id(FAKE_TEAM);
    assert_eq!(
        auth.args(),
        vec![
            "--apple-id",
            "placeholder@example.invalid",
            "--team-id",
            FAKE_TEAM,
            "--password",
            "placeholder-app-specific-password",
        ],
        "notarytool needs all three, in this order"
    );
}

#[test]
fn the_team_id_notarytool_sees_comes_from_the_anchor_not_the_profile() {
    let profile = "signing_key = \"AA==\"\n\
                   notary_apple_id = \"placeholder@example.invalid\"\n\
                   notary_password = \"placeholder-app-specific-password\"\n\
                   team_id = \"OTHERTEAM1\"\n";
    let auth = sign::credentials_notary_auth(profile)
        .expect("the stray team_id line is inert, not a parse error")
        .expect("the Apple stanza is present");
    let args = auth.clone().with_team_id(FAKE_TEAM).args();
    assert!(
        args.contains(&FAKE_TEAM.to_string()),
        "the anchor's team must be the one submitted: {args:?}"
    );
    assert!(
        !args.contains(&OTHER_TEAM.to_string()),
        "a team id in the profile must never reach notarytool: {args:?}"
    );
}

#[test]
fn a_password_never_reaches_a_debug_line() {
    let auth = NotaryAuth::AppleId {
        apple_id: "placeholder@example.invalid".into(),
        team_id: FAKE_TEAM.into(),
        password: "placeholder-app-specific-password".into(),
    };
    let rendered = format!("{auth:?}");
    assert!(
        !rendered.contains("placeholder-app-specific-password"),
        "the password must never render: {rendered}"
    );
}

#[test]
fn a_cut_without_a_notary_credential_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("aterm-apple-refusal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(home.join(".aterm")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let pkcs8 =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let key = home.join(".aterm/machine.key");
    std::fs::write(&key, pkcs8.as_ref()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

    let machine = sign::ReleaseCredentials::resolve_with(None, &repo, home.to_str())
        .expect("machine.key resolves")
        .expect("machine.key yields credentials");
    assert!(machine.profile().is_none(), "machine.key is no profile");
    for credentials in [Some(&machine), None] {
        sign::resolve_apple_tier(FAKE_TEAM, credentials)
            .expect_err("no profile, no notary credential");
    }

    let encoded = aterm_codec::base64::encode(pkcs8.as_ref()).expect("pkcs8");
    let path = root.join("profile.toml");
    std::fs::write(&path, format!("signing_key = \"{encoded}\"\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let profile = sign::ReleaseCredentials::load(&path).expect("the profile loads");
    assert_eq!(profile.profile(), Some(path.as_path()));
    sign::resolve_apple_tier(FAKE_TEAM, Some(&profile))
        .expect_err("a profile without notary_profile cannot notarize");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_profile_with_no_apple_stanza_yields_no_auth() {
    assert!(
        sign::credentials_notary_auth("signing_key = \"AA==\"\n")
            .expect("no Apple stanza is legal")
            .is_none()
    );
}

#[test]
fn a_profile_naming_both_spellings_is_refused() {
    sign::credentials_notary_auth(
        "notary_profile = \"placeholder-profile\"\n\
         notary_apple_id = \"placeholder@example.invalid\"\n\
         notary_password = \"placeholder-app-specific-password\"\n",
    )
    .expect_err("two credentials means the file cannot say which one signed");
}

#[test]
fn a_half_written_headless_stanza_is_refused() {
    for body in [
        "notary_apple_id = \"placeholder@example.invalid\"\n",
        "notary_password = \"placeholder-app-specific-password\"\n",
    ] {
        sign::credentials_notary_auth(body).expect_err("half a credential is not one");
    }
}

#[test]
fn an_empty_notary_profile_is_refused_rather_than_read_as_absent() {
    sign::credentials_notary_auth("notary_profile = \"\"\n")
        .expect_err("an empty value is a mistake, not an absence");
}

/// Real `security find-identity -v -p codesigning` output shape.
fn listing(entries: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (sha1, cn)) in entries.iter().enumerate() {
        out.push_str(&format!("  {}) {sha1} \"{cn}\"\n", i + 1));
    }
    out.push_str(&format!("     {} valid identities found\n", entries.len()));
    out
}

#[test]
fn the_certificate_is_derived_from_the_anchor_with_nothing_else_supplied() {
    let text = listing(&[(
        FAKE_SHA1,
        &format!("Developer ID Application: Placeholder Org ({FAKE_TEAM})"),
    )]);
    let id = sign::select_devid_identity(&text, FAKE_TEAM, None).expect("one match resolves");
    assert_eq!(id.sha1, FAKE_SHA1);
    assert!(id.provenance().contains(FAKE_TEAM), "{}", id.provenance());
}

#[test]
fn an_installer_certificate_for_the_same_team_is_never_chosen() {
    let text = listing(&[
        (
            OTHER_SHA1,
            &format!("Developer ID Installer: Placeholder Org ({FAKE_TEAM})"),
        ),
        (
            FAKE_SHA1,
            &format!("Developer ID Application: Placeholder Org ({FAKE_TEAM})"),
        ),
    ]);
    let id = sign::select_devid_identity(&text, FAKE_TEAM, None).expect("the Application cert");
    assert_eq!(
        id.sha1, FAKE_SHA1,
        "only a \"Developer ID Application\" certificate may be chosen"
    );
}

#[test]
fn a_certificate_for_another_team_is_never_chosen() {
    let text = listing(&[(
        OTHER_SHA1,
        &format!("Developer ID Application: Placeholder Org ({OTHER_TEAM})"),
    )]);
    let err = sign::select_devid_identity(&text, FAKE_TEAM, None)
        .expect_err("another team's certificate must not resolve");
    assert!(
        err.contains(FAKE_TEAM),
        "the refusal names the anchor: {err}"
    );
}

#[test]
fn an_empty_keychain_fails_closed() {
    sign::select_devid_identity("     0 valid identities found\n", FAKE_TEAM, None)
        .expect_err("no certificate must be a hard error");
}

#[test]
fn two_matching_certificates_are_refused_rather_than_guessed_between() {
    let cn = format!("Developer ID Application: Placeholder Org ({FAKE_TEAM})");
    let text = listing(&[(FAKE_SHA1, &cn), (OTHER_SHA1, &cn)]);
    let err = sign::select_devid_identity(&text, FAKE_TEAM, None)
        .expect_err("an ambiguous keychain must not be resolved by guessing");
    assert!(err.contains(FAKE_SHA1) && err.contains(OTHER_SHA1), "{err}");
    let id = sign::select_devid_identity(&text, FAKE_TEAM, Some(&OTHER_SHA1.to_lowercase()))
        .expect("a named hash resolves the ambiguity");
    assert_eq!(id.sha1, OTHER_SHA1);
}

#[test]
fn the_disambiguator_can_only_narrow_never_widen() {
    let text = listing(&[(
        OTHER_SHA1,
        &format!("Developer ID Application: Placeholder Org ({OTHER_TEAM})"),
    )]);
    sign::select_devid_identity(&text, FAKE_TEAM, Some(OTHER_SHA1))
        .expect_err("naming a hash must not smuggle in another team's certificate");
}

#[test]
fn an_empty_anchor_can_never_select_a_certificate() {
    let text = listing(&[(FAKE_SHA1, "Developer ID Application: Placeholder Org ()")]);
    sign::select_devid_identity(&text, "", None).expect_err("an empty anchor selects nothing");
}

fn good_evidence() -> AppleSelfcheck<'static> {
    AppleSelfcheck {
        team_id: FAKE_TEAM,
        app_codesign_dv: good_dv(),
        app_stapled: true,
        app_gatekeeper_ok: true,
        dmg_stapled: true,
        dmg_gatekeeper_ok: true,
    }
}

#[test]
fn a_fully_notarized_stapled_cut_passes_the_self_check() {
    sign::apple_selfcheck_verdict(&good_evidence()).expect("a real Tier APPLE cut must pass");
}

#[test]
fn a_bundle_without_a_stapled_ticket_fails_the_self_check() {
    let mut e = good_evidence();
    e.app_stapled = false;
    assert!(
        e.app_gatekeeper_ok,
        "precondition: Gatekeeper is happy, which is exactly why the staple must be \
         checked separately"
    );
    sign::apple_selfcheck_verdict(&e).expect_err("an unstapled bundle must fail");
}

#[test]
fn a_dmg_without_a_stapled_ticket_fails_the_self_check() {
    let mut e = good_evidence();
    e.dmg_stapled = false;
    assert!(
        e.dmg_gatekeeper_ok,
        "precondition: only the ticket is missing"
    );
    sign::apple_selfcheck_verdict(&e).expect_err("an unstapled DMG must fail");
}

#[test]
fn a_gatekeeper_rejection_fails_the_self_check_for_either_artifact() {
    let mut app_rejected = good_evidence();
    app_rejected.app_gatekeeper_ok = false;
    let err = sign::apple_selfcheck_verdict(&app_rejected)
        .expect_err("Gatekeeper said no about the app and the cut continued");
    assert!(err.contains("app"), "{err}");

    let mut dmg_rejected = good_evidence();
    dmg_rejected.dmg_gatekeeper_ok = false;
    let err = sign::apple_selfcheck_verdict(&dmg_rejected)
        .expect_err("Gatekeeper said no about the DMG and the cut continued");
    assert!(err.contains("DMG"), "{err}");
}

#[test]
fn a_team_mismatch_fails_the_self_check() {
    let mut e = good_evidence();
    e.team_id = OTHER_TEAM;
    let err = sign::apple_selfcheck_verdict(&e).expect_err("a team mismatch must fail");
    assert!(err.contains("TeamIdentifier"), "{err}");
}

#[test]
fn the_active_build_notarizes_the_bundle_before_the_zip_that_carries_it() {
    let log = log();
    let out = publish::notarize_and_package(
        &app(),
        &dist(),
        "9.9.9",
        &active_tier(),
        &FakeTools::healthy(&log),
        &FakePackager {
            log: Rc::clone(&log),
        },
    )
    .expect("the active packaging path");
    assert_eq!(
        entries(&log),
        vec![
            "preflight:aterm.app",
            "notarize:aterm.app",
            "create_dmg:aterm.app",
            "sign_dmg:aterm.dmg",
            "preflight:aterm.dmg",
            "notarize:aterm.dmg",
            "rehash:aterm.dmg",
            "resize:aterm.dmg",
            "create_zip:aterm.app",
        ],
        "the bundle must be stapled before any container is built"
    );
    assert_eq!(out.dmg_sha256, DMG_SHA_AFTER);
    assert_eq!(out.dmg_size, 2_000, "the size must be re-read too");
    assert_eq!(out.zip.sha256, ZIP_SHA);
}

#[test]
fn the_inactive_build_packages_exactly_as_it_always_did() {
    let log = log();
    let out = publish::notarize_and_package(
        &app(),
        &dist(),
        "9.9.9",
        &AppleTier::Inactive,
        &FakeTools::healthy(&log),
        &FakePackager {
            log: Rc::clone(&log),
        },
    )
    .expect("the inactive packaging path");
    assert_eq!(
        entries(&log),
        vec!["create_dmg:aterm.app", "create_zip:aterm.app"],
        "the inactive tier packages and does nothing else"
    );
    assert_eq!(
        out.dmg_sha256, DMG_SHA_BEFORE,
        "nothing rewrote the DMG, so nothing may re-hash it"
    );
    assert_eq!(out.dmg_size, 1_000);
}

#[test]
fn a_failed_bundle_notarization_stops_the_cut_before_anything_is_packaged() {
    let log = log();
    let outcome = publish::notarize_and_package(
        &app(),
        &dist(),
        "9.9.9",
        &active_tier(),
        &FakeTools::failing(&log, "notarize"),
        &FakePackager {
            log: Rc::clone(&log),
        },
    );
    assert!(
        outcome.is_err(),
        "a failed submission must abort the cut, not return artifacts"
    );
    assert!(
        !entries(&log).iter().any(|c| c.starts_with("create_")),
        "nothing may be packaged after a failed notarization, got {:?}",
        entries(&log)
    );
}

#[test]
fn a_failed_dmg_notarization_stops_the_cut_before_the_zip_is_built() {
    let log = log();
    let tools = FakeTools {
        fail_on_target: Some("aterm.dmg"),
        ..FakeTools::failing(&log, "notarize")
    };
    let outcome = publish::notarize_and_package(
        &app(),
        &dist(),
        "9.9.9",
        &active_tier(),
        &tools,
        &FakePackager {
            log: Rc::clone(&log),
        },
    );
    assert!(
        outcome.is_err(),
        "a failed DMG submission must abort the cut"
    );
    let seen = entries(&log);
    assert!(
        seen.contains(&"notarize:aterm.app".to_string()),
        "precondition: the bundle's own submission succeeded, {seen:?}"
    );
    assert!(
        !seen.iter().any(|c| c.starts_with("create_zip")),
        "the zip must never be built, got {seen:?}"
    );
}

#[test]
fn the_self_check_gate_judges_every_cut_that_claims_a_team() {
    let log = log();
    let tools = FakeTools {
        unstapled: Some("aterm.app"),
        ..FakeTools::healthy(&log)
    };
    publish::selfcheck_signing(FAKE_TEAM, &app(), &dmg(), &tools)
        .expect_err("an unstapled bundle must fail the self-check");
    assert!(
        entries(&log).contains(&"stapled?:aterm.app".to_string()),
        "{:?}",
        entries(&log)
    );
}

#[test]
fn the_self_check_gate_passes_a_genuinely_notarized_cut() {
    let log = log();
    publish::selfcheck_signing(FAKE_TEAM, &app(), &dmg(), &FakeTools::healthy(&log))
        .expect("a fully notarized, stapled cut passes");
    assert_eq!(
        entries(&log),
        vec![
            "verify_strict:aterm.app",
            "dv:aterm.app",
            "stapled?:aterm.app",
            "gatekeeper?:aterm.app",
            "stapled?:aterm.dmg",
            "gatekeeper?:aterm.dmg",
        ],
        "both artifacts are assessed, and the hard codesign gate runs first"
    );
}

#[test]
fn the_self_check_asks_nothing_of_apple_for_a_cut_that_claims_no_team() {
    let log = log();
    publish::selfcheck_signing("", &app(), &dmg(), &FakeTools::healthy(&log))
        .expect("a cut that claims no team passes without asking Apple anything");
    assert_eq!(
        entries(&log),
        vec!["verify_strict:aterm.app"],
        "the ad-hoc cut pays for the codesign gate and not one Apple query more"
    );
}

#[test]
fn the_hard_codesign_gate_runs_on_the_shipped_tier_too() {
    let log = log();
    let tools = FakeTools {
        strict_ok: false,
        ..FakeTools::healthy(&log)
    };
    let err = publish::selfcheck_signing("", &app(), &dmg(), &tools)
        .expect_err("a failed codesign --verify must abort the cut");
    assert!(err.to_string().contains("--deep --strict"), "{err}");
}

#[test]
fn a_gatekeeper_rejection_of_the_dmg_stops_the_cut() {
    let log = log();
    let tools = FakeTools {
        gatekeeper_rejects: Some("aterm.dmg"),
        ..FakeTools::healthy(&log)
    };
    let err = publish::selfcheck_signing(FAKE_TEAM, &app(), &dmg(), &tools)
        .expect_err("a rejected DMG must fail the self-check");
    assert!(err.to_string().contains("DMG"), "{err}");
}

#[test]
fn each_artifact_gets_the_gatekeeper_assessment_it_actually_faces() {
    assert_eq!(
        sign::spctl_argv(GatekeeperKind::App),
        vec!["/usr/sbin/spctl", "-a", "-t", "exec"]
    );
    assert_eq!(
        sign::spctl_argv(GatekeeperKind::Dmg),
        vec![
            "/usr/sbin/spctl",
            "-a",
            "-t",
            "open",
            "--context",
            "context:primary-signature"
        ]
    );
}

fn journal_with(done: &[&str]) -> Journal {
    Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "9.9.9".into(),
        build_number: 1_783_918_101,
        commit: "aed5a06caed5a06caed5a06caed5a06caed5a06c".into(),
        min_build: None,
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        done: done.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[test]
fn a_resume_past_the_build_never_asks_for_an_apple_credential() {
    let tier = publish::resume_apple_tier(FAKE_TEAM, &journal_with(&["sign"]), None)
        .expect("a resume past the build must not need Apple credentials");
    assert!(
        tier.identity().is_none(),
        "nothing remaining signs, so no identity is held"
    );
}

#[test]
fn a_resume_that_will_rebuild_must_still_prove_it_can_notarize() {
    publish::resume_apple_tier(FAKE_TEAM, &journal_with(&["gates"]), None)
        .expect_err("an active anchor with no credentials must refuse to rebuild");
}

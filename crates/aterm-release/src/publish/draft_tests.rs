// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::channel::ChannelRelease;

pub(crate) struct Lab {
    _dir: aterm_tempfile::TempDir,
    pub(crate) fake: agh_fake::FakeGithub,
    pub(crate) ctx: CutCtx,
    route: crate::github::Fixture,
    _route: crate::github::FixtureGuard,
}

impl Lab {
    fn new() -> Self {
        Self::for_cut("0.191.0", 191, &"a".repeat(40))
    }

    pub(crate) fn for_cut(version: &str, build: u64, commit: &str) -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        assert!(
            command("git")
                .args(["init", "-q"])
                .arg(&repo)
                .status()
                .unwrap()
                .success()
        );
        let dist = repo.join("dist");
        fs::create_dir(&dist).unwrap();
        let slug = "draft-fixture/aterm";
        fs::write(
            repo.join("Cargo.toml"),
            format!("[workspace.package]\nrepository = \"https://github.com/{slug}\"\n"),
        )
        .unwrap();
        let fake = agh_fake::FakeGithub::start();
        let token = agh_fake::token("private-draft-cutter");
        fake.add_token(&token, "cutter");
        fake.add_repo(slug, agh_fake::RepoSpec::public().push("cutter"));
        fake.add_release(
            slug,
            agh_fake::ReleaseSpec::published("v0.190.0", "2026-11-08T00:00:00Z"),
        );
        let ports = fake.ports();
        let fixture = crate::github::Fixture {
            origins: aterm_update_core::github_rest::Origins::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ),
            token: Some(token),
            spawn: None,
        };
        let route = fixture.clone().enter();
        let mut ctx = CutCtx {
            credentials: None,
            apple: sign::AppleTier::Inactive,
            repo: repo.clone(),
            tree: repo.clone(),
            dist: dist.clone(),
            journal_path: dist.join(JOURNAL),
            slug: slug.into(),
            version: version.into(),
            tag: format!("v{version}"),
            build,
            commit: commit.into(),
            min_build: None,
            artifact: Some("b".repeat(64)),
            macho_sha256: Some("c".repeat(64)),
            linux: None,
            manifest_signed: false,
            signature_required: false,
            signature_pubkey: None,
            verify_pubkey: None,
            signature_machine_id: None,
            attribution: None,
            roster: None,
            release_id: None,
            release_intent: false,
            upload_intents: Vec::new(),
            kind: CutKind::Draft,
            lease: None,
            fence: None,
            journal: None,
            adopted: false,
        };
        let release = crate::release::Journal {
            version: ctx.version.clone(),
            commit: "d".repeat(40),
            reserved: Some(crate::release::Reservation {
                build: ctx.build,
                commit: ctx.commit.clone(),
                line: format!("{build} {version}"),
            }),
            ..Default::default()
        };
        release
            .save(
                &crate::release::release_dir(&GitCli::new(&repo))
                    .unwrap()
                    .join(crate::release::JOURNAL_FILE),
            )
            .unwrap();
        for name in ctx.channel_asset_names() {
            fs::write(dist.join(&name), format!("signed fixture: {name}")).unwrap();
        }
        fs::write(ctx.notes_path(), "Private fixture release").unwrap();
        record_bytes(&ctx).unwrap();
        let journal = snapshot(&ctx);
        journal.save(&ctx.journal_path).unwrap();
        ctx.journal = Some(journal);
        Self {
            _dir: dir,
            fake,
            ctx,
            route: fixture,
            _route: route,
        }
    }

    pub(crate) fn stage(&mut self) -> Result<()> {
        let _route = self.route.clone().enter();
        let slug = self.ctx.slug.clone();
        channel::stage_on_channel(&mut NetworkOnly(LiveChannelRelease {
            ctx: &mut self.ctx,
            slug: &slug,
            bound: None,
        }))
    }

    pub(crate) fn publish(&mut self) -> Result<()> {
        let _route = self.route.clone().enter();
        // The native ledger/fence are separately exercised by the claimed
        // pipeline's tests. This fixture exercises the real REST/asset path.
        self.ctx.kind = CutKind::DryRun;
        let slug = self.ctx.slug.clone();
        channel::publish_staged_on_channel(&mut NetworkOnly(LiveChannelRelease {
            ctx: &mut self.ctx,
            slug: &slug,
            bound: None,
        }))
    }

    pub(crate) fn posts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|r| r.method == "POST")
            .count()
    }

    pub(crate) fn attest(&self) {
        for name in channel::SOURCE_ATTESTATION_ASSETS {
            self.fake.apply(agh_fake::Change::AddAsset {
                release_id: self.ctx.release_id.unwrap(),
                spec: agh_fake::AssetSpec::uploaded(name, b"source evidence"),
            });
        }
    }

    pub(crate) fn source_read(&self, unavailable: bool) -> Result<aterm_json::Value> {
        let _route = self.route.clone().enter();
        let path = format!(
            "repos/{}/releases/{}",
            self.ctx.slug,
            self.ctx.release_id.unwrap()
        );
        if unavailable {
            self.fake.fault(
                agh_fake::Match::api(&format!("/{path}")).method("GET"),
                agh_fake::Fault::Status {
                    status: 404,
                    body: "unavailable source read".into(),
                    headers: Vec::new(),
                },
            );
        }
        crate::github::get(&path)
    }

    pub(crate) fn resume(&mut self) {
        let (build, commit) = (self.ctx.build, self.ctx.commit.clone());
        self.resume_as(build, &commit).unwrap();
    }

    /// A cut of the reservation `build` at `commit` adopts this lab's journaled draft and
    /// stages it; a refusal leaves the lab's cut as it was.
    pub(crate) fn resume_as(&mut self, build: u64, commit: &str) -> Result<()> {
        let was = (
            self.ctx.build,
            self.ctx.commit.clone(),
            self.ctx.release_id,
            self.ctx.release_intent,
            self.ctx.upload_intents.clone(),
        );
        self.ctx.kind = CutKind::Draft;
        self.ctx.build = build;
        commit.clone_into(&mut self.ctx.commit);
        self.ctx.release_id = None;
        self.ctx.release_intent = false;
        self.ctx.upload_intents.clear();
        if let Err(error) = adopt(&mut self.ctx) {
            (
                self.ctx.build,
                self.ctx.commit,
                self.ctx.release_id,
                self.ctx.release_intent,
                self.ctx.upload_intents,
            ) = was;
            return Err(error);
        }
        self.stage()
    }

    /// Journal `reserved` in this lab's release.toml, as the release's own journal holds it
    /// now; `None`: that journal is gone.
    pub(crate) fn reserve_as(&self, reserved: Option<crate::release::Reservation>) {
        let path = crate::release::release_dir(&GitCli::new(&self.ctx.repo))
            .unwrap()
            .join(crate::release::JOURNAL_FILE);
        let Some(reserved) = reserved else {
            forget_copy_record(&path).unwrap();
            return;
        };
        let mut release = crate::release::Journal::load(&path)
            .unwrap()
            .unwrap_or_else(|| crate::release::Journal {
                version: self.ctx.version.clone(),
                commit: "d".repeat(40),
                ..Default::default()
            });
        release.reserved = Some(reserved);
        release.save(&path).unwrap();
    }

    /// The engine's only form that attaches the source pair to this lab's journaled draft.
    pub(crate) fn keep_draft_remedy(&self) -> String {
        format!(
            "pub sign-release aterm --tag {} --keep-draft --draft-state {} --repo-path {}",
            self.ctx.tag,
            self.ctx.dist.join(JOURNAL).display(),
            self.ctx.repo.display()
        )
    }

    pub(crate) fn discard_signed(
        &self,
        reason: crate::release::SignedDiscard,
    ) -> crate::release::Discarded {
        self.discard_as(&self.ctx.version, self.ctx.build, reason)
    }

    /// [`super::discard_signed`] of build `build` of `version` in this lab's `dist/`.
    pub(crate) fn discard_as(
        &self,
        version: &str,
        build: u64,
        reason: crate::release::SignedDiscard,
    ) -> crate::release::Discarded {
        let _route = self.route.clone().enter();
        super::discard_signed(&self.ctx.dist, version, build, reason)
    }
}

/// Only product execution and native claims are injected. Upload, byte
/// proof, draft capability and the public PATCH use the production code.
struct NetworkOnly<'a>(LiveChannelRelease<'a>);

impl ChannelRelease for NetworkOnly<'_> {
    fn head_made(&mut self) -> Result<bool> {
        self.0.head_made()
    }
    fn artifacts(&mut self) -> Result<Vec<PathBuf>> {
        Ok(self.0.ctx.channel_asset_paths())
    }
    fn upload(&mut self, file: &Path) -> Result<()> {
        self.0.upload(file)
    }
    fn upload_payloads(&mut self, files: &[PathBuf]) -> Result<()> {
        self.0.upload_payloads(files)
    }
    fn prove_assets(&mut self) -> Result<()> {
        self.0.prove_assets()
    }
    fn ratchet_head(&mut self) -> Result<()> {
        Ok(())
    }
    fn ratchet(&mut self) -> Result<()> {
        Ok(())
    }
    fn bind(&mut self) -> Result<()> {
        self.0.bind()
    }
    fn make_head(&mut self) -> Result<()> {
        self.0.make_head()
    }
    fn prove_head(&mut self) -> Result<()> {
        let release = crate::github::get(&format!("repos/{}/releases/latest", self.0.slug))?;
        assert_eq!(release["id"].as_u64(), self.0.ctx.release_id);
        Ok(())
    }
}

/// NEGATIVE CONTROL: serializing upload after the source finishes deadlocks
/// this rendezvous; the minute is a hang detector, not a timing assertion.
#[test]
fn private_draft_uploads_while_source_publish_is_pending() {
    let mut lab = Lab::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let (staged, source) = crate::cut_timing::independent(
        ("private uploads", || {
            let r = lab.stage();
            tx.send(()).unwrap();
            r
        }),
        ("source publish", move || {
            rx.recv_timeout(Duration::from_secs(60))
        }),
    );
    staged.unwrap();
    source.unwrap();
    let id = lab.ctx.release_id.unwrap();
    assert!(lab.fake.release_state(id).unwrap().0.draft);
    assert_eq!(
        Journal::load(&lab.ctx.journal_path)
            .unwrap()
            .unwrap()
            .release_id,
        Some(id)
    );
    let latest = crate::github::get(&format!("repos/{}/releases/latest", lab.ctx.slug)).unwrap();
    assert_ne!(latest["id"].as_u64(), Some(id));
    let ports = lab.fake.ports();
    let name = channel::dmg_asset_name(&lab.ctx.version);
    let url = format!(
        "http://127.0.0.1:{}/{}/releases/download/{}/{}",
        ports.web, lab.ctx.slug, lab.ctx.tag, name
    );
    let anonymous = command("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            &url,
        ])
        .output()
        .unwrap();
    assert!(anonymous.status.success());
    assert_eq!(
        String::from_utf8(anonymous.stdout).unwrap(),
        "404",
        "draft asset URLs are private"
    );
    lab.attest();
    assert!(lab.source_read(true).is_err());
    lab.source_read(false).unwrap();
    lab.publish().unwrap();
    assert!(!lab.fake.release_state(id).unwrap().0.draft);
    let patches: Vec<_> = lab
        .fake
        .seen()
        .into_iter()
        .filter(|r| r.method == "PATCH")
        .collect();
    assert_eq!(patches.len(), 1);
    let posts = lab.posts();
    lab.ctx.kind = CutKind::Draft;
    assert!(lab.stage().is_err(), "a published release is never re-cut");
    assert_eq!(lab.posts(), posts);
    lab.fake.assert_clean();
}

#[test]
fn lost_reservation_discards_only_the_owned_private_release() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    lab.attest();
    let id = lab.ctx.release_id.unwrap();
    assert!(
        lab.discard_signed(crate::release::SignedDiscard::LostReservation)
            .complete()
    );
    assert!(lab.fake.release_state(id).is_none());
    assert_eq!(lab.fake.release_ids(&lab.ctx.slug).len(), 1);
    assert!(!lab.ctx.journal_path.exists());
    assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
    lab.fake.assert_clean();
}

/// A DROPPED RESERVATION'S DRAFT LEAVES ONLY WHILE RELEASE.TOML STILL RESERVES IT: once a
/// replacement reservation is journaled, authority refuses the old draft's discard, and a cut
/// of the replacement names the journaled draft and the person's remedy; while it still
/// stands, the discard withdraws exactly that draft and the next cut starts clean. A
/// cancellation retains only a preparation of the journal's own build. NEGATIVE CONTROL: the
/// cancellation kept another build's preparation beside any journal, and the mismatch said
/// only "differs from this cut; refusing adoption", naming neither the draft nor a way out.
#[test]
fn a_dropped_reservation_withdraws_its_draft_only_while_release_toml_reserves_it() {
    use crate::release::{Discarded, Reservation, SignedDiscard};
    let mut lab = Lab::new();
    lab.stage().unwrap();
    lab.attest();
    let id = lab.ctx.release_id.unwrap();
    let dist = lab.ctx.dist.clone();
    let replacement = Reservation {
        build: 192,
        commit: "e".repeat(40),
        line: "192 0.191.0".into(),
    };
    let prepared = |build| {
        let path = prepared_record_path(&dist, build);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "prepared fixture").unwrap();
        path
    };

    let own = prepared(191);
    assert!(
        lab.discard_as("0.191.0", 191, SignedDiscard::CancelPreparation)
            .complete()
    );
    assert!(own.exists(), "the journaled draft owns build 191's bytes");
    let other = prepared(192);
    assert!(
        lab.discard_as("0.191.0", 192, SignedDiscard::CancelPreparation)
            .complete()
    );
    assert!(
        !other.exists(),
        "build 191's draft owns nothing of build 192"
    );

    // What step 4 journals when it replaces the reservation.
    lab.reserve_as(Some(replacement.clone()));
    let refused = lab
        .resume_as(192, &replacement.commit)
        .unwrap_err()
        .to_string();
    let path = format!("repos/{}/releases/{id}", lab.ctx.slug);
    for words in [
        "v0.191.0 build 191",
        &format!("release {id}"),
        &format!("agh api {path} -X DELETE --confirm {path}"),
        "dist/draft-state.toml and dist/draft-bytes.toml",
    ] {
        assert!(refused.contains(words), "{words}: {refused}");
    }
    let left = lab.discard_as("0.191.0", 191, SignedDiscard::LostReservation);
    assert!(
        matches!(&left, Discarded::Held { why, remedy }
            if why.contains("does not reserve") && remedy.contains("-X DELETE")),
        "authority refuses once the reservation is replaced: {left:?}"
    );
    let left = lab.discard_as("0.191.0", 192, SignedDiscard::LostReservation);
    assert!(
        matches!(&left, Discarded::Held { why, remedy }
            if why.contains("another reservation's") && remedy.contains("-X DELETE")),
        "{left:?}"
    );
    assert!(lab.fake.release_state(id).is_some() && lab.ctx.journal_path.exists());

    // While release.toml reserves build 191, a cut that differs from its draft names --forget.
    lab.reserve_as(Some(Reservation {
        build: 191,
        commit: lab.ctx.commit.clone(),
        line: "191 0.191.0".into(),
    }));
    let artifact = lab.ctx.artifact.replace("f".repeat(64));
    let refused = adopt(&mut lab.ctx).unwrap_err().to_string();
    assert!(
        refused.contains("ship release --forget` withdraws it with its reservation"),
        "{refused}"
    );
    lab.ctx.artifact = artifact;

    assert!(
        lab.discard_as("0.191.0", 191, SignedDiscard::LostReservation)
            .complete()
    );
    assert!(lab.fake.release_state(id).is_none());
    assert!(!lab.ctx.journal_path.exists() && !dist.join(RECEIPT).exists());
    assert!(!own.exists());
    let deletes: Vec<_> = lab
        .fake
        .seen()
        .into_iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.path_and_query)
        .collect();
    assert_eq!(deletes, [format!("/{path}")]);
    lab.fake.assert_clean();
    // The replacement's cut finds no draft to adopt, so its sign drafts afresh.
    lab.ctx.build = 192;
    lab.ctx.commit.clone_from(&replacement.commit);
    lab.ctx.release_id = None;
    adopt(&mut lab.ctx).unwrap();
    assert_eq!(lab.ctx.release_id, None);
}

/// WHAT A WITHDRAWAL LEAVES SAYS WHETHER A RETRY CAN FINISH IT: a release read that fails
/// leaves the draft for a retry (`Except`); a draft the clone cannot prove its own or see is
/// held, decided, with what a person does (`Held`): a create POST with nothing under the tag,
/// several releases under it and no id bound, a bound id GitHub no longer answers, a bound
/// release now public, a journal this cutter cannot read. The bound id finds the draft
/// beside a twin, and a journal with no create POST forgets itself and leaves the tag's
/// release alone. NEGATIVE CONTROL: every refusal was one string, so the release retried a
/// permanent state as COULD NOT RUN for good; the undecided POST told the person to "retain
/// the journal", a twin under the tag blocked a bound draft's withdrawal, and a journal with
/// no POST refused the tag's release as "unowned".
#[test]
fn a_withdrawal_says_whether_a_retry_can_finish_it() {
    use crate::release::{Discarded, SignedDiscard::LostReservation};
    let removal = "remove dist/draft-state.toml and dist/draft-bytes.toml";
    let held = |left: &Discarded, why_has: &str, remedy_has: &str| {
        assert!(
            matches!(left, Discarded::Held { why, remedy }
                if why.contains(why_has) && remedy.contains(remedy_has)),
            "{why_has} / {remedy_has}: {left:?}"
        );
    };
    let deletes = |lab: &Lab| {
        lab.fake
            .seen()
            .iter()
            .filter(|r| r.method == "DELETE")
            .count()
    };

    // A create POST whose answer was lost, nothing under the tag: undecided.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let mut journal = Journal::load(&lab.ctx.journal_path).unwrap().unwrap();
    journal.release_id = None;
    journal.upload_intents.clear();
    journal.save(&lab.ctx.journal_path).unwrap();
    lab.fake
        .apply(agh_fake::Change::DeleteRelease { release_id: id });
    let left = lab.discard_signed(LostReservation);
    held(&left, "undecided", "releases page");
    held(&left, "v0.191.0", removal);
    assert!(!format!("{left:?}").contains("retain the journal"));
    assert!(lab.ctx.journal_path.exists());
    // Two drafts appear under the tag: which is this clone's is undecided too.
    let spec = || agh_fake::ReleaseSpec::draft("v0.191.0", "2026-11-09T00:00:00Z");
    let twins = [
        lab.fake.add_release(&lab.ctx.slug, spec()),
        lab.fake.add_release(&lab.ctx.slug, spec()),
    ];
    let left = lab.discard_signed(LostReservation);
    held(
        &left,
        &format!("IDs {}, {}", twins[0], twins[1]),
        "-X DELETE",
    );
    assert_eq!(deletes(&lab), 0);
    // One left: the POST's draft, withdrawn.
    lab.fake.apply(agh_fake::Change::DeleteRelease {
        release_id: twins[1],
    });
    assert!(lab.discard_signed(LostReservation).complete());
    assert!(lab.fake.release_state(twins[0]).is_none());
    assert!(!lab.ctx.journal_path.exists());

    // A bound id GitHub no longer answers.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake
        .apply(agh_fake::Change::DeleteRelease { release_id: id });
    held(
        &lab.discard_signed(LostReservation),
        &format!("no release {id}"),
        removal,
    );
    assert!(lab.ctx.journal_path.exists());

    // A bound release now public is never withdrawn.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake.apply(agh_fake::Change::Publish { release_id: id });
    held(&lab.discard_signed(LostReservation), "is public", removal);
    assert!(lab.fake.release_state(id).is_some());
    assert_eq!(deletes(&lab), 0);

    // GitHub's release reads fail: a retry may finish it.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    lab.fake.fault_always(
        agh_fake::Match::api(&format!("/repos/{}/releases", lab.ctx.slug)).method("GET"),
        agh_fake::Fault::Status {
            status: 502,
            body: "bad gateway".into(),
            headers: Vec::new(),
        },
    );
    let left = lab.discard_signed(LostReservation);
    assert!(
        matches!(&left, Discarded::Except(why) if why.len() == 1 && why[0].contains("502")),
        "{left:?}"
    );
    assert!(lab.ctx.journal_path.exists() && lab.fake.release_state(id).is_some());

    // The bound id finds this clone's draft beside a twin under its tag.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let twin = lab.fake.add_release(&lab.ctx.slug, spec());
    assert!(lab.discard_signed(LostReservation).complete());
    assert!(lab.fake.release_state(id).is_none());
    assert!(lab.fake.release_state(twin).is_some(), "not this clone's");

    // No create POST: the journal forgets itself; the tag's release is not this clone's.
    let lab = Lab::new();
    let source = lab.fake.add_release(
        &lab.ctx.slug,
        agh_fake::ReleaseSpec::published("v0.191.0", "2026-11-09T00:00:00Z"),
    );
    assert!(lab.discard_signed(LostReservation).complete());
    assert!(lab.fake.release_state(source).is_some());
    assert!(!lab.ctx.journal_path.exists());
    assert_eq!(deletes(&lab), 0);

    // A journal this cutter cannot read.
    let lab = Lab::new();
    fs::write(&lab.ctx.journal_path, "format = \"no\"\n").unwrap();
    held(
        &lab.discard_signed(LostReservation),
        "cannot be read as a private-draft journal",
        removal,
    );
    assert!(lab.ctx.journal_path.exists());
}

/// A WITHDRAWAL CUT SHORT IS FINISHED BY THE RETRY ITS COULD NOT RUN PROMISES. With `dist/`
/// read-only, the `delete_intent` journal write fails before any DELETE goes out (nothing in
/// `dist/` is written or removed). With the receipt a directory no unlink removes, the DELETE
/// lands and the receipt stays: the half that covers a DELETE that landed, whose retry reads
/// the journaled id's 404, with the listing agreeing, as done. Each leaves Except, a retry
/// while it stands leaves Except again, and once it clears the retry completes, the draft
/// deleted once; meanwhile no cut adopts the draft being withdrawn. A read after the DELETE
/// that fails leaves the state the second half leaves (the DELETE landed, the journal names
/// it), and has no case of its own here.
/// NEGATIVE CONTROL: nothing journaled the DELETE, so once it landed the retry met the bound
/// id's 404 and held the draft for a person ("GitHub answers no release …"), exit 1 for a
/// draft this clone deleted itself; and the journal went before the receipt, so a retry
/// with the receipt still there found no journal and called the withdrawal complete.
#[test]
fn a_withdrawal_cut_short_after_its_delete_is_finished_by_a_retry() {
    use crate::release::{Discarded, SignedDiscard::LostReservation};
    use std::os::unix::fs::PermissionsExt;
    let except = |left: &Discarded, has: &str| matches!(left, Discarded::Except(why) if why.len() == 1 && why[0].contains(has));
    let deletes = |lab: &Lab| {
        lab.fake
            .seen()
            .into_iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| r.path_and_query)
            .collect::<Vec<_>>()
    };

    // dist/ read-only: restored before any assertion, so the scratch directory goes.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let mode = |lab: &Lab, mode| {
        fs::set_permissions(&lab.ctx.dist, fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(&lab, 0o555);
    let left = lab.discard_signed(LostReservation);
    let again = lab.discard_signed(LostReservation);
    mode(&lab, 0o755);
    assert!(except(&left, JOURNAL), "{left:?}");
    assert!(
        except(&again, JOURNAL),
        "a retry while it stands: {again:?}"
    );
    let done = lab.discard_signed(LostReservation);
    assert!(done.complete(), "{done:?}");
    assert!(lab.fake.release_state(id).is_none());
    assert!(!lab.ctx.journal_path.exists() && !lab.ctx.dist.join(RECEIPT).exists());
    assert_eq!(
        deletes(&lab),
        [format!("/repos/{}/releases/{id}", lab.ctx.slug)]
    );

    // The receipt is a directory: the DELETE lands, and the receipt stays.
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let receipt = lab.ctx.dist.join(RECEIPT);
    fs::remove_file(&receipt).unwrap();
    fs::create_dir_all(receipt.join("held")).unwrap();
    let left = lab.discard_signed(LostReservation);
    assert!(except(&left, RECEIPT), "{left:?}");
    assert!(lab.fake.release_state(id).is_none(), "the DELETE landed");
    // A draft whose withdrawal began is never adopted; its withdrawal finishes it.
    let (build, commit) = (lab.ctx.build, lab.ctx.commit.clone());
    let adopted = lab.resume_as(build, &commit).unwrap_err().to_string();
    assert!(
        adopted.contains("is being withdrawn") && adopted.contains("--forget"),
        "{adopted}"
    );
    let again = lab.discard_signed(LostReservation);
    assert!(
        except(&again, RECEIPT),
        "never complete while the receipt is left: {again:?}"
    );
    fs::remove_dir_all(&receipt).unwrap();
    let done = lab.discard_signed(LostReservation);
    assert!(done.complete(), "{done:?}");
    assert!(!lab.ctx.journal_path.exists());
    assert_eq!(
        deletes(&lab),
        [format!("/repos/{}/releases/{id}", lab.ctx.slug)]
    );
}

/// A JOURNALED DELETE IS TAKEN FOR DONE ONLY WHERE THE LISTING AGREES: the withdrawal
/// journaled its DELETE and stopped before sending it (killed there, or the guard's first read
/// failed), and the retry's read of the release by its id answers 404 once while the
/// releases listing, drafts included, still holds it. The retry is Except, with the draft,
/// its journal and its receipt in place, and the next retry deletes the draft, once.
/// NEGATIVE CONTROL: one 404 for the journaled id counted as the withdrawal's own, so the
/// retry forgot the journal, the receipt and the signed copy and called the withdrawal
/// complete while the private draft stood on GitHub, a DELETE never sent.
#[test]
fn a_journaled_delete_is_done_only_where_the_listing_agrees() {
    use crate::release::{Discarded, SignedDiscard::LostReservation};
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let mut journal = Journal::load(&lab.ctx.journal_path).unwrap().unwrap();
    journal.delete_intent = Some(id);
    journal.save(&lab.ctx.journal_path).unwrap();
    lab.fake.fault(
        agh_fake::Match::api(&format!("/repos/{}/releases/{id}", lab.ctx.slug)).method("GET"),
        agh_fake::Fault::Status {
            status: 404,
            body: "{\"message\":\"Not Found\"}".into(),
            headers: Vec::new(),
        },
    );
    let left = lab.discard_signed(LostReservation);
    // (a retry may finish it, the draft stands, its journal, its receipt, DELETEs sent)
    assert_eq!(
        (
            matches!(&left, Discarded::Except(why)
                if why.len() == 1 && why[0].contains(&format!("still lists release {id}"))),
            lab.fake
                .release_state(id)
                .is_some_and(|(spec, _)| spec.draft),
            lab.ctx.journal_path.exists(),
            lab.ctx.dist.join(RECEIPT).exists(),
            lab.fake
                .seen()
                .iter()
                .filter(|r| r.method == "DELETE")
                .count(),
        ),
        (true, true, true, true, 0),
        "{left:?}"
    );

    let done = lab.discard_signed(LostReservation);
    assert!(done.complete(), "{done:?}");
    assert!(lab.fake.release_state(id).is_none());
    assert!(!lab.ctx.journal_path.exists() && !lab.ctx.dist.join(RECEIPT).exists());
    let deletes: Vec<_> = lab
        .fake
        .seen()
        .into_iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.path_and_query)
        .collect();
    assert_eq!(deletes, [format!("/repos/{}/releases/{id}", lab.ctx.slug)]);
}

/// WITHOUT A JOURNAL, RECOVER DELETES ONLY A DRAFT OF BARE CUT ASSETS: the decision
/// [`withdraw_unpublished_release`] takes over the tag's draft when no journal names a
/// create intent (`recover` of a cut whose `dist/cut-state.toml` is gone) deletes a draft
/// holding nothing but this version's cut assets, and refuses, keeping the claim lease, the
/// same draft once it carries step 7's source pair, as the private draft a claim adopts does
/// after the source signing. The way out a `dist/cut-state.toml` that is no cut journal
/// names (release::cut_journal_invalid) rests on both.
#[test]
fn recover_without_a_journal_refuses_a_draft_carrying_the_source_pair() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let decision = |lab: &Lab| {
        let _route = lab.route.clone().enter();
        let names: Vec<String> = release_asset_inventory_for_release_id(&lab.ctx.slug, id)
            .unwrap()
            .into_iter()
            .map(|asset| asset.name)
            .collect();
        assert!(!names.is_empty());
        draft_cleanup_decision(
            None,
            true,
            channel::binds_to_version(&names, &lab.ctx.version),
        )
    };
    assert_eq!(decision(&lab), DraftCleanupDecision::DeleteIssuedVisible);
    lab.attest();
    assert_eq!(
        decision(&lab),
        DraftCleanupDecision::RefuseUnknownOrInconsistent
    );
}

#[test]
fn cancelling_sign_preparation_keeps_an_existing_attested_draft_and_its_bytes() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    lab.attest();
    let id = lab.ctx.release_id.unwrap();
    let state = lab.fake.release_state(id).unwrap();
    let journal = fs::read(&lab.ctx.journal_path).unwrap();
    assert!(
        discard_signed(
            &lab.ctx.dist,
            &lab.ctx.version,
            lab.ctx.build,
            crate::release::SignedDiscard::CancelPreparation
        )
        .complete()
    );
    assert_eq!(lab.fake.release_state(id), Some(state));
    assert_eq!(fs::read(&lab.ctx.journal_path).unwrap(), journal);
    check_bytes(&lab.ctx).unwrap();
    lab.fake.assert_clean();
}

#[test]
fn matching_resume_adopts_without_another_upload_and_mismatch_refuses() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let posts = lab.posts();
    lab.resume();
    assert_eq!(
        lab.posts(),
        posts,
        "matching bytes must never be re-uploaded"
    );
    let name = channel::dmg_asset_name(&lab.ctx.version);
    let wrong = vec![b'x'; fs::metadata(lab.ctx.dist.join(&name)).unwrap().len() as usize];
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: lab.ctx.release_id.unwrap(),
        spec: agh_fake::AssetSpec::uploaded(&name, &wrong),
    });
    let error = lab.stage().unwrap_err();
    assert!(error.to_string().contains("bytes differ"), "{error}");
    assert_eq!(lab.posts(), posts, "a mismatch must never clobber");
    assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
}

/// NEGATIVE CONTROL: the cut has no upload path. Removing a required draft
/// asset must refuse before its head PATCH, even with all local bytes present.
#[test]
fn cut_refuses_an_incomplete_draft_instead_of_repairing_or_publishing_it() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    let id = lab.ctx.release_id.unwrap();
    let name = channel::dmg_asset_name(&lab.ctx.version);
    let (asset, _) = release_asset_identity_for_release_id(&lab.ctx.slug, id, &name).unwrap();
    crate::github::send_once(
        aterm_update_core::github_rest::Method::Delete,
        &format!("repos/{}/releases/assets/{asset}", lab.ctx.slug),
        None,
    )
    .unwrap();
    let posts = lab.posts();
    assert!(lab.publish().is_err());
    assert_eq!(posts, lab.posts());
    assert!(lab.fake.release_state(id).unwrap().0.draft);
    assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
}

/// NEGATIVE CONTROL: today's make_head publishes each incomplete source pair.
#[test]
fn cut_requires_both_source_attestations_on_the_bound_release_before_patch() {
    for missing in [
        vec!["SHA256SUMS", "SHA256SUMS.sig"],
        vec!["SHA256SUMS"],
        vec!["SHA256SUMS.sig"],
    ] {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        for name in channel::SOURCE_ATTESTATION_ASSETS {
            if !missing.contains(&name) {
                lab.fake.apply(agh_fake::Change::AddAsset {
                    release_id: lab.ctx.release_id.unwrap(),
                    spec: agh_fake::AssetSpec::uploaded(name, b"source evidence"),
                });
            }
        }
        // A complete pair on another release cannot authorize this cut.
        let other = lab.fake.release_ids(&lab.ctx.slug)[0];
        for name in channel::SOURCE_ATTESTATION_ASSETS {
            lab.fake.apply(agh_fake::Change::AddAsset {
                release_id: other,
                spec: agh_fake::AssetSpec::uploaded(name, b"foreign source evidence"),
            });
        }
        let posts = lab.posts();
        let error = lab.publish().unwrap_err().to_string();
        assert!(error.contains(&lab.keep_draft_remedy()), "{error}");
        for name in missing {
            assert!(error.contains(name), "{error}");
        }
        assert_eq!(lab.posts(), posts);
        assert!(
            lab.fake
                .release_state(lab.ctx.release_id.unwrap())
                .unwrap()
                .0
                .draft
        );
        assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
        lab.attest();
        lab.publish().unwrap();
        lab.fake.assert_clean();
    }
}

/// A DRAFT IS NAMED ONLY THE FORM THAT REACHES IT: the signature is missing on the cut's
/// private draft, and dist/draft-state.toml is gone, journals another build's draft of the
/// version, or does not parse. The refusal names its finding and the `--keep-draft` form
/// whose `--draft-state` is the cut's own journal, which binds exactly this draft: its id,
/// version, create intent and claim commit, every field the engine's cutter_draft reads (it
/// never reads the build). It never names the plain form, which would publish the draft. When
/// the cut's journal binds no release either, no form is named, and neither is a resume: the
/// refusal names what proceeds, `recover` with the claim commit, after the draft's DELETE by
/// hand where it carries any of the source pair, which recover refuses without a journal.
/// NEGATIVE CONTROL: the form named dist/draft-state.toml whatever it held. Gone, the
/// engine's cutter_draft waited forever on a journal naming no release; another build's, it
/// attached the pair to that build's draft. The refusal said the form "refuses until" the
/// journal names this draft, which no tool brings about. Then, with no form, it said "finish
/// the source signing (no form reaches this draft …), then resume the cut", naming nothing
/// that proceeds.
#[test]
fn head_patch_names_only_a_form_that_reaches_its_draft() {
    for case in ["gone", "another build's", "unreadable", "no cut journal"] {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        let id = lab.ctx.release_id.unwrap();
        lab.fake.apply(agh_fake::Change::AddAsset {
            release_id: id,
            spec: agh_fake::AssetSpec::uploaded("SHA256SUMS", b"source evidence"),
        });
        // The claim journals its cut in dist/cut-state.toml; the draft's journal is apart.
        let draft_journal = lab.ctx.journal_path.clone();
        lab.ctx.journal_path = lab.ctx.dist.join("cut-state.toml");
        match case {
            "another build's" => {
                let mut journal = Journal::load(&draft_journal).unwrap().unwrap();
                journal.build_number = 192;
                journal.release_id = Some(id + 1);
                journal.save(&draft_journal).unwrap();
            }
            "unreadable" => fs::write(&draft_journal, "format = \"no\"\n").unwrap(),
            // The private draft's journal still binds the draft; the cut's binds nothing.
            "no cut journal" => lab.ctx.journal = None,
            _ => fs::remove_file(&draft_journal).unwrap(),
        }
        let error = lab.publish().unwrap_err().to_string();
        assert!(
            error.contains("SHA256SUMS.sig is missing"),
            "{case}: {error}"
        );
        assert!(!error.contains("refuses until"), "{case}: {error}");
        assert!(
            !error.contains("(pub sign-release aterm --tag v0.191.0)"),
            "{case}: never the plain form: {error}"
        );
        assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
        let keep = format!(
            "pub sign-release aterm --tag v0.191.0 --keep-draft --draft-state {}",
            lab.ctx.dist.display()
        );
        if case == "no cut journal" {
            // No form, and so no resume: what proceeds is recover, deleting the draft by hand
            // first where it carries the source pair recover refuses without a journal.
            for words in [
                "no form reaches this draft",
                "it is gone",
                &format!(
                    "ship recover v0.191.0 {} --old-publisher-stopped",
                    lab.ctx.commit
                ),
                &format!("repos/{}/releases/{id} -X DELETE", lab.ctx.slug),
                "--no-draft-was-posted",
            ] {
                assert!(error.contains(words), "{words}: {error}");
            }
            assert!(
                !error.contains(&keep) && !error.contains("then resume the cut"),
                "{error}"
            );
            continue;
        }
        let named = error
            .split_once(&keep)
            .and_then(|(_, rest)| rest.split_once(" --repo-path "))
            .map(|(file, _)| lab.ctx.dist.join(file.trim_start_matches('/')))
            .unwrap_or_else(|| panic!("{case}: no `--keep-draft` form: {error}"));
        // What the engine reads from the journal the form names: this draft, no other.
        let state = Journal::load(&named)
            .unwrap()
            .unwrap_or_else(|| panic!("{case}: {} is gone: {error}", named.display()));
        assert_eq!(state.release_id, Some(id), "{case}: {error}");
        assert!(state.release_intent, "{case}");
        assert_eq!(
            (state.version.as_str(), state.commit.as_str()),
            ("0.191.0", lab.ctx.commit.as_str()),
            "{case}"
        );
    }
}

/// The refusal names the `agh` DELETE of the unfinished asset, since the source signing
/// cannot upload over it. NEGATIVE CONTROL: the head PATCH counted the attestation pair by
/// name, so an upload GitHub still lists as `starter` (or one that finished empty) passed and
/// the release went public with a source signature nobody can download; then the refusal
/// said to delete the asset and named no command.
#[test]
fn head_patch_refuses_a_source_attestation_still_uploading() {
    for (unfinished, state, bytes) in [
        ("SHA256SUMS.sig", "starter", &b""[..]),
        ("SHA256SUMS", "starter", &b""[..]),
        ("SHA256SUMS.sig", "open", &b"partial"[..]),
        ("SHA256SUMS.sig", "uploaded", &b""[..]),
    ] {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        let id = lab.ctx.release_id.unwrap();
        let mut asset = 0;
        for name in channel::SOURCE_ATTESTATION_ASSETS {
            let spec = if name == unfinished {
                agh_fake::AssetSpec {
                    state: state.into(),
                    ..agh_fake::AssetSpec::uploaded(name, bytes)
                }
            } else {
                agh_fake::AssetSpec::uploaded(name, b"source evidence")
            };
            let added = lab.fake.add_asset(id, spec);
            if name == unfinished {
                asset = added;
            }
        }
        let posts = lab.posts();
        let error = lab.publish().unwrap_err().to_string();
        let path = format!("repos/{}/releases/assets/{asset}", lab.ctx.slug);
        assert!(
            error.contains(unfinished)
                && error.contains(state)
                && error.contains(&format!("agh api {path} -X DELETE --confirm {path}"))
                && error.contains(&lab.keep_draft_remedy()),
            "{error}"
        );
        assert_eq!(lab.posts(), posts);
        assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
        assert!(lab.fake.release_state(id).unwrap().0.draft);
        lab.fake.apply(agh_fake::Change::AddAsset {
            release_id: id,
            spec: agh_fake::AssetSpec::uploaded(unfinished, b"source evidence"),
        });
        lab.publish().unwrap();
        assert!(!lab.fake.release_state(id).unwrap().0.draft);
        lab.fake.assert_clean();
    }
}

#[test]
fn head_patch_refuses_each_duplicated_source_attestation() {
    for name in channel::SOURCE_ATTESTATION_ASSETS {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        lab.attest();
        let id = lab.ctx.release_id.unwrap();
        let path = format!("repos/{}/releases/{id}", lab.ctx.slug);
        let mut release = crate::github::get(&path).unwrap();
        let duplicate = release["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .unwrap()
            .clone();
        release
            .as_object_mut()
            .unwrap()
            .get_mut("assets")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        lab.fake.fault(
            agh_fake::Match::api(&format!("/{path}")).method("GET"),
            agh_fake::Fault::Status {
                status: 200,
                body: release.to_string(),
                headers: Vec::new(),
            },
        );
        lab.ctx.kind = CutKind::DryRun;
        let slug = lab.ctx.slug.clone();
        let error = LiveChannelRelease {
            ctx: &mut lab.ctx,
            slug: &slug,
            bound: Some((id, false)),
        }
        .make_head()
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(name) && error.contains(&lab.keep_draft_remedy()),
            "{error}"
        );
        assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
        lab.fake.assert_clean();
    }
}

#[test]
fn head_patch_rechecks_source_attestations_before_retrying() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    lab.attest();
    let id = lab.ctx.release_id.unwrap();
    let path = format!("/repos/{}/releases/{id}", lab.ctx.slug);
    let release = crate::github::get(&path).unwrap();
    let mut incomplete = release.clone();
    incomplete
        .as_object_mut()
        .unwrap()
        .get_mut("assets")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .retain(|a| a["name"].as_str() != Some("SHA256SUMS.sig"));
    for response in [release, incomplete] {
        lab.fake.fault(
            agh_fake::Match::api(&path).method("GET"),
            agh_fake::Fault::Status {
                status: 200,
                body: response.to_string(),
                headers: Vec::new(),
            },
        );
    }
    lab.fake.fault(
        agh_fake::Match::api(&path).method("PATCH"),
        agh_fake::Fault::Status {
            status: 502,
            body: "transient patch refusal".into(),
            headers: Vec::new(),
        },
    );
    lab.ctx.kind = CutKind::DryRun;
    let slug = lab.ctx.slug.clone();
    let error = LiveChannelRelease {
        ctx: &mut lab.ctx,
        slug: &slug,
        bound: Some((id, false)),
    }
    .make_head()
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("SHA256SUMS.sig") && error.contains(&lab.keep_draft_remedy()),
        "{error}"
    );
    assert_eq!(
        lab.fake
            .seen()
            .iter()
            .filter(|r| r.method == "PATCH")
            .count(),
        1
    );
    assert!(lab.fake.release_state(id).unwrap().0.draft);
    lab.fake.assert_clean();
}

/// NEGATIVE CONTROL: the guard said only "finish pub sign-release aterm" — for the journaled
/// draft the engine reaches only with `--keep-draft --draft-state` (without them it takes a
/// published release of the tag, publishing a draft it finds), and for a published release
/// without its tag.
#[test]
fn head_patch_names_the_sign_release_form_that_reaches_the_bound_release() {
    let mut lab = Lab::new();
    let published = lab.fake.add_release(
        &lab.ctx.slug,
        agh_fake::ReleaseSpec::published(&lab.ctx.tag, "2026-11-09T00:00:00Z"),
    );
    lab.fake.apply(agh_fake::Change::AddAsset {
        release_id: published,
        spec: agh_fake::AssetSpec::uploaded("SHA256SUMS", b"source evidence"),
    });
    lab.ctx.kind = CutKind::DryRun;
    let slug = lab.ctx.slug.clone();
    let error = LiveChannelRelease {
        ctx: &mut lab.ctx,
        slug: &slug,
        bound: Some((published, true)),
    }
    .make_head()
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("SHA256SUMS.sig")
            && error.contains("pub sign-release aterm --tag v0.191.0")
            && !error.contains("--keep-draft"),
        "{error}"
    );
    assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
    lab.fake.assert_clean();
}

#[test]
fn changing_local_signed_bytes_or_the_reservation_refuses_adoption() {
    let mut lab = Lab::new();
    lab.stage().unwrap();
    check_bytes(&lab.ctx).unwrap();
    fs::write(lab.ctx.dmg_path(), b"changed").unwrap();
    assert!(check_bytes(&lab.ctx).is_err());
    assert!(authority(&lab.ctx.repo, &"f".repeat(40), lab.ctx.build).is_err());
    lab.publish().unwrap_err();
}

#[test]
fn an_unowned_draft_is_never_adopted_even_when_empty() {
    let mut lab = Lab::new();
    let foreign = lab.fake.add_release(
        &lab.ctx.slug,
        agh_fake::ReleaseSpec::draft(&lab.ctx.tag, "2026-11-09T00:00:00Z"),
    );
    assert!(lab.stage().is_err());
    assert_eq!(lab.posts(), 0);
    assert!(lab.fake.release_state(foreign).unwrap().1.is_empty());
    assert!(!lab.ctx.release_intent);
}

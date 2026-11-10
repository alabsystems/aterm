// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The private half of a cut. Its journal precedes every POST; the claim adopts
//! exactly these bytes, and only the claimed channel path can make them public.

use super::*;

const JOURNAL: &str = "draft-state.toml";
const RECEIPT: &str = "draft-bytes.toml";

#[derive(Serialize, Deserialize)]
struct Bytes {
    files: Vec<FileDigest>,
}

#[derive(Serialize, Deserialize)]
struct FileDigest {
    name: String,
    sha256: String,
}

pub(super) fn authority(repo: &Path, commit: &str, build: u64) -> Result<()> {
    let git = GitCli::new(repo);
    let path = crate::release::release_dir(&git)
        .map_err(|e| Error::new(e.to_string()))?
        .join(crate::release::JOURNAL_FILE);
    let release = crate::release::Journal::load(&path)
        .map_err(|e| Error::new(e.to_string()))?
        .ok_or_else(|| Error::new("private upload has no release journal"))?;
    if !release
        .reserved
        .as_ref()
        .is_some_and(|r| r.commit == commit && r.build == build)
    {
        return Err(Error::new(
            "private upload's reservation no longer belongs to this release",
        ));
    }
    Ok(())
}

fn snapshot(ctx: &CutCtx) -> Journal {
    Journal {
        format: JOURNAL_FORMAT,
        version: ctx.version.clone(),
        build_number: ctx.build,
        commit: ctx.commit.clone(),
        min_build: ctx.min_build,
        artifact: ctx.artifact.clone(),
        macho_sha256: ctx.macho_sha256.clone(),
        linux: ctx.linux.clone(),
        manifest_signed: ctx.manifest_signed,
        signature_required: ctx.signature_required,
        signature_pubkey: ctx.signature_pubkey.clone(),
        verify_pubkey: ctx.verify_pubkey.clone(),
        signature_machine_id: ctx.signature_machine_id.clone(),
        release_id: ctx.release_id,
        release_intent: ctx.release_intent,
        upload_intents: ctx.upload_intents.clone(),
        done: Vec::new(),
    }
}

fn agrees(ctx: &CutCtx, draft: &Journal) -> Result<()> {
    if draft.version != ctx.version
        || draft.build_number != ctx.build
        || draft.commit != ctx.commit
        || draft.artifact != ctx.artifact
        || draft.macho_sha256 != ctx.macho_sha256
        || draft.min_build != ctx.min_build
        || draft.signature_required != ctx.signature_required
        || draft.signature_pubkey != ctx.signature_pubkey
        || draft.signature_machine_id != ctx.signature_machine_id
    {
        return Err(Error::new(
            "private draft identity, floor or signer differs from this cut; refusing adoption",
        ));
    }
    Ok(())
}

fn record_bytes(ctx: &CutCtx) -> Result<()> {
    let files = ctx
        .channel_asset_names()
        .into_iter()
        .map(|name| {
            let sha256 = artifact::file_sha256(&ctx.dist.join(&name))
                .map_err(|e| Error::new(e.to_string()))?;
            Ok(FileDigest { name, sha256 })
        })
        .collect::<Result<Vec<_>>>()?;
    let text = aterm_toml::to_string(&Bytes { files }).map_err(|e| Error::new(e.to_string()))?;
    crate::durable::write_atomic_durable(&ctx.dist.join(RECEIPT), text.as_bytes(), 0o644)
        .map_err(|e| Error::new(e.to_string()))
}

fn check_bytes(ctx: &CutCtx) -> Result<()> {
    let text = crate::durable::read_regular_text(&ctx.dist.join(RECEIPT), 1 << 20)
        .map_err(|e| Error::new(e.to_string()))?;
    let receipt: Bytes = aterm_toml::from_str(&text).map_err(|e| Error::new(e.to_string()))?;
    let names: Vec<_> = receipt.files.iter().map(|f| f.name.clone()).collect();
    if names != ctx.channel_asset_names() {
        return Err(Error::new(
            "private draft receipt has an incomplete or foreign asset set",
        ));
    }
    for file in receipt.files {
        if artifact::file_sha256(&ctx.dist.join(&file.name))
            .map_err(|e| Error::new(e.to_string()))?
            != file.sha256
        {
            return Err(Error::new(format!(
                "private draft digest mismatch: {}",
                file.name
            )));
        }
    }
    Ok(())
}

/// Import the immutable release id and one-shot intents into the claim's journal.
pub(super) fn adopt(ctx: &mut CutCtx) -> Result<()> {
    let Some(draft) = Journal::load(&ctx.dist.join(JOURNAL))? else {
        return Ok(());
    };
    agrees(ctx, &draft)?;
    ctx.manifest_signed = draft.manifest_signed;
    ctx.release_id = draft.release_id;
    ctx.release_intent = draft.release_intent;
    ctx.upload_intents.clone_from(&draft.upload_intents);
    ctx.linux.clone_from(&draft.linux);
    check_bytes(ctx)?;
    if let Some(journal) = &mut ctx.journal {
        journal.manifest_signed = draft.manifest_signed;
        journal.release_id = draft.release_id;
        journal.release_intent = draft.release_intent;
        journal.upload_intents = draft.upload_intents;
        journal.linux = draft.linux;
        journal.save(&ctx.journal_path)?;
    }
    Ok(())
}

/// A resume retains the original manifest date and signature, as well as containers.
pub(super) fn staged(ctx: &mut CutCtx) -> Result<bool> {
    let Some(draft) = Journal::load(&ctx.dist.join(JOURNAL))? else {
        return Ok(false);
    };
    agrees(ctx, &draft)?;
    check_bytes(ctx)?;
    let source = signing_source(ctx)?;
    let record = read_signed_record(&ctx.dist, ctx.build)
        .map_err(Error::new)?
        .ok_or_else(|| Error::new("private draft lost its signed bundle record"))?;
    signed_copy_stands(&record, &signed_key(ctx, &source.complete)?, &ctx.dist)
        .map_err(Error::new)?;
    prove_publication_on_disk(ctx)?;
    ctx.adopted = true;
    Ok(true)
}

pub(super) fn prepare(ctx: &mut CutCtx, opts: &CutOptions) -> Result<()> {
    let _timing = crate::cut_timing::Timer::new("private draft staging");
    let newest = verify::scan_head_before_claim(&ctx.slug)?;
    ctx.min_build = effective_min_build(
        opts.min_build,
        newest.first().and_then(|r| r.min_build),
        ctx.build,
    )?;
    let path = ctx.dist.join(JOURNAL);
    if let Some(draft) = Journal::load(&path)? {
        agrees(ctx, &draft)?;
        ctx.manifest_signed = draft.manifest_signed;
        ctx.linux.clone_from(&draft.linux);
        ctx.release_id = draft.release_id;
        ctx.release_intent = draft.release_intent;
        ctx.upload_intents.clone_from(&draft.upload_intents);
        ctx.journal = Some(draft);
        check_bytes(ctx)?;
        prove_publication_on_disk(ctx)?;
    } else {
        ctx.linux = opts
            .linux_artifacts
            .as_deref()
            .map(|dir| {
                buildplan::linux::Handoff::new(dir, &opts.linux_targets, &opts.linux_workers)
            })
            .transpose()?;
        step_sign(ctx)?;
        prove_publication_on_disk(ctx)?;
        record_bytes(ctx)?;
        let journal = snapshot(ctx);
        journal.save(&path)?;
        ctx.journal = Some(journal);
    }
    ctx.journal_path = path;
    ctx.kind = CutKind::Draft;
    authority(&ctx.repo, &ctx.commit, ctx.build)?;
    let slug = ctx.slug.clone();
    channel::stage_on_channel(&mut LiveChannelRelease {
        ctx,
        slug: &slug,
        bound: None,
    })
}

/// Delete only this journal's exact draft. A published release is never withdrawn.
pub(super) fn discard(dist: &Path, version: &str, build: u64) -> Result<()> {
    let path = dist.join(JOURNAL);
    let Some(draft) = Journal::load(&path)? else {
        return Ok(());
    };
    if draft.version != version || draft.build_number != build {
        return Err(Error::new(
            "refusing to discard another reservation's private draft",
        ));
    }
    let repo = dist
        .parent()
        .ok_or_else(|| Error::new("draft has no repository"))?;
    authority(repo, &draft.commit, draft.build_number)?;
    let slug = workspace_channel_slug(repo)?;
    let tag = format!("v{version}");
    let observed = unique_release_object_by_tag(&slug, &tag)?;
    if let Some(release) = observed {
        if draft.release_id.is_some_and(|id| id != release.id)
            || !draft.release_intent
            || !release.draft
        {
            return Err(Error::new(
                "private draft cleanup refuses a replacement, unowned or published release",
            ));
        }
        delete_release_object_by_id_with_guard(
            &slug,
            &release,
            false,
            || authority(repo, &draft.commit, build),
            || authority(repo, &draft.commit, build),
        )?;
    } else if draft.release_intent {
        return Err(Error::new(
            "private draft POST outcome is not visible; retain the journal for digest adoption on resume",
        ));
    }
    forget_copy_record(&path)?;
    forget_copy_record(&dist.join(RECEIPT))
}

pub(super) fn forget_published(ctx: &CutCtx) -> Result<()> {
    let Some(draft) = Journal::load(&ctx.dist.join(JOURNAL))? else {
        return Ok(());
    };
    agrees(ctx, &draft)?;
    if ctx.release_id != draft.release_id {
        return Err(Error::new(
            "published cut does not own the private draft journal",
        ));
    }
    forget_copy_record(&ctx.dist.join(JOURNAL))?;
    forget_copy_record(&ctx.dist.join(RECEIPT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelRelease;

    struct Lab {
        _dir: aterm_tempfile::TempDir,
        fake: agh_fake::FakeGithub,
        ctx: CutCtx,
        _route: crate::github::FixtureGuard,
    }

    impl Lab {
        fn new() -> Self {
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
            let route = crate::github::Fixture {
                origins: aterm_update_core::github_rest::Origins::loopback(
                    ports.api,
                    ports.uploads,
                    ports.web,
                    ports.cdn,
                ),
                token: Some(token),
                spawn: None,
            }
            .enter();
            let mut ctx = CutCtx {
                credentials: None,
                apple: sign::AppleTier::Inactive,
                repo: repo.clone(),
                tree: repo.clone(),
                dist: dist.clone(),
                journal_path: dist.join(JOURNAL),
                slug: slug.into(),
                version: "0.191.0".into(),
                tag: "v0.191.0".into(),
                build: 191,
                commit: "a".repeat(40),
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
                notes_section: "Unreleased".into(),
                journal: None,
                adopted: false,
            };
            let release = crate::release::Journal {
                version: ctx.version.clone(),
                commit: "d".repeat(40),
                reserved: Some(crate::release::Reservation {
                    build: ctx.build,
                    commit: ctx.commit.clone(),
                    line: "191 0.191.0".into(),
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
                _route: route,
            }
        }

        fn stage(&mut self) -> Result<()> {
            let slug = self.ctx.slug.clone();
            channel::stage_on_channel(&mut NetworkOnly(LiveChannelRelease {
                ctx: &mut self.ctx,
                slug: &slug,
                bound: None,
            }))
        }

        fn publish(&mut self) -> Result<()> {
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

        fn posts(&self) -> usize {
            self.fake
                .seen()
                .iter()
                .filter(|r| r.method == "POST")
                .count()
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
        let latest =
            crate::github::get(&format!("repos/{}/releases/latest", lab.ctx.slug)).unwrap();
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
    fn refusal_after_draft_removes_only_the_owned_private_release() {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        let id = lab.ctx.release_id.unwrap();
        discard(&lab.ctx.dist, &lab.ctx.version, lab.ctx.build).unwrap();
        assert!(lab.fake.release_state(id).is_none());
        assert_eq!(lab.fake.release_ids(&lab.ctx.slug).len(), 1);
        assert!(!lab.ctx.journal_path.exists());
        assert!(!lab.fake.seen().iter().any(|r| r.method == "PATCH"));
        lab.fake.assert_clean();
    }

    #[test]
    fn matching_resume_adopts_without_another_upload_and_mismatch_refuses() {
        let mut lab = Lab::new();
        lab.stage().unwrap();
        let posts = lab.posts();
        let saved = Journal::load(&lab.ctx.journal_path).unwrap().unwrap();
        lab.ctx.release_id = saved.release_id;
        lab.ctx.release_intent = saved.release_intent;
        lab.ctx.upload_intents = saved.upload_intents;
        lab.stage().unwrap();
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
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The private half of a cut. Its journal precedes every POST; the claim adopts
//! exactly these bytes, and only the claimed channel path can make them public.

use super::*;

/// The private draft's journal in `dist/`, the file the engine's `--draft-state` reads.
pub(super) const JOURNAL: &str = "draft-state.toml";
const RECEIPT: &str = "draft-bytes.toml";

/// The private draft journal names build `build` of `version`: that draft owns the build's
/// signed bytes. A journal of another build or version owns nothing of this one.
pub(super) fn has_journal_for(dist: &Path, version: &str, build: u64) -> Result<bool> {
    Ok(Journal::load(&dist.join(JOURNAL))?
        .is_some_and(|draft| draft.version == version && draft.build_number == build))
}

#[derive(Serialize, Deserialize)]
struct Bytes {
    files: Vec<FileDigest>,
}

#[derive(Serialize, Deserialize)]
struct FileDigest {
    name: String,
    sha256: String,
}

/// The clone's release journal (`release.toml`), which holds the reservation a draft serves.
fn release_journal(repo: &Path) -> Result<Option<crate::release::Journal>> {
    let git = GitCli::new(repo);
    let path = crate::release::release_dir(&git)
        .map_err(|e| Error::new(e.to_string()))?
        .join(crate::release::JOURNAL_FILE);
    crate::release::Journal::load(&path).map_err(|e| Error::new(e.to_string()))
}

fn reserves(release: &crate::release::Journal, commit: &str, build: u64) -> bool {
    release
        .reserved
        .as_ref()
        .is_some_and(|r| r.commit == commit && r.build == build)
}

pub(super) fn authority(repo: &Path, commit: &str, build: u64) -> Result<()> {
    let release = release_journal(repo)?
        .ok_or_else(|| Error::new("private upload has no release journal"))?;
    if !reserves(&release, commit, build) {
        return Err(Error::new(
            "private upload's reservation no longer belongs to this release",
        ));
    }
    Ok(())
}

/// Why [`discard`] left the journaled draft where it is.
#[derive(Debug)]
pub(super) enum Kept {
    /// A read, a DELETE or a file failed, or GitHub's reads disagree on whether a release whose
    /// DELETE was journaled is gone; a retry may get through.
    Failed(Error),
    /// Decided, and a retry decides the same: this clone cannot prove the draft its own, or cannot
    /// see what its create POST made. A person does what `remedy` says.
    Held { why: String, remedy: String },
}

impl From<Error> for Kept {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
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
        delete_intent: None,
        done: Vec::new(),
    }
}

fn agrees(ctx: &CutCtx, draft: &Journal) -> Result<()> {
    if let Some(id) = draft.delete_intent {
        return Err(Error::new(format!(
            "the private draft dist/{JOURNAL} journals ({}) is being withdrawn: the DELETE of \
             release {id} was issued, so no cut adopts it — {}",
            journaled_words(draft),
            withdraw_remedy(&ctx.repo, &ctx.slug, draft)
        )));
    }
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
        return Err(Error::new(format!(
            "the private draft dist/{JOURNAL} journals ({}) differs from this cut (v{} build {} \
             at {}) in identity, floor or signer; refusing adoption — {}",
            journaled_words(draft),
            ctx.version,
            ctx.build,
            short_commit(&ctx.commit),
            withdraw_remedy(&ctx.repo, &ctx.slug, draft)
        )));
    }
    Ok(())
}

/// The journaled draft as a person finds it: version, build, reservation commit, release id.
fn journaled_words(draft: &Journal) -> String {
    let release = draft
        .release_id
        .map_or_else(|| "no release id".to_string(), |id| format!("release {id}"));
    format!(
        "v{} build {} at {}, {release}",
        draft.version,
        draft.build_number,
        short_commit(&draft.commit)
    )
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// How `draft` leaves this clone. While release.toml still reserves its build and commit, the
/// release's `--forget` withdraws it under that reservation ([`discard`]). Once the reservation is
/// gone nothing can prove the draft this clone's, so a person deletes the release and the journal.
fn withdraw_remedy(repo: &Path, slug: &str, draft: &Journal) -> String {
    let forgets = release_journal(repo).ok().flatten().is_some_and(|release| {
        release.version == draft.version && reserves(&release, &draft.commit, draft.build_number)
    });
    if forgets {
        return format!(
            "`{} --forget` withdraws it with its reservation, then `{}` reserves anew",
            crate::release::RELEASE_COMMAND,
            crate::release::RELEASE_COMMAND
        );
    }
    format!(
        "release.toml no longer reserves its build, so no tool can prove the draft this clone's: \
         {}",
        removal(slug, draft.release_id)
    )
}

/// A person's removal of a private draft no tool withdraws: its release, where the journal names
/// one, then the journal and its receipt.
fn removal(slug: &str, release_id: Option<u64>) -> String {
    let release = release_id.map_or_else(String::new, |id| {
        format!(
            "delete its draft release ({}), then ",
            delete_command(slug, id)
        )
    });
    format!("{release}remove dist/{JOURNAL} and dist/{RECEIPT}")
}

/// The `agh` write that deletes release `id` of `slug`.
pub(super) fn delete_command(slug: &str, id: impl std::fmt::Display) -> String {
    let path = format!("repos/{slug}/releases/{id}");
    format!("agh api {path} -X DELETE --confirm {path}")
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

/// The release id this clone's private-draft journal names for build `build` of `version`, when
/// it names one; a journal of another build or version names none of this one's.
pub(super) fn journaled_release(dist: &Path, version: &str, build: u64) -> Result<Option<u64>> {
    Ok(Journal::load(&dist.join(JOURNAL))?
        .filter(|draft| draft.version == version && draft.build_number == build)
        .and_then(|draft| draft.release_id))
}

/// Withdraw this journal's exact draft and forget the journal. The draft is found by the id the
/// journal binds, or, where its create POST was issued and no id bound, as the tag's one release.
/// A journal with no create POST withdraws nothing on GitHub. The DELETE is journaled
/// (`delete_intent`) before it goes out, so a retry reads that release's absence (its id
/// answering 404 and the releases listing, drafts included, without it) as this withdrawal's own;
/// the receipt goes before the journal, so no journal leaves no receipt (one a withdrawal cut
/// short left goes too). Held, a retry deciding the same: a journal this cutter cannot read,
/// another reservation's draft, a reservation release.toml no longer holds, a bound id GitHub no
/// longer answers and this clone never journaled deleting, a release that is public or under
/// another tag, a create POST with no release or several under the tag. A read, DELETE, journal
/// write or file removal that fails, and a journaled DELETE's release that answers 404 by its id
/// while the listing still holds it, are [`Kept::Failed`].
pub(super) fn discard(dist: &Path, version: &str, build: u64) -> std::result::Result<(), Kept> {
    let path = dist.join(JOURNAL);
    let text = match read_journal_text(&path) {
        Ok(Some(text)) => Ok(text),
        Ok(None) => {
            forget_copy_record(&dist.join(RECEIPT))?;
            return Ok(());
        }
        Err(JournalUnread::Read(error)) => return Err(Kept::Failed(error)),
        Err(JournalUnread::Invalid(error)) => Err(error),
    };
    let repo = dist
        .parent()
        .ok_or_else(|| Error::new("draft has no repository"))?;
    let slug = workspace_channel_slug(repo)?;
    let tag = format!("v{version}");
    let unreadable = |error: Error| Kept::Held {
        why: format!("dist/{JOURNAL} cannot be read as a private-draft journal: {error}"),
        remedy: format!(
            "delete by hand any draft of {tag} this clone made on {slug}, then {}",
            removal(&slug, None)
        ),
    };
    let draft = text
        .and_then(|text| Journal::from_text(&text, &path))
        .map_err(unreadable)?;
    if draft.version != version || draft.build_number != build {
        return Err(Kept::Held {
            why: format!(
                "dist/{JOURNAL} journals another reservation's private draft ({}), not v{version} \
                 build {build}",
                journaled_words(&draft)
            ),
            remedy: withdraw_remedy(repo, &slug, &draft),
        });
    }
    if !release_journal(repo)?
        .is_some_and(|release| reserves(&release, &draft.commit, draft.build_number))
    {
        return Err(Kept::Held {
            why: format!(
                "release.toml does not reserve the private draft's build ({})",
                journaled_words(&draft)
            ),
            remedy: withdraw_remedy(repo, &slug, &draft),
        });
    }
    let release = match draft.release_id.or(draft.delete_intent) {
        Some(id) => match release_object_by_id(&slug, id)? {
            Some(release) => Some(release),
            // This clone journaled its DELETE of `id` before issuing it, so the DELETE may never
            // have gone out (the run stopped after the journal, or the guard's first read
            // failed): the release is gone only where the releases listing, drafts included,
            // agrees with the 404. One it still lists is a read that missed it, or a deletion
            // GitHub has not settled, and a retry decides.
            None if draft.delete_intent == Some(id) => {
                if release_listed(&slug, id)? {
                    return Err(Kept::Failed(Error::new(format!(
                        "GitHub answers no release {id} by its ID, yet {slug}'s releases listing \
                         still lists release {id}, whose DELETE dist/{JOURNAL} journals"
                    ))));
                }
                None
            }
            None => {
                return Err(Kept::Held {
                    why: format!(
                        "GitHub answers no release {id}, the private draft dist/{JOURNAL} binds"
                    ),
                    remedy: format!(
                        "once {slug}'s releases page shows no draft of {tag} this clone made, {}",
                        removal(&slug, None)
                    ),
                });
            }
        },
        None if !draft.release_intent => None,
        None => match release_objects_by_tag(&slug, &tag)?.as_slice() {
            [] => {
                return Err(Kept::Held {
                    why: format!(
                        "dist/{JOURNAL} journals a create POST of {tag}'s private draft, and no \
                         release under {tag} is visible on {slug}, so whether it created one is \
                         undecided"
                    ),
                    remedy: format!(
                        "if {slug}'s releases page, drafts included, shows no release for {tag}, \
                         the POST created nothing: {} (a draft that appears there is withdrawn by \
                         the next run)",
                        removal(&slug, None)
                    ),
                });
            }
            [release] => Some(release.clone()),
            several => {
                let mut ids: Vec<u64> = several.iter().map(|r| r.id).collect();
                ids.sort_unstable();
                let ids: Vec<String> = ids.iter().map(u64::to_string).collect();
                return Err(Kept::Held {
                    why: format!(
                        "{tag} names {} releases on {slug} (IDs {}) and dist/{JOURNAL} binds none \
                         of them, so which is this clone's draft is undecided",
                        several.len(),
                        ids.join(", ")
                    ),
                    remedy: format!(
                        "delete by hand each that is not this clone's draft ({}), so its own is \
                         the one left",
                        delete_command(&slug, "<id>")
                    ),
                });
            }
        },
    };
    if let Some(release) = release {
        if !release.draft || release.tag != tag {
            let state = if release.draft {
                format!("now a draft under {}", release.tag)
            } else {
                "public".to_string()
            };
            return Err(Kept::Held {
                why: format!(
                    "release {} on {slug}, the private draft of {tag} this clone journaled, is \
                     {state}, and only this clone's unpublished draft of its tag is withdrawn",
                    release.id
                ),
                remedy: format!(
                    "decide release {} by hand; once it no longer serves this clone's cut, {}",
                    release.id,
                    removal(&slug, None)
                ),
            });
        }
        if draft.delete_intent != Some(release.id) {
            let mut withdrawing = draft.clone();
            withdrawing.delete_intent = Some(release.id);
            withdrawing.save(&path)?;
        }
        delete_release_object_by_id_with_guard(
            &slug,
            &release,
            false,
            || authority(repo, &draft.commit, build),
            || authority(repo, &draft.commit, build),
        )?;
    }
    forget_copy_record(&dist.join(RECEIPT))?;
    forget_copy_record(&path)?;
    Ok(())
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
#[path = "draft_tests.rs"]
pub(crate) mod tests;

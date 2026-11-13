// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact signed bytes for an interrupted package-index publication. These are
//! retained inputs, not a replacement for native publication admission.

use super::*;
use crate::api::Delta;
use crate::channel::Log;
use crate::channel::index::{self, Base, PreparedPublication, PublicationObserver};
use crate::quad::{self, Freshness, QUAD_FILES, VerifiedQuad};

const QUAD_CAP: u64 = 16 * 1024 * 1024;

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema: u32,
    intent: String,
    build: u64,
    quad: [String; 4],
    predecessor_build: u64,
    predecessor: [String; 4],
}

/// Exact retained candidate and predecessor. Loading verifies their signatures
/// and frozen package delta; this object grants no publication authority.
pub struct RetainedIndex {
    candidate: VerifiedQuad,
    predecessor: VerifiedQuad,
}

impl RetainedIndex {
    /// Exact candidate files retained before publication was attempted.
    #[must_use]
    pub const fn candidate(&self) -> &VerifiedQuad {
        &self.candidate
    }

    /// Exact predecessor files against which that candidate was qualified.
    #[must_use]
    pub const fn predecessor(&self) -> &VerifiedQuad {
        &self.predecessor
    }
}

/// The campaign's native pre-publication observer, held through the call to the
/// publisher. A retention failure stops both public mirroring and index writes.
pub struct Retention<'a> {
    campaign: &'a mut Campaign,
    ctx: &'a Ctx,
    retained: Option<RetainedIndex>,
}

struct Recheck<'a> {
    campaign: &'a Campaign,
    ctx: &'a Ctx,
}

impl PublicationObserver for Recheck<'_> {
    fn before_publish(&mut self, prepared: &PreparedPublication<'_>) -> Result<()> {
        let retained = self.campaign.retained_index(self.ctx)?.ok_or_else(|| {
            Error::refused("the retained index attempt disappeared during qualification")
        })?;
        if index::quad_digests(prepared.candidate())? != index::quad_digests(&retained.candidate)?
            || prepared
                .predecessor()
                .map(index::quad_digests)
                .transpose()?
                != Some(index::quad_digests(&retained.predecessor)?)
        {
            return Err(Error::refused(
                "the retained attempt changed during qualification",
            ));
        }
        Ok(())
    }
}

impl Retention<'_> {
    /// Release the campaign writer borrow and return the retained, verified bytes.
    #[must_use]
    pub fn into_retained(self) -> Option<RetainedIndex> {
        self.retained
    }
}

impl Campaign {
    /// Resume the exact retained candidate through fresh native qualification,
    /// public-head checks, publication guards and anonymous readback. A newer
    /// channel head is refused; this never signs or repins a replacement.
    /// # Errors
    /// Missing retention, changed inputs, failed native gates or publication.
    pub fn resume_index(&mut self, ctx: &Ctx, log: &mut Log) -> Result<index::Outcome> {
        let retained = self
            .retained_index(ctx)?
            .ok_or_else(|| Error::refused("the package campaign has no retained index attempt"))?;
        let mut delta = Delta::default();
        delta
            .pins
            .insert(self.inputs.program.clone(), self.inputs.build);
        let request = crate::api::IndexRequest {
            baseline: crate::api::Baseline::Dir(retained.predecessor.dir.clone()),
            delta,
            out: retained.candidate.dir.clone(),
            upload: true,
            pack_dir: None,
        };
        self.check()?;
        let outcome = index::resume_exact(
            ctx,
            &request,
            &retained.predecessor.dir,
            log,
            &mut Recheck {
                campaign: self,
                ctx,
            },
        )?;
        self.record(Observation::IndexPublished {
            build: outcome.published.index_build,
            quad: index::quad_digests(&retained.candidate)?,
        })?;
        Ok(outcome)
    }

    /// Retain one native publication attempt before any public side effect.
    #[must_use]
    pub const fn retention<'a>(&'a mut self, ctx: &'a Ctx) -> Retention<'a> {
        Retention {
            campaign: self,
            ctx,
            retained: None,
        }
    }

    /// Reopen the last exact candidate after a lost acknowledgement. Its record
    /// remains prepared until actual public readback proves publication.
    /// # Errors
    /// Changed destinations, damaged bytes, signature failures or a different delta.
    pub fn retained_index(&self, ctx: &Ctx) -> Result<Option<RetainedIndex>> {
        let entries = self.read_entries()?;
        let Some((sequence, binding)) =
            entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(sequence, entry)| match &entry.observation {
                    Observation::IndexPrepared {
                        build,
                        quad,
                        predecessor_build,
                        predecessor,
                    } => Some((
                        sequence,
                        Binding {
                            schema: 1,
                            intent: self.intent.clone(),
                            build: *build,
                            quad: quad.clone(),
                            predecessor_build: *predecessor_build,
                            predecessor: predecessor.clone(),
                        },
                    )),
                    _ => None,
                })
        else {
            return Ok(None);
        };
        let retained = self.load_index(ctx, &binding)?;
        // A previous process can have observed a failed directory flush after
        // the name appeared. Finish that same receipt before a caller proceeds.
        self.flush(&receipt_name(sequence))?;
        Ok(Some(retained))
    }

    fn load_index(&self, ctx: &Ctx, expected: &Binding) -> Result<RetainedIndex> {
        self.check()?;
        destinations(ctx, &self.inputs)?;
        let root = snapshot_path(&self.root, expected);
        let bytes = private_file::read_private(&root, "binding.json", MAX_BYTES)
            .map_err(|error| Error::refused(format!("read retained publication: {error}")))?;
        let binding: Binding = aterm_json::from_slice(&bytes)
            .map_err(|error| Error::refused(format!("decode retained publication: {error}")))?;
        if binding != *expected {
            return Err(Error::refused(
                "retained publication belongs to another attempt",
            ));
        }
        for child in ["candidate", "predecessor"] {
            private_file::check_private_dir(&root.join(child))
                .map_err(|error| Error::io("check retained publication directory", &error))?;
        }
        let candidate = quad::verify_dir(ctx, &root.join("candidate"))?;
        let predecessor = quad::verify_dir_with(ctx, &root.join("predecessor"), Freshness::Ignore)?;
        if candidate.index.index_build != binding.build
            || predecessor.index.index_build != binding.predecessor_build
            || index::quad_digests(&candidate)? != binding.quad
            || index::quad_digests(&predecessor)? != binding.predecessor
        {
            return Err(Error::refused("retained signed publication changed"));
        }
        package_plan(&self.inputs, &candidate, &predecessor)?;
        self.check()?;
        Ok(RetainedIndex {
            candidate,
            predecessor,
        })
    }
}

impl PublicationObserver for Retention<'_> {
    fn before_publish(&mut self, prepared: &PreparedPublication<'_>) -> Result<()> {
        self.campaign.check()?;
        destinations(self.ctx, &self.campaign.inputs)?;
        if prepared.public_owner() != self.campaign.inputs.public_owner
            || prepared.staging_owner() != self.campaign.inputs.staging_owner
            || !prepared.request().upload
            || prepared.request().pack_dir.is_some()
        {
            return Err(Error::refused(
                "publication destinations differ from the campaign",
            ));
        }
        let predecessor = prepared.predecessor().ok_or_else(|| {
            Error::refused("package campaigns require an existing public channel")
        })?;
        package_plan(&self.campaign.inputs, prepared.candidate(), predecessor)?;
        let binding = Binding {
            schema: 1,
            intent: self.campaign.intent.clone(),
            build: prepared.candidate().index.index_build,
            quad: index::quad_digests(prepared.candidate())?,
            predecessor_build: predecessor.index.index_build,
            predecessor: index::quad_digests(predecessor)?,
        };
        let root = snapshot_path(&self.campaign.root, &binding);
        directory(&root)?;
        for (name, source) in [
            ("candidate", prepared.candidate()),
            ("predecessor", predecessor),
        ] {
            let destination = root.join(name);
            directory(&destination)?;
            for file in QUAD_FILES {
                let bytes = crate::identity::read_regular_bytes(&source.file(file), QUAD_CAP)
                    .map_err(|error| Error::io("read signed publication input", &error))?;
                keep(&destination, file, &bytes)?;
            }
        }
        keep(
            &root,
            "binding.json",
            &aterm_json::to_vec(&binding).map_err(|error| Error::refused(error.to_string()))?,
        )?;
        let retained = self.campaign.load_index(self.ctx, &binding)?;
        self.campaign.record(Observation::IndexPrepared {
            build: binding.build,
            quad: binding.quad,
            predecessor_build: binding.predecessor_build,
            predecessor: binding.predecessor,
        })?;
        self.retained = Some(retained);
        Ok(())
    }
}

fn destinations(ctx: &Ctx, intent: &Intent) -> Result<()> {
    if ctx.accounts.public != intent.public_owner || ctx.accounts.staging != intent.staging_owner {
        return Err(Error::refused("package campaign destinations changed"));
    }
    Ok(())
}

fn package_plan(
    intent: &Intent,
    candidate: &VerifiedQuad,
    predecessor: &VerifiedQuad,
) -> Result<()> {
    let mut delta = Delta::default();
    // ALab package rows use the global pin; the client routes only their
    // declared targets to ALab and leaves the other targets on the vendor lane.
    delta.pins.insert(intent.program.clone(), intent.build);
    let plan = index::plan(
        &Base::of_head(predecessor)?,
        &delta,
        candidate
            .index
            .machine_id
            .as_deref()
            .ok_or_else(|| Error::refused("candidate names no signer"))?,
        candidate
            .index
            .roster_seq
            .ok_or_else(|| Error::refused("candidate names no roster"))?,
        &candidate.index.valid_until,
        &mut Log::capture("package campaign"),
    )?;
    let expected: aterm_toml::Value = index::render(&plan)
        .parse()
        .map_err(|error| Error::refused(format!("decode package plan: {error}")))?;
    let bytes = crate::identity::read_regular_text(&candidate.file(QUAD_FILES[0]), QUAD_CAP)
        .map_err(|error| Error::io("read retained index plan", &error))?;
    let actual: aterm_toml::Value = bytes
        .parse()
        .map_err(|error| Error::refused(format!("decode retained index plan: {error}")))?;
    if actual != expected {
        return Err(Error::refused(
            "candidate changes more than the frozen package pin",
        ));
    }
    Ok(())
}

fn snapshot_path(root: &Path, binding: &Binding) -> PathBuf {
    let digest = atpkg::sha256_bytes(binding.quad.join(":").as_bytes());
    root.join(format!("index-{}-{digest}", binding.build))
}

pub(super) fn directory(path: &Path) -> Result<()> {
    private_file::create_private_dir(path)
        .and_then(|()| aterm_durable::Flush::Durable.dir(path.parent().unwrap_or(path)))
        .map_err(|error| Error::io("retain publication directory", &error))
}

fn keep(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    private_file::write_private_new(root, name, bytes, |_| Ok(()))
        .map_err(|error| Error::io("retain signed publication bytes", &error))?;
    let actual = private_file::read_private(root, name, QUAD_CAP as usize)
        .map_err(|error| Error::refused(format!("read signed publication bytes: {error}")))?;
    if actual != bytes {
        return Err(Error::refused(
            "retained publication bytes belong to another attempt",
        ));
    }
    let (file, _) = private_file::open_private(root, name)
        .map_err(|error| Error::refused(format!("finish retained publication: {error}")))?;
    aterm_durable::Flush::Durable
        .file(&file)
        .and_then(|()| aterm_durable::Flush::Durable.dir(root))
        .map_err(|error| Error::io("finish retained publication", &error))
}

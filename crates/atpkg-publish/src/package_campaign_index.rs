// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact signed bytes for an interrupted package-index publication. These are
//! retained inputs, not a replacement for native publication admission.

use super::*;
use crate::api::Delta;
use crate::channel::index::{self, Base, PreparedPublication, PublicationObserver};
use crate::channel::{Log, Scratch};
use crate::quad::{self, Freshness, QUAD_FILES, VerifiedQuad, index_tag};

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

/// Where a campaign's index attempt stands against the public channel.
pub(super) enum Standing {
    /// No live attempt: plan a candidate over the channel head.
    Plan,
    /// The retained candidate can still publish exactly as signed.
    Resume,
    /// The channel serves the campaign's index at this number: nothing is left to write.
    Published(u64),
}

/// The live attempt the receipts name, before any external state is consulted.
struct Attempt {
    /// The sequence of the receipt that retained it.
    sequence: usize,
    /// The retained candidate and predecessor binding.
    binding: Binding,
    /// A later receipt recorded the candidate's public readback.
    published: bool,
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
    /// remains prepared until actual public readback proves publication, and a
    /// retired candidate is no longer retained.
    /// # Errors
    /// Changed destinations, damaged bytes, signature failures or a different delta.
    pub fn retained_index(&self, ctx: &Ctx) -> Result<Option<RetainedIndex>> {
        Ok(self.retained(ctx)?.map(|(retained, _)| retained))
    }

    fn retained(&self, ctx: &Ctx) -> Result<Option<(RetainedIndex, bool)>> {
        let entries = self.read_entries()?;
        let attempt = self.attempt(&entries)?;
        let retained = attempt
            .as_ref()
            .map(|attempt| self.load_index(ctx, &attempt.binding))
            .transpose()?;
        // A previous process can have observed a failed directory flush after
        // the name appeared. Finish the receipts that decide this attempt, its
        // retention and the last index receipt, before a caller proceeds.
        let last = entries.iter().rposition(|entry| {
            matches!(
                entry.observation,
                Observation::IndexPrepared { .. }
                    | Observation::IndexPublished { .. }
                    | Observation::IndexRetired { .. }
            )
        });
        let mut decided: Vec<usize> = attempt
            .as_ref()
            .map(|attempt| attempt.sequence)
            .into_iter()
            .collect();
        decided.extend(last.filter(|last| !decided.contains(last)));
        for sequence in decided {
            self.flush(&receipt_name(sequence))?;
        }
        Ok(retained.zip(attempt.map(|attempt| attempt.published)))
    }

    /// The last retained candidate no retirement followed. Every later index
    /// receipt must name that same candidate.
    fn attempt(&self, entries: &[Entry]) -> Result<Option<Attempt>> {
        let mut closed: Option<&Observation> = None;
        for (sequence, entry) in entries.iter().enumerate().rev() {
            match &entry.observation {
                Observation::IndexPrepared {
                    build,
                    quad,
                    predecessor_build,
                    predecessor,
                } => {
                    let published = match closed {
                        None => false,
                        Some(Observation::IndexPublished { build: b, quad: q })
                            if b == build && q == quad =>
                        {
                            true
                        }
                        Some(Observation::IndexRetired {
                            build: b, quad: q, ..
                        }) if b == build && q == quad => return Ok(None),
                        Some(_) => {
                            return Err(Error::refused(
                                "package campaign index receipts name another attempt than the one they follow",
                            ));
                        }
                    };
                    return Ok(Some(Attempt {
                        sequence,
                        binding: Binding {
                            schema: 1,
                            intent: self.intent.clone(),
                            build: *build,
                            quad: quad.clone(),
                            predecessor_build: *predecessor_build,
                            predecessor: predecessor.clone(),
                        },
                        published,
                    }));
                }
                later @ (Observation::IndexPublished { .. } | Observation::IndexRetired { .. }) => {
                    if closed.is_some_and(|closing| closing != later) {
                        return Err(Error::refused(
                            "package campaign index receipts disagree about one attempt",
                        ));
                    }
                    closed = Some(later);
                }
                _ => {}
            }
        }
        if closed.is_some() {
            return Err(Error::refused(
                "a package campaign index receipt follows no retained attempt",
            ));
        }
        Ok(None)
    }

    /// Judge the campaign's index attempt against the public channel. A receipt
    /// never stands in for that: a recorded publication is read back again, and an
    /// open attempt is retired only where the channel can never serve it, which
    /// the controller records before planning afresh. A plan, the first or one
    /// after a retirement, starts only over a head under which the frozen pin can
    /// install, before anything is staged for it.
    /// # Errors
    /// A channel that cannot be read, that lost published indexes or no longer
    /// serves this campaign's recorded one, or whose head supersedes or withdrew the
    /// frozen build; or a damaged retained attempt.
    pub(super) fn standing(&mut self, ctx: &Ctx, log: &mut Log) -> Result<Standing> {
        let Some((retained, published)) = self.retained(ctx)? else {
            self.admit_over_head(ctx)?;
            return Ok(Standing::Plan);
        };
        let build = retained.candidate.index.index_build;
        let predecessor = retained.predecessor.index.index_build;
        let tag = index_tag(build);
        let slug = ctx.index_slug();
        if published {
            index::confirm_public_quad(ctx, &retained.candidate).map_err(|error| match error {
                Error::Refused(why) => Error::refused(format!(
                    "{slug} no longer serves this campaign's published {tag} as recorded: {why}. \
                     The channel replaced a published index; look at it. To pin the package \
                     again, publish it as a new campaign under a new --state"
                )),
                other => other,
            })?;
            log.say(format!(
                "{tag} on {slug} serves this campaign's signed index, as recorded: nothing to publish"
            ));
            return Ok(Standing::Published(build));
        }
        let quad = index::quad_digests(&retained.candidate)?;
        let head = quad::head_build(ctx, &slug)
            .map_err(|error| {
                Error::cannot_run(format!(
                    "could not read the head of {slug} to settle the retained {tag}: {error} — \
                     re-run once it answers"
                ))
            })?
            .ok_or_else(|| {
                Error::refused(format!(
                    "{slug} serves no published index, though this campaign's retained {tag} \
                     was signed over {}: look at the channel before re-running",
                    index_tag(predecessor)
                ))
            })?;
        let why = if head >= build {
            match index::served(ctx, &retained.candidate)? {
                index::Served::Exactly => {
                    self.record(Observation::IndexPublished { build, quad })?;
                    log.say(format!(
                        "{tag} on {slug} serves this campaign's retained candidate: it was \
                         published and is read back; nothing to publish"
                    ));
                    return Ok(Standing::Published(build));
                }
                index::Served::Other => format!("another index holds {tag}"),
                index::Served::Absent if head > build => {
                    format!("the head {} moved past {tag}", index_tag(head))
                }
                index::Served::Absent => {
                    return Err(Error::cannot_run(format!(
                        "{slug} lists {tag} as its head, but the anonymous download route \
                         serves none of it yet — re-run once it does"
                    )));
                }
            }
        } else if head == predecessor {
            if retained.candidate.lapsed(ctx.now_unix) {
                format!(
                    "its signed horizon {} lapsed, so no client would accept it",
                    retained.candidate.index.valid_until
                )
            } else {
                // The candidate is bound to its predecessor's exact files, so a channel that
                // replaced them can never take it, however long its horizon runs.
                let predecessor_tag = index_tag(predecessor);
                match index::served(ctx, &retained.predecessor)? {
                    index::Served::Exactly => return Ok(Standing::Resume),
                    index::Served::Other => {
                        format!("the channel replaced {predecessor_tag}, over which it was signed")
                    }
                    index::Served::Absent => {
                        return Err(Error::cannot_run(format!(
                            "{slug} lists {predecessor_tag} as its head, but the anonymous \
                             download route serves none of it — re-run once it does"
                        )));
                    }
                }
            }
        } else {
            return Err(Error::refused(format!(
                "the public head {} of {slug} is below {}, over which this campaign's retained \
                 {tag} was signed: published indexes left the channel. Look at it before \
                 re-running",
                index_tag(head),
                index_tag(predecessor)
            )));
        };
        self.record(Observation::IndexRetired {
            build,
            quad,
            head_build: head,
        })?;
        log.warn(format!(
            "retired the retained {tag}: {why}; the frozen pin is planned afresh over the \
             channel head where that head admits it"
        ));
        self.admit_over_head(ctx)?;
        Ok(Standing::Plan)
    }

    /// Refuse a channel head under which the frozen pin can never install, before
    /// the campaign stages or signs anything over it. The horizon is left to the
    /// native plan, whose refusal of a lapsed head names `--valid-until`.
    fn admit_over_head(&self, ctx: &Ctx) -> Result<()> {
        let scratch = Scratch::new("campaign-head")?;
        let (_, head) =
            quad::fetch_head_with(ctx, &ctx.index_slug(), scratch.path(), Freshness::Ignore)?;
        admit_frozen_pin(&self.inputs, &head)
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
        // The candidate's own horizon is judged where it matters: the campaign
        // retires a lapsed attempt the channel does not serve, resumption enforces
        // the horizon again, and an index already published stays published.
        let candidate = quad::verify_dir_with(ctx, &root.join("candidate"), Freshness::Ignore)?;
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
        // The head this candidate was planned over, which can have moved since the
        // campaign judged the channel: the native plan only warns about a yank.
        admit_frozen_pin(&self.campaign.inputs, predecessor)?;
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

/// Refuse a head under which the frozen pin can never install. A pin above it
/// supersedes the package, since an index never lowers a pin; a yank of it or a
/// floor above it withdrew the build, since a client takes no ALab pin its channel
/// yanks or holds below its floor (`atpkg::alab::channel_pin`): it keeps the ALab
/// build it has or routes the program to the vendor, so the pin reaches nobody.
fn admit_frozen_pin(intent: &Intent, head: &VerifiedQuad) -> Result<()> {
    let channel = Base::of_head(head)?.channel;
    let (program, build) = (intent.program.as_str(), intent.build);
    let tag = index_tag(head.index.index_build);
    if let Some(&pinned) = channel.pin.get(program)
        && pinned > build
    {
        return Err(Error::refused(format!(
            "{tag} pins {program} {pinned}, above this campaign's frozen {build}: the package \
             is superseded, and an index never lowers a pin, so no campaign publishes it. A \
             newer build is a new campaign under a new --state"
        )));
    }
    if atpkg::gate::is_yanked(&channel, program, build) {
        return Err(Error::refused(format!(
            "{tag} yanks {program}@{build}: no client installs a yanked ALab pin, so an index \
             pinning it would reach nobody, and no campaign pins it. Publish a fixed build as a \
             new campaign under a new --state; if the yank was wrong, lift it first \
             (`atpkg-publish index --unyank {program}@{build} --upload`), then re-run"
        )));
    }
    let floor = channel.min_build_for(program);
    if build < floor {
        return Err(Error::refused(format!(
            "{tag} holds {program} at a floor of {floor}, above this campaign's frozen \
             {build}: no client installs an ALab pin below its floor, so an index pinning it \
             would reach nobody, and no campaign pins it. A build at or above {floor} is a new \
             campaign under a new --state"
        )));
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

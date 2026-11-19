// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Freeze the two Codex packages a source controller will check. An expectation
//! binds its later publish to those inputs; it supplies no execution verdict.

use super::*;
use crate::artifact::package::{PackageOutcome, PackageRequest};
use crate::artifact::tools::Tools;
use crate::artifact::{pack, pkgdoc, upload};
use crate::channel::{Log, Scratch, index::PreparedPublication};
use crate::host::Probe;
use crate::quad::{self, VerifiedQuad};

const INPUT: &str = "codex-protocol-input.json";
const PROGRAM: &str = "codex";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    build: u64,
    version: String,
    source: String,
    manifest_sha256: String,
    tree_root: String,
    executable_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Relation {
    First,
    Upgrade,
    AlreadySelected,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    schema: u32,
    target: String,
    predecessor: Option<Endpoint>,
    successor: Endpoint,
    candidate: PathBuf,
    relation: Relation,
}

struct Manifest {
    pkg: atpkg::PkgManifest,
    digest: String,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    crate::identity::read_regular_bytes(path, crate::identity::MAX_META_BYTES)
        .map_err(|error| Error::io("read Codex protocol input", &error))
}

fn executable_hash(path: &Path) -> Result<String> {
    atpkg::sha256_file(path).map_err(|error| Error::io("hash Codex protocol executable", &error))
}

fn pin(quad: &VerifiedQuad, repo: &str, target: &str) -> Result<Option<u64>> {
    let channel = quad
        .index
        .channel_for(atpkg::config::CHANNEL, target)
        .ok_or_else(|| {
            Error::refused("the signed index has no stable channel for the Codex target")
        })?;
    let Some(&build) = channel.pin.get(PROGRAM) else {
        return Ok(None);
    };
    let row = quad
        .index
        .program(PROGRAM)
        .ok_or_else(|| Error::refused("the signed index pins Codex without a program row"))?;
    Ok((row.repo == repo).then_some(build))
}

fn public_file(
    ctx: &Ctx,
    repo: &str,
    tag: &str,
    name: &str,
    output: &Path,
    cap: u64,
) -> Result<()> {
    let slug = ctx.public_slug(repo);
    match ctx
        .hosts
        .channel
        .fetch_public(&slug, tag, name, output, cap)
    {
        Probe::Present => Ok(()),
        other => Err(Error::cannot_run(format!(
            "cannot read the predecessor from {slug}/{tag}/{name}: {other:?}"
        ))),
    }
}

impl Manifest {
    fn from_files(
        manifest: &Path,
        signature: &Path,
        build: u64,
        verify: impl FnOnce(&[u8], &[u8]) -> Result<atpkg::PkgManifest>,
    ) -> Result<Self> {
        let raw = read(manifest)?;
        let pkg = verify(&raw, &read(signature)?)?;
        if !pkg.is_for(PROGRAM) || pkg.build_number != build {
            return Err(Error::refused(
                "the signed Codex package does not name the selected build",
            ));
        }
        Ok(Self {
            pkg,
            digest: atpkg::sha256_bytes(&raw),
        })
    }

    fn fetch(
        ctx: &Ctx,
        quad: &VerifiedQuad,
        repo: &str,
        build: u64,
        dir: &Path,
        public: bool,
    ) -> Result<Self> {
        let mf = pkgdoc::manifest_name(PROGRAM, build);
        let sig = pkgdoc::sig_name(PROGRAM, build);
        let tag = upload::tag(PROGRAM, build);
        if public {
            public_file(
                ctx,
                repo,
                &tag,
                &mf,
                &dir.join(&mf),
                atpkg::net::MANIFEST_CAP,
            )?;
            public_file(ctx, repo, &tag, &sig, &dir.join(&sig), atpkg::net::SIG_CAP)?;
        } else {
            package::fetch_pair(ctx, repo, &tag, &mf, &sig, dir)?;
        }
        Self::from_files(&dir.join(mf), &dir.join(sig), build, |raw, sig| {
            quad.verify_pkg(raw, sig)
        })
    }

    fn endpoint(&self, archive: &Path, scratch: &Path, target: &str) -> Result<Endpoint> {
        let artifact = self
            .pkg
            .artifact_for(target)
            .ok_or_else(|| Error::refused("the signed Codex manifest does not serve the target"))?;
        let root = scratch.join("payload");
        atpkg::verify_for_publication(artifact, archive, &root, &atpkg::install::StageHooks::NONE)
            .map_err(|e| Error::refused(format!("the signed Codex archive does not stage: {e}")))?;
        let (_, row, triple) = package::package_row(PROGRAM)?;
        let checked = package::read_package(&root, row.exposes[0], triple)?;
        if checked.version != self.pkg.version {
            return Err(Error::refused(
                "signed Codex payload and manifest versions differ",
            ));
        }
        Ok(Endpoint {
            build: self.pkg.build_number,
            version: checked.version,
            source: checked.source,
            manifest_sha256: self.digest.clone(),
            tree_root: artifact.tree_root.clone(),
            executable_sha256: executable_hash(&root.join("bin/codex"))?,
        })
    }
}

fn public_endpoint(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    repo: &str,
    build: u64,
    target: &str,
) -> Result<Endpoint> {
    let files = Scratch::new("codex-protocol-predecessor")?;
    let dir = files.path();
    let manifest = Manifest::fetch(ctx, quad, repo, build, dir, true)?;
    let artifact = manifest
        .pkg
        .artifact_for(target)
        .ok_or_else(|| Error::refused("the signed Codex predecessor does not serve the target"))?;
    // The downloaded file is private scratch. Its destination never comes from
    // the remote manifest, even for a validly signed historical package.
    let archive = dir.join("payload.archive");
    public_file(
        ctx,
        repo,
        &upload::tag(PROGRAM, build),
        &artifact.asset,
        &archive,
        artifact.size,
    )?;
    manifest.endpoint(&archive, dir, target)
}

fn qualified_endpoint(
    ctx: &Ctx,
    build: u64,
    package: &PackageOutcome,
    target: &str,
) -> Result<Endpoint> {
    let signer = pack::Signer::load(ctx)?;
    let manifest =
        Manifest::from_files(&package.manifest, &package.signature, build, |raw, sig| {
            signer.authority().verify_pkg(raw, sig)
        })?;
    manifest.endpoint(
        &package.asset,
        Scratch::new("codex-protocol-candidate")?.path(),
        target,
    )
}

impl Input {
    fn relation(old: Option<&Endpoint>, new: &Endpoint) -> Result<Relation> {
        match old {
            None => Ok(Relation::First),
            Some(old) if old.build < new.build => Ok(Relation::Upgrade),
            Some(old) if old == new => Ok(Relation::AlreadySelected),
            Some(_) => Err(Error::refused(
                "the public Codex pin supersedes or disagrees with the frozen candidate",
            )),
        }
    }

    fn binds(&self, intent: &Intent) -> Result<()> {
        if self.schema != 1
            || self.target != intent.target
            || intent.program != PROGRAM
            || self.successor.build != intent.build
            || self.successor.version != intent.version
            || self.successor.source != intent.source
            || self.successor.tree_root != intent.packed_tree
            || !hex(&self.successor.manifest_sha256, 64)
            || !hex(&self.successor.executable_sha256, 64)
            || Self::relation(self.predecessor.as_ref(), &self.successor)? != self.relation
        {
            return Err(Error::refused(
                "Codex protocol input differs from the frozen publication",
            ));
        }
        Ok(())
    }
}

impl Campaign {
    /// Retain the public predecessor and the locally signed successor's exact input
    /// identities for the Codex release controller. This writes no public or staging release.
    /// # Errors
    /// The campaign, signed channel, package, payload or local input cannot be authenticated.
    pub fn prepare_protocol(
        &mut self,
        ctx: &Ctx,
        request: &PackageRequest,
        tools: &dyn Tools,
        inputs: &crate::project_toolchain::Inputs,
        signed_root: &dyn Fn(u64) -> std::result::Result<String, String>,
    ) -> Result<()> {
        let standing = self.standing(ctx, &mut Log::echo("codex protocol inputs"))?;
        let (predecessor, successor) =
            if let super::index_snapshot::Standing::Published(_) = standing {
                // An already-public campaign can be re-read after a later index moved again.
                let retained = self.retained_index(ctx)?.ok_or_else(|| {
                    Error::refused("the published Codex index is no longer retained")
                })?;
                let completed = public_endpoint(
                    ctx,
                    retained.candidate(),
                    &self.inputs.repository,
                    self.inputs.build,
                    &self.inputs.target,
                )?;
                (Some(completed.clone()), completed)
            } else {
                let qualified = self.qualify_package(ctx, request, tools, inputs, signed_root)?;
                let successor =
                    qualified_endpoint(ctx, self.inputs.build, &qualified, &self.inputs.target)?;
                let scratch = Scratch::new("codex-protocol-head")?;
                let (_, head) = quad::fetch_head_with(
                    ctx,
                    &ctx.index_slug(),
                    scratch.path(),
                    quad::Freshness::Ignore,
                )?;
                let old = pin(&head, &self.inputs.repository, &self.inputs.target)?
                    .map(|build| {
                        public_endpoint(
                            ctx,
                            &head,
                            &self.inputs.repository,
                            build,
                            &self.inputs.target,
                        )
                    })
                    .transpose()?;
                (old, successor)
            };
        let pkg =
            package::read_package(&request.package, PROGRAM, package::package_row(PROGRAM)?.2)?;
        if Intent::from_package(ctx, &request.program, request.build, &pkg)? != self.inputs {
            return Err(Error::refused(
                "the Codex input changed during protocol preparation",
            ));
        }
        let candidate = pkg.root.join("bin/codex");
        if executable_hash(&candidate)? != successor.executable_sha256 {
            return Err(Error::refused(
                "the candidate executable differs from the signed archive",
            ));
        }
        let input = Input {
            schema: 1,
            target: self.inputs.target.clone(),
            relation: Input::relation(predecessor.as_ref(), &successor)?,
            predecessor,
            successor,
            candidate,
        };
        input.binds(&self.inputs)?;
        let bytes = aterm_json::to_vec(&input).map_err(|e| Error::refused(e.to_string()))?;
        let hash = atpkg::sha256_bytes(&bytes);
        self.check()?;
        self.keep(&format!("codex-protocol-{hash}.json"), &bytes)?;
        let prepared = Observation::CodexProtocolPrepared { input: hash };
        if !self.observations().any(|event| event == &prepared) {
            self.record(prepared)?;
        }
        private_file::write_private(&self.root, INPUT, &bytes)
            .map_err(|e| Error::io("write private Codex protocol input", &e))?;
        self.check()
    }

    /// Bind this publishing operation to the named protocol input before public promotion.
    /// This is an input comparison, independent of the source controller's check result.
    /// # Errors
    /// The named input is missing, malformed, modified or belongs to another campaign.
    pub fn expect_protocol(&mut self, hash: &str) -> Result<()> {
        if !hex(hash, 64) {
            return Err(Error::usage(
                "--expect-codex-protocol requires a lowercase SHA-256",
            ));
        }
        self.check()?;
        let bytes = private_file::read_private(
            &self.root,
            &format!("codex-protocol-{hash}.json"),
            MAX_BYTES,
        )
        .map_err(|e| Error::refused(format!("read retained Codex protocol input: {e}")))?;
        if atpkg::sha256_bytes(&bytes) != hash {
            return Err(Error::refused("the retained Codex protocol input changed"));
        }
        if !self.observations().any(|event| {
            matches!(event,
            Observation::CodexProtocolPrepared { input } if input == hash)
        }) {
            return Err(Error::refused(
                "this campaign has no preparation record for the named Codex input",
            ));
        }
        let input: Input =
            aterm_json::from_slice(&bytes).map_err(|e| Error::refused(e.to_string()))?;
        input.binds(&self.inputs)?;
        self.expected_protocol = Some(input);
        Ok(())
    }

    pub(super) fn check_protocol(
        &self,
        ctx: &Ctx,
        publication: &PreparedPublication<'_>,
    ) -> Result<()> {
        let Some(expected) = &self.expected_protocol else {
            if self
                .observations()
                .any(|event| matches!(event, Observation::CodexProtocolPrepared { .. }))
            {
                return Err(Error::refused(
                    "this campaign prepared a Codex protocol check; resume through the release controller with --expect-codex-protocol",
                ));
            }
            return Ok(());
        };
        let old = publication.predecessor().ok_or_else(|| {
            Error::refused("the Codex campaign has no signed channel predecessor")
        })?;
        let repo = &self.inputs.repository;
        for (quad, endpoint, public) in [
            (old, expected.predecessor.as_ref(), true),
            (publication.candidate(), Some(&expected.successor), false),
        ] {
            let actual = pin(quad, repo, &expected.target)?;
            if actual != endpoint.map(|end| end.build) {
                return Err(Error::refused(
                    "the Codex pin changed after preparation; prepare and check the new pair",
                ));
            }
            if let Some(end) = endpoint {
                let files = Scratch::new("codex-protocol-recheck")?;
                let live = Manifest::fetch(ctx, quad, repo, end.build, files.path(), public)?;
                if live.digest != end.manifest_sha256
                    || live.pkg.version != end.version
                    || !live
                        .pkg
                        .artifact_for(&expected.target)
                        .is_some_and(|row| row.tree_root == end.tree_root)
                {
                    return Err(Error::refused(
                        "the signed Codex manifest changed after preparation; prepare and check the new pair",
                    ));
                }
            }
        }
        Ok(())
    }
}

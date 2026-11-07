// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Install integrity for Trust trains. Consumer verdicts inform adoption; only integrity
//! refuses publication. Receipts bind the signed manifest and every installed artifact.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::artifact::{pkgdoc, upload};
use crate::channel::{Log, Scratch, download_retried, host_error};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::quad::{self, VerifiedQuad};

/// The committed evidence and its detached machine signature.
pub const RECEIPT: &str = "train-receipt.toml";
/// Signature over the exact receipt bytes.
pub const SIGNATURE: &str = "train-receipt.toml.sig";
/// Command line.
pub const SYNOPSIS: &str =
    "canary trust=<N> --out DIR [--consumer NAME=CHECKOUT]… [--stage-receipt]";

/// One consumer's observation, never an index gate.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsumerReport {
    /// Consumer name.
    pub name: String,
    /// Tree judged (empty when unavailable).
    pub commit: String,
    /// Existing lock build, if readable; canary never moves a consumer's lock.
    pub locked_build: Option<u64>,
    /// Green, red, or could-not-run.
    pub verdict: String,
    /// Key steps or the reason it could not run.
    pub detail: String,
}

/// One artifact installed and executed on its native canary host.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    /// Host triple.
    pub target: String,
    /// Signed archive identity.
    pub asset: String,
    /// Digest of its downloaded bytes.
    pub sha256: String,
    /// Root returned by the client's installation chain.
    pub tree_root: String,
    /// Commit answered by the installed compiler, also by its rustc alias.
    pub compiler_commit: String,
}

/// Signed train evidence. A separate native canary is required for every artifact.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Receipt schema.
    pub schema: u32,
    /// Build installed.
    pub build: u64,
    /// Exact signed package manifest, preventing same-number replacement.
    pub manifest_sha256: String,
    /// Rostered signer.
    pub machine_id: String,
    /// Roster generation authorizing the signer.
    pub roster_seq: u64,
    /// Clock at installation.
    pub installed_unix: i64,
    /// Installations actually run.
    pub installed: Vec<Installed>,
    /// Consumer keys, including unavailable consumers.
    pub consumers: Vec<ConsumerReport>,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    crate::identity::read_regular_bytes(path, crate::identity::MAX_META_BYTES)
        .map_err(|e| Error::io(format!("read {}", path.display()), &e))
}

fn hash(path: &Path) -> Result<String> {
    atpkg::sha256_file(path).map_err(|e| Error::io(format!("hash {}", path.display()), &e))
}

fn manifest(quad: &VerifiedQuad, dir: &Path, build: u64) -> Result<atpkg::PkgManifest> {
    manifest_with_digest(quad, dir, "trust", build).map(|(pkg, _)| pkg)
}

fn manifest_with_digest(
    quad: &VerifiedQuad,
    dir: &Path,
    program: &str,
    build: u64,
) -> Result<(atpkg::PkgManifest, String)> {
    let bytes = read(&dir.join(pkgdoc::manifest_name(program, build)))?;
    let pkg = quad.verify_pkg(&bytes, &read(&dir.join(pkgdoc::sig_name(program, build)))?)?;
    if pkg.program != program || pkg.build_number != build || pkg.artifacts.is_empty() {
        return Err(Error::refused(format!(
            "canary: manifest does not bind the requested {program} build"
        )));
    }
    Ok((pkg, atpkg::abrew::fetch::sha256_bytes(&bytes)))
}

/// Verify the receipt under the head roster, and bind every artifact and the exact
/// manifest to what was installed and run. Consumer reds deliberately do not refuse.
///
/// # Errors
/// Missing, unsigned, tampered, incomplete or different installation evidence.
pub fn check_receipt(ctx: &Ctx, quad: &VerifiedQuad, dir: &Path, build: u64) -> Result<Receipt> {
    let bytes = read(&dir.join(RECEIPT))?;
    let who = quad
        .roster
        .authorize_appcast(&bytes, &read(&dir.join(SIGNATURE))?, ctx.now_unix)
        .map_err(|e| Error::refused(format!("canary receipt signature refused: {e:?}")))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| Error::refused(e.to_string()))?;
    let receipt: Receipt =
        aterm_toml::from_str(text).map_err(|e| Error::refused(format!("canary receipt: {e}")))?;
    who.bind(Some(&receipt.machine_id), Some(receipt.roster_seq))
        .map_err(|e| Error::refused(format!("canary receipt attribution: {e:?}")))?;
    let (pkg, manifest_sha256) = manifest_with_digest(quad, dir, "trust", build)?;
    if receipt.schema != 1
        || receipt.build != build
        || receipt.manifest_sha256 != manifest_sha256
        || receipt.installed.len() != pkg.artifacts.len()
    {
        return Err(Error::refused(
            "canary receipt does not bind this manifest and its complete artifact set",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for artifact in &pkg.artifacts {
        let mut matches = receipt
            .installed
            .iter()
            .filter(|i| i.target == artifact.target);
        let (Some(installed), None) = (matches.next(), matches.next()) else {
            return Err(Error::refused(format!(
                "canary receipt: {} has no unique installation",
                artifact.target
            )));
        };
        if artifact.kind != "sysroot-bundle"
            || artifact.tree_root.len() != 64
            || !seen.insert(&artifact.target)
            || installed.asset != artifact.asset
            || installed.sha256 != artifact.sha256
            || installed.tree_root != artifact.tree_root
            || installed.compiler_commit.len() != 40
            || hash(&dir.join(&artifact.asset))? != installed.sha256
        {
            return Err(Error::refused(format!(
                "canary receipt: {} differs from the installed and executed bytes",
                artifact.target
            )));
        }
    }
    Ok(receipt)
}

/// Injected native execution boundary; the real runner runs only the installed tree.
pub trait Runner {
    /// Run both compiler names, then return their common commit.
    /// # Errors
    /// Either compiler fails, or reports a different commit.
    fn compiler(&self, bin: &Path, rustup_home: &Path) -> Result<String>;
    /// Observe a consumer's key using this compiler, preserving its pinned lock.
    fn consumer(
        &self,
        name: &str,
        checkout: &Path,
        bin: &Path,
        rustup_home: &Path,
        installed: &Installed,
    ) -> ConsumerReport;
}

/// Real native compiler and consumer execution.
pub struct Native;

impl Runner for Native {
    fn compiler(&self, bin: &Path, rustup_home: &Path) -> Result<String> {
        let mut commits = Vec::new();
        for tool in ["trustc", "rustc"] {
            let mut cmd = aterm_uds::spawnfd::command(bin.join(tool));
            cmd.arg("-vV").env("RUSTUP_HOME", rustup_home);
            let output = aterm_uds::deadline::output_until(
                &mut cmd,
                Instant::now() + Duration::from_secs(60),
            )
            .map_err(|e| Error::cannot_run(format!("canary {tool}: {e:?}")))?;
            if !output.status.success() {
                return Err(Error::refused(format!(
                    "canary {tool} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            let text = String::from_utf8_lossy(&output.stdout);
            let commit = text
                .lines()
                .find_map(|l| l.strip_prefix("commit-hash: "))
                .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| {
                    Error::refused(format!("canary {tool} answered no full compiler commit"))
                })?;
            commits.push(commit.to_string());
        }
        if commits[0] != commits[1] {
            return Err(Error::refused("canary rustc alias differs from trustc"));
        }
        Ok(commits.remove(0))
    }

    fn consumer(
        &self,
        name: &str,
        checkout: &Path,
        bin: &Path,
        rustup_home: &Path,
        installed: &Installed,
    ) -> ConsumerReport {
        let mut report = ConsumerReport {
            name: name.to_string(),
            commit: crate::git::head(checkout).unwrap_or_default(),
            locked_build: None,
            verdict: "could-not-run".into(),
            detail: String::new(),
        };
        report.locked_build = std::fs::read_to_string(checkout.join("trust.lock"))
            .ok()
            .and_then(|s| atpkg::trust_lock::parse(&s).ok())
            .map(|l| l.build);
        if !crate::git::is_clean(checkout).unwrap_or(false) {
            report.detail =
                "consumer checkout unavailable or dirty; no committed tree was judged".into();
            return report;
        }
        match consumer_key(checkout, bin, rustup_home, installed) {
            Ok((green, detail)) => {
                report.verdict = if green { "green" } else { "red" }.into();
                report.detail = detail;
            }
            Err(e) => report.detail = e.to_string(),
        }
        report.detail = format!(
            "TRUST_STAGE2_BIN={} overrides consumer lock {:?} for this report; {}",
            bin.display(),
            report.locked_build,
            report.detail
        );
        report
    }
}

fn consumer_key(
    checkout: &Path,
    bin: &Path,
    rustup_home: &Path,
    installed: &Installed,
) -> Result<(bool, String)> {
    let steps = crate::adopt::key_steps(checkout)?;
    let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .map_err(|e| Error::refused(e.to_string()))?;
    let lock = atpkg::trust_lock::TrustLock {
        build: 1,
        tree_root: installed.tree_root.clone(),
        compiler_commit: installed.compiler_commit.clone(),
        receipt: String::new(),
        stock_toolchain: String::new(),
    };
    let movers = atpkg::toolchain::rustc_movers::rustc_movers(
        Some(checkout),
        &|k| std::env::var(k).ok(),
        None,
    );
    if let Some(why) = atpkg::toolchain::rustc_movers::lock_refusal(
        &movers,
        &path,
        &checkout.join("trust.lock"),
        &lock,
    ) {
        return Err(Error::refused(why));
    }
    let mut detail = Vec::new();
    for words in steps {
        let (program, args) = words
            .split_first()
            .ok_or_else(|| Error::refused("empty consumer key"))?;
        if program != "targo" {
            return Err(Error::refused(
                "canary consumer keys must invoke targo directly so every compile uses the candidate",
            ));
        }
        // Naming both compiler selectors prevents an ambient default or a lock from
        // changing the compiler between key steps. Verify checks their compiler line.
        let env = vec![
            ("PATH".into(), path.clone()),
            ("TRUST_STAGE2_BIN".into(), bin.as_os_str().to_owned()),
            ("RUSTUP_HOME".into(), rustup_home.as_os_str().to_owned()),
            ("RUSTC".into(), bin.join("rustc").into_os_string()),
            (
                "CARGO_BUILD_RUSTC".into(),
                bin.join("rustc").into_os_string(),
            ),
        ];
        let green = crate::adopt::run_targo_with_env(checkout, &bin.join("targo"), args, &env)
            .map_err(|e| Error::cannot_run(format!("consumer key: {e}")))?;
        detail.push(format!(
            "{}: {}",
            words.join(" "),
            if green { "green" } else { "red" }
        ));
        if !green {
            return Ok((false, detail.join("\n")));
        }
    }
    Ok((true, detail.join("\n")))
}

/// Install a downloaded, signed pack into an isolated prefix through the client's
/// digest, extraction and tree-root chain, execute it, then sign the reports.
///
/// # Errors
/// Integrity or native execution fails, output is occupied, or signing fails.
pub fn install(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    source: &Path,
    build: u64,
    out: &Path,
    consumers: &BTreeMap<String, PathBuf>,
    runner: &dyn Runner,
) -> Result<Receipt> {
    std::fs::create_dir(out).map_err(|e| Error::io("create fresh canary output", &e))?;
    let physical_out = out
        .canonicalize()
        .map_err(|e| Error::io("resolve canary output", &e))?;
    let out = physical_out.as_path();
    let layout = atpkg::Layout {
        prefix: out.join("prefix"),
    };
    let (pkg, manifest_sha256) = manifest_with_digest(quad, source, "trust", build)?;
    let mut installed = Vec::new();
    let rustup_home = out.join("rustup");
    std::fs::create_dir(&rustup_home).map_err(|e| Error::io("create scratch RUSTUP_HOME", &e))?;
    let triple = crate::triple::Triple::host()
        .ok_or_else(|| Error::refused("no native canary target"))?
        .as_str();
    for artifact in &pkg.artifacts {
        if artifact.target != triple || artifact.kind != "sysroot-bundle" {
            return Err(Error::refused(format!(
                "canary needs a native sysroot-bundle for {}, this host is {triple}",
                artifact.target
            )));
        }
        let dest = layout.build_dir("trust", build);
        let root = atpkg::verify_and_stage(
            artifact,
            &source.join(&artifact.asset),
            &dest,
            &atpkg::install::StageHooks::NONE,
        )
        .map_err(|e| Error::refused(format!("canary client install refused: {e}")))?;
        let bin = dest.join("bin");
        let commit = runner.compiler(&bin, &rustup_home)?;
        installed.push(Installed {
            target: artifact.target.clone(),
            asset: artifact.asset.clone(),
            sha256: artifact.sha256.clone(),
            tree_root: root,
            compiler_commit: commit,
        });
    }
    let item = installed
        .first()
        .ok_or_else(|| Error::refused("no native installation"))?;
    let bin = layout.build_dir("trust", build).join("bin");
    let reports = consumers
        .iter()
        .map(|(name, dir)| runner.consumer(name, dir, &bin, &rustup_home, item))
        .collect();
    let signer = crate::identity::signer_for(ctx, &quad.roster)?;
    let receipt = Receipt {
        schema: 1,
        build,
        manifest_sha256,
        machine_id: signer.record.id,
        roster_seq: signer.roster_seq,
        installed_unix: ctx.now_unix,
        installed,
        consumers: reports,
    };
    // Keep the exact pack beside its evidence: later publication never repacks a seal.
    for name in [
        pkgdoc::manifest_name("trust", build),
        pkgdoc::sig_name("trust", build),
    ]
    .into_iter()
    .chain(pkg.artifacts.iter().map(|a| a.asset.clone()))
    {
        crate::identity::copy_regular_file(&source.join(&name), &out.join(&name))
            .map_err(|e| Error::io("copy canaried pack", &e))?;
    }
    if hash(&out.join(pkgdoc::manifest_name("trust", build)))? != receipt.manifest_sha256
        || receipt
            .installed
            .iter()
            .any(|i| hash(&out.join(&i.asset)).ok().as_deref() != Some(i.sha256.as_str()))
    {
        return Err(Error::refused(
            "canary source changed during execution; no receipt signed",
        ));
    }
    let text = aterm_toml::to_string(&receipt).map_err(|e| Error::refused(e.to_string()))?;
    crate::lane::report::write_atomic(&out.join(RECEIPT), &text)?;
    signer.key.sign_file(&out.join(RECEIPT))?;
    check_receipt(ctx, quad, out, build)?;
    Ok(receipt)
}

/// Install the staged pack, or stage an existing signed receipt without moving the index.
/// The current head's roster authorizes a new build without pinning it first.
///
/// # Errors
/// Download, verification, install, signing or staging fails.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut build = None;
    let mut out = None;
    let mut consumers = BTreeMap::new();
    let mut stage_existing = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stage-receipt" => stage_existing = true,
            "--out" => {
                out = Some(PathBuf::from(crate::channel::flag_value(
                    args, &mut i, "--out",
                )?))
            }
            "--consumer" => {
                let value = crate::channel::flag_value(args, &mut i, "--consumer")?;
                let (name, dir) = value
                    .split_once('=')
                    .ok_or_else(|| Error::usage("--consumer NAME=CHECKOUT"))?;
                consumers.insert(name.to_string(), PathBuf::from(dir));
            }
            s if s.starts_with("trust=") && build.is_none() => {
                build = s[6..].parse::<u64>().ok().filter(|b| *b > 0)
            }
            s => return Err(Error::usage(format!("canary: unknown argument {s}"))),
        }
        i += 1;
    }
    let build = build.ok_or_else(|| Error::usage(SYNOPSIS))?;
    let out = out.ok_or_else(|| Error::usage(SYNOPSIS))?;
    if stage_existing {
        if !consumers.is_empty() {
            return Err(Error::usage(
                "--stage-receipt stages existing evidence; it takes no --consumer",
            ));
        }
        stage_receipt(ctx, &out, build)?;
        Log::echo("atpkg-publish canary").say(format!(
            "staged verified canary evidence for Trust {build}; index unchanged"
        ));
        return Ok(());
    }
    for name in ["aterm", "ay", "ty", "clean"] {
        consumers
            .entry(name.to_string())
            .or_insert_with(|| ctx.home.join(name));
    }
    let scratch = Scratch::new("train-canary")?;
    let (_, head) = quad::fetch_head(ctx, &ctx.index_slug(), &scratch.path().join("head"))?;
    let dir = scratch.path().join("pack");
    let slug = ctx.staging_slug("trust");
    let tag = upload::tag("trust", build);
    let pair = [
        pkgdoc::manifest_name("trust", build),
        pkgdoc::sig_name("trust", build),
    ];
    download_retried(
        &*ctx.hosts.staging,
        &slug,
        &tag,
        &pair.iter().map(String::as_str).collect::<Vec<_>>(),
        &dir,
    )
    .map_err(|e| host_error("download staged Trust manifest", &e))?;
    let pkg = manifest(&head, &dir, build)?;
    let names: Vec<_> = pkg.artifacts.iter().map(|a| a.asset.as_str()).collect();
    download_retried(&*ctx.hosts.staging, &slug, &tag, &names, &dir)
        .map_err(|e| host_error("download staged Trust pack", &e))?;
    let receipt = install(ctx, &head, &dir, build, &out, &consumers, &Native)?;
    let mut log = Log::echo("atpkg-publish canary");
    log.say(format!(
        "Trust {build}: installed and executed {} artifact(s); signed {}",
        receipt.installed.len(),
        out.join(RECEIPT).display()
    ));
    for report in receipt.consumers {
        log.say(format!(
            "{}: {} — {}",
            report.name, report.verdict, report.detail
        ));
    }
    Ok(())
}

/// Resume the staged group without running a packer. Upload the signed canary evidence
/// beside staging's immutable pack, then let index/mirror verify it again before publication.
///
/// # Errors
/// Missing or changed staged bytes, bad receipt, or publication failure.
pub fn publish_staged(
    ctx: &Ctx,
    out: &Path,
    indexer: &dyn crate::api::Indexer,
    log: &mut Log,
) -> Result<crate::channel::group::GroupOutcome> {
    let text = String::from_utf8(read(&out.join("staged-builds.toml"))?)
        .map_err(|e| Error::refused(e.to_string()))?;
    let builds: BTreeMap<String, u64> =
        aterm_toml::from_str(&text).map_err(|e| Error::refused(format!("staged builds: {e}")))?;
    let build = *builds
        .get("trust")
        .ok_or_else(|| Error::refused("staged group has no Trust build"))?;
    let scratch = Scratch::new("publish-staged")?;
    let (_, head) = quad::fetch_head(ctx, &ctx.index_slug(), &scratch.path().join("head"))?;
    let (pkg, receipt) = stage_receipt_with(ctx, &head, &out.join("canary"), build)?;
    let targets = pkg
        .artifacts
        .iter()
        .map(|a| crate::triple::Triple::parse(&a.target))
        .collect::<Result<Vec<_>>>()?;
    let delta = crate::channel::group::selection_delta(&builds, targets)?;
    check_cohort(ctx, &head, &builds, &receipt, scratch.path())?;
    let published = indexer.publish(
        ctx,
        &crate::api::IndexRequest {
            baseline: crate::api::Baseline::ChannelHead,
            delta,
            out: out.join("index"),
            upload: true,
            pack_dir: None,
        },
    )?;
    log.say(format!(
        "published staged Trust {build} with byte-bound canary receipt"
    ));
    Ok(crate::channel::group::GroupOutcome {
        builds,
        index_build: Some(published.index_build),
        out: out.to_path_buf(),
    })
}

/// Stage an existing signed receipt against the complete, re-downloaded pack.
/// This leaves the index unchanged so one later delta can include other packages.
///
/// # Errors
/// Missing, changed or invalid staged bytes or evidence, or a staging upload refusal.
pub fn stage_receipt(ctx: &Ctx, evidence: &Path, build: u64) -> Result<atpkg::PkgManifest> {
    let scratch = Scratch::new("stage-canary")?;
    let (_, head) = quad::fetch_head(ctx, &ctx.index_slug(), &scratch.path().join("head"))?;
    stage_receipt_with(ctx, &head, evidence, build).map(|(pkg, _)| pkg)
}

fn stage_receipt_with(
    ctx: &Ctx,
    head: &VerifiedQuad,
    evidence: &Path,
    build: u64,
) -> Result<(atpkg::PkgManifest, Receipt)> {
    let scratch = Scratch::new("stage-canary-pack")?;
    check_receipt(ctx, head, evidence, build)?;
    let slug = ctx.staging_slug("trust");
    let tag = upload::tag("trust", build);
    let dir = scratch.path().join("pack");
    let release = ctx
        .hosts
        .staging
        .release(&slug, &tag)
        .map_err(|e| host_error("read staged Trust", &e))?;
    download_retried(
        &*ctx.hosts.staging,
        &slug,
        &tag,
        &release
            .assets
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        &dir,
    )
    .map_err(|e| host_error("re-read staged Trust", &e))?;
    // Re-check downloaded bytes with the evidence, before any public operation.
    let mut missing = Vec::new();
    for name in [RECEIPT, SIGNATURE] {
        if release.assets.iter().any(|n| n == name) {
            if read(&dir.join(name))? != read(&evidence.join(name))? {
                return Err(Error::refused(
                    "staging already carries different canary evidence",
                ));
            }
        } else {
            missing.push(evidence.join(name));
        }
        crate::identity::copy_regular_file(&evidence.join(name), &dir.join(name))
            .map_err(|e| Error::io("copy canary evidence", &e))?;
    }
    let receipt = check_receipt(ctx, head, &dir, build)?;
    if !missing.is_empty() {
        ctx.hosts
            .staging
            .upload(&slug, &tag, &missing, crate::host::Replace::Never)
            .map_err(|e| host_error("stage canary evidence", &e))?;
    }
    Ok((manifest(head, &dir, build)?, receipt))
}

/// Fetch each sibling's signed metadata once, then judge every canaried target
/// against that same snapshot. Index/mirror still verify the complete asset set.
fn check_cohort(
    ctx: &Ctx,
    head: &VerifiedQuad,
    builds: &BTreeMap<String, u64>,
    receipt: &Receipt,
    work: &Path,
) -> Result<()> {
    for (program, &build) in builds {
        if program == "trust" {
            continue;
        }
        let def = crate::programs::get(program)
            .ok_or_else(|| Error::refused(format!("unknown rustc group member {program}")))?;
        let slug = ctx.staging_slug(def.repo);
        let tag = upload::tag(program, build);
        let dir = work.join(program);
        let mf = pkgdoc::manifest_name(program, build);
        let sig = pkgdoc::sig_name(program, build);
        download_retried(&*ctx.hosts.staging, &slug, &tag, &[&mf, &sig], &dir)
            .map_err(|e| host_error("read staged compiler-group manifest", &e))?;
        let (pkg, _) = manifest_with_digest(head, &dir, program, build)?;
        for installed in &receipt.installed {
            let mut rows = pkg
                .artifacts
                .iter()
                .filter(|a| a.target == installed.target);
            let (Some(artifact), None) = (rows.next(), rows.next()) else {
                return Err(Error::refused(format!(
                    "staged {program}@{build} has no unique artifact for canaried target {}",
                    installed.target
                )));
            };
            if artifact
                .provenance
                .as_ref()
                .map(|p| p.compiler_commit.as_str())
                != Some(installed.compiler_commit.as_str())
            {
                return Err(Error::refused(format!(
                    "staged {program}@{build} on {} does not bind canaried compiler commit {}",
                    installed.target, installed.compiler_commit
                )));
            }
        }
    }
    Ok(())
}

/// Verify a newly selected Trust build even when its public release already exists.
///
/// # Errors
/// Neither host serves a complete signed pack and its native canary evidence.
pub fn verify_pinned(ctx: &Ctx, quad: &VerifiedQuad, build: u64) -> Result<()> {
    let scratch = Scratch::new("canary-pin")?;
    let tag = upload::tag("trust", build);
    for (host, slug) in [
        (&ctx.hosts.channel, ctx.public_slug("trust")),
        (&ctx.hosts.staging, ctx.staging_slug("trust")),
    ] {
        let release = match host.release(&slug, &tag) {
            Ok(r) => r,
            Err(crate::host::HostError::NotFound) => continue,
            Err(e) => return Err(host_error("read Trust canary evidence", &e)),
        };
        let dir = scratch.path().join(&slug.owner);
        download_retried(
            &**host,
            &slug,
            &tag,
            &release
                .assets
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            &dir,
        )
        .map_err(|e| host_error("download Trust canary evidence", &e))?;
        check_receipt(ctx, quad, &dir, build)?;
        return Ok(());
    }
    Err(Error::refused(format!(
        "Trust {build} has no pack with native canary evidence"
    )))
}

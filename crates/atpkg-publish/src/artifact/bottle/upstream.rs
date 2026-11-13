// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pinned upstream source builds and prebuilt tools, prepared without signing or upload.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use atpkg::abrew::{
    api, fetch,
    formula::{Bottle, Formula},
    machine::Machine,
};

use super::{BottleOutcome, BottleRequest};
use crate::api::{Build, Signing};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::{Arch, Os, Triple};

/// The deliberately unsigned acquisition/qualification command.
pub const SYNOPSIS: &str =
    "pack-upstream <program> --version V --build N --triple T [--merge-from PKG] [--out DIR]";

/// One pinned package's absent GNU target, prepared unsigned for the existing signing lane.
#[derive(Clone, Debug)]
pub struct Request {
    /// A declared upstream-bottle program.
    pub program: String,
    /// The existing signed package's exact upstream version.
    pub version: String,
    /// Its existing build number.
    pub build: Build,
    /// The absent target.
    pub triple: Triple,
    /// Its published manifest, retained byte for byte as the merge prefix.
    pub merge_from: Option<PathBuf>,
    /// The unsigned output root.
    pub out: PathBuf,
}

/// Official metadata and archives, separated so closure/version/digest controls are testable.
pub trait Source {
    /// An unresolved official formula; target variations are applied by the collector.
    /// # Errors
    /// Unavailable or malformed metadata.
    fn formula(&self, name: &str) -> Result<Record>;
    /// Download only; the collector independently checks the pinned digest before extraction.
    /// # Errors
    /// A transport failure.
    fn download(&self, bottle: &Bottle, path: &Path) -> Result<()>;
}

/// Formula metadata retained for both prebuilt donors and the main source build.
pub struct Record {
    /// The ordinary formula and its platform variants.
    pub formula: Formula,
    /// macOS supplies these source dependencies; a GNU source build must carry them.
    pub source_dependencies: Vec<String>,
}

fn source_dependencies(doc: &aterm_json::Value) -> Result<Vec<String>> {
    let entries = doc
        .get("uses_from_macos")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| {
            Error::refused(
                "official formula has no readable uses_from_macos source dependency metadata",
            )
        })?;
    let mut names = Vec::new();
    for entry in entries {
        if let Some(name) = entry.as_str() {
            names.push(name.to_string());
            continue;
        }
        let map = entry
            .as_object()
            .filter(|map| map.len() == 1)
            .ok_or_else(|| Error::refused("unreadable source dependency role metadata"))?;
        for (name, roles) in map {
            if let Some(role) = roles.as_str() {
                match role {
                    "build" | "test" => continue,
                    "run" => {
                        names.push(name.clone());
                        continue;
                    }
                    _ => return Err(Error::refused("unknown source dependency role")),
                }
            }
            let roles = roles
                .as_array()
                .ok_or_else(|| Error::refused("source dependency roles must be an array"))?;
            if roles
                .iter()
                .any(|role| !matches!(role.as_str(), Some("build" | "test")))
                || roles.is_empty()
            {
                names.push(name.clone());
            }
        }
    }
    Ok(names)
}

/// HTTPS formula metadata and anonymous, digest-addressed GHCR archives.
pub struct Official;

/// Formula API documents are small; this matches abrew's existing in-memory response cap.
const FORMULA_DOCUMENT_CAP: u64 = 64 << 20;
/// The previous source downloader admitted archives up to 16 GiB. The shared resumable lane
/// retains that cap while replacing its one-shot curl.
const SOURCE_ARCHIVE_CAP: u64 = 16 << 30;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+@-".contains(&byte))
}

impl Source for Official {
    fn formula(&self, name: &str) -> Result<Record> {
        if !valid_name(name) {
            return Err(Error::refused("invalid upstream formula name"));
        }
        let bytes = aterm_update_core::download_bytes_with_accept(
            &format!("https://formulae.brew.sh/api/formula/{name}.json"),
            FORMULA_DOCUMENT_CAP,
            "application/json",
        )
        .map_err(Error::cannot_run)?;
        let doc = aterm_json::from_slice(&bytes)
            .map_err(|error| Error::refused(format!("formula {name}: {error}")))?;
        let formula = Formula::from_api(&doc).map_err(Error::refused)?;
        if formula.name != name || formula.disable {
            return Err(Error::refused(format!(
                "official formula {name} is disabled or names another formula"
            )));
        }
        Ok(Record {
            formula,
            source_dependencies: source_dependencies(&doc)
                .map_err(|error| Error::refused(format!("formula {name}: {error}")))?,
        })
    }

    fn download(&self, bottle: &Bottle, path: &Path) -> Result<()> {
        if bottle.tag == "source" {
            if ![
                "https://mirrors.edge.kernel.org/pub/software/scm/git/",
                "https://www.kernel.org/pub/software/scm/git/",
                "https://cdn.kernel.org/pub/software/scm/git/",
            ]
            .iter()
            .any(|prefix| bottle.url.starts_with(prefix))
            {
                return Err(Error::refused(
                    "official Git source URL is not a kernel.org source archive",
                ));
            }
            return aterm_update_core::download_to_resumable_https_only(
                &bottle.url,
                path,
                SOURCE_ARCHIVE_CAP,
            )
            .map_err(Error::cannot_run);
        }
        let repo = api::ghcr_repo_of(&bottle.url)
            .filter(|repo| repo.starts_with("homebrew/core/"))
            .ok_or_else(|| Error::refused("official bottle is not a Homebrew core GHCR blob"))?;
        if !bottle
            .url
            .ends_with(&format!("/blobs/sha256:{}", bottle.sha256))
        {
            return Err(Error::refused(
                "official bottle URL does not name its pinned digest",
            ));
        }
        let authorization = api::ghcr_authorization(&repo).map_err(Error::cannot_run)?;
        fetch::curl_to_file(&bottle.url, path, &[authorization.as_str()]).map_err(Error::cannot_run)
    }
}

/// Exact formula-version admission and the complete runtime dependency archive closure.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Origin {
    /// The formula the admitted input belongs to.
    pub formula: String,
    /// Its exact upstream version.
    pub version: String,
    /// The downloaded URL.
    pub url: String,
    /// The independently verified content digest.
    pub sha256: String,
}

/// Exact source and dependency inputs, with their content identities retained for BUILD-ORIGIN.json.
pub struct Inputs {
    /// The verified source archive.
    pub archive: PathBuf,
    /// The source's admitted identity.
    pub source: Origin,
    /// Dependency archives, used only as library/license donors.
    pub donors: Vec<PathBuf>,
    /// The exact runtime donor identities.
    pub dependencies: Vec<Origin>,
}

/// Fetch a coherent, bounded runtime dependency graph and verify every archive before use.
///
/// # Errors
/// A different upstream version, unsupported target, missing bottle or malformed/different digest.
pub fn collect(request: &Request, source: &dyn Source, dir: &Path) -> Result<Inputs> {
    let (_, definition) = super::bottle_row(&request.program, request.triple)?;
    if request.triple.os() != Os::Linux || !definition.linux.contains(&request.triple) {
        return Err(Error::usage(
            "automatic formula acquisition serves declared GNU bottle targets",
        ));
    }
    let (arch, expected) = match request.triple.arch() {
        Arch::Aarch64 => ("arm", "arm64"),
        Arch::X86_64 => ("intel", "x86_64"),
    };
    let machine = Machine::simulated(Some("linux"), Some(arch));
    if machine.arch() != expected {
        return Err(Error::refused(
            "the formula model did not resolve the requested target architecture",
        ));
    }
    let tags = machine.bottle_tags();
    let mut formulas = BTreeMap::new();
    let mut pending = vec![request.program.clone()];
    while let Some(name) = pending.pop() {
        if formulas.contains_key(&name) {
            continue;
        }
        if formulas.len() >= 64 || !valid_name(&name) {
            return Err(Error::refused(
                "upstream runtime dependency graph is invalid or exceeds 64 formulas",
            ));
        }
        let record = source.formula(&name)?;
        let mut formula = record.formula;
        if formula.name != name || !formula.resolve_variations(&machine) {
            return Err(Error::refused(format!(
                "formula {name} cannot be resolved for {}",
                request.triple
            )));
        }
        if name == request.program && formula.version != request.version {
            return Err(Error::refused(format!(
                "{} is pinned at upstream {}, but the official formula serves {}; an absent target cannot silently change the signed package version",
                request.program, request.version, formula.version
            )));
        }
        if name == request.program {
            pending.extend(record.source_dependencies);
        }
        pending.extend(formula.dependencies.iter().cloned());
        formulas.insert(name, formula);
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| Error::io(format!("mkdir {}", dir.display()), &error))?;
    let main = formulas
        .remove(&request.program)
        .ok_or_else(|| Error::refused("main formula is missing"))?;
    let source_bottle = Bottle {
        tag: "source".into(),
        url: main.stable_url,
        sha256: main.stable_sha256,
        cellar: String::new(),
    };
    if source_bottle.sha256.len() != 64
        || !source_bottle
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::refused(
            "official Git source has no strict SHA-256 digest",
        ));
    }
    let archive = dir.join(format!("git-source-{}.tar.xz", source_bottle.sha256));
    source.download(&source_bottle, &archive)?;
    fetch::verify_digest_strict(&archive, &source_bottle.sha256).map_err(Error::refused)?;
    let origin = Origin {
        formula: main.name,
        version: main.version,
        url: source_bottle.url,
        sha256: source_bottle.sha256,
    };
    let mut donors = Vec::new();
    let mut dependencies = Vec::new();
    for (name, formula) in formulas {
        let bottle = formula.bottle_for(&tags).ok_or_else(|| {
            Error::refused(format!(
                "formula {name} has no bottle for {}",
                request.triple
            ))
        })?;
        if bottle.sha256.len() != 64 || !bottle.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::refused(format!(
                "formula {name} has no strict SHA-256 bottle digest"
            )));
        }
        let path = dir.join(format!("{name}-{}.tar.gz", bottle.sha256));
        source.download(bottle, &path)?;
        fetch::verify_digest_strict(&path, &bottle.sha256).map_err(Error::refused)?;
        donors.push(path);
        dependencies.push(Origin {
            formula: name,
            version: formula.version.clone(),
            url: bottle.url.clone(),
            sha256: bottle.sha256.clone(),
        });
    }
    Ok(Inputs {
        archive,
        source: origin,
        donors,
        dependencies,
    })
}

/// Acquire, build, relocate and prepare a declared upstream program without publishing.
///
/// # Errors
/// Any metadata, digest, relocation, client staging or native qualification refusal.
pub fn pack(ctx: &Ctx, request: &Request) -> Result<BottleOutcome> {
    let (_, definition) = super::bottle_row(&request.program, request.triple)?;
    match definition.origin {
        crate::programs::BottleOrigin::Pinned(recipe) => {
            let recipe = recipe.versioned(&request.version).ok_or_else(|| {
                Error::refused(format!(
                    "{} has no shared upstream pin for version {}",
                    request.program, request.version
                ))
            })?;
            return super::pinned::pack(ctx, request, recipe);
        }
        crate::programs::BottleOrigin::Reference(_) => {
            return Err(Error::cannot_run(
                "source-derived references use pack-bottle --install-tree with their qualified build and corresponding source",
            ));
        }
        crate::programs::BottleOrigin::Formula => {}
    }
    let work = crate::channel::Scratch::new("upstream-bottle")?;
    let inputs = collect(request, &Official, work.path())?;
    let tree = super::source::build(request, &inputs, work.path())?;
    let backend = crate::artifact::relocate::backend_for_host().map_err(Error::cannot_run)?;
    let result = super::pack_bottle_with(
        ctx,
        &BottleRequest {
            program: request.program.clone(),
            input: super::BottleInput::InstallTree(tree),
            version: request.version.clone(),
            build: request.build,
            triple: request.triple,
            strip_components: 0,
            dep_bottles: inputs.donors,
            donor_roots: Vec::new(),
            merge_from: request.merge_from.clone(),
            key: None,
            out: request.out.clone(),
            signing: Signing::Unsigned,
            upload: false,
        },
        backend.as_ref(),
        &crate::artifact::tarwrite::Zstd,
    )?;
    qualify(&result)?;
    Ok(result)
}

pub(crate) fn can_build(program: &str, triple: Triple) -> Result<()> {
    let (_, definition) = super::bottle_row(program, triple)?;
    match definition.origin {
        crate::programs::BottleOrigin::Pinned(recipe) => super::pinned::can_pack(recipe, triple),
        crate::programs::BottleOrigin::Reference(_) => Err(Error::cannot_run(
            "source-derived references require a separately qualified install tree",
        )),
        crate::programs::BottleOrigin::Formula => super::source::can_build(triple),
    }
}

fn qualify(result: &BottleOutcome) -> Result<()> {
    let manifest = crate::lane::rows::local(&result.manifest)?;
    let row = manifest
        .artifact_for(result.triple.as_str())
        .ok_or_else(|| Error::refused("qualified bottle has no target row"))?;
    let work = crate::channel::Scratch::new("gnu-git-qualification")?;
    let stage = work.path().join("moved store with spaces");
    atpkg::verify_for_publication(
        row,
        &result.asset,
        &stage,
        &atpkg::install::StageHooks::NONE,
    )
    .map_err(|error| Error::refused(format!("GNU client staging: {error}")))?;
    qualify_staged(result, &stage, row)
}

/// One public Git qualification read. The caller supplies the child so tests can inject exact
/// HTTP outcomes; this function owns the sole anonymous-HTTPS retry budget.
fn anonymous_git_qualification<T>(
    attempt: impl FnMut(
        u32,
        aterm_update_core::git_https::Trace<'_>,
    ) -> std::result::Result<T, aterm_update_core::git_https::Failure>,
    on_retry: impl FnMut(aterm_types::http_retry::Retry),
) -> Result<T> {
    aterm_update_core::git_https::read(true, attempt, on_retry).map_err(|failure| {
        Error::cannot_run(format!(
            "GNU Git qualification anonymous HTTPS read: {failure}"
        ))
    })
}

pub(crate) fn qualify_staged(
    result: &BottleOutcome,
    stage: &Path,
    row: &atpkg::manifest::Artifact,
) -> Result<()> {
    let work = crate::channel::Scratch::new("gnu-git-qualification")?;
    let git = stage.join("bin/git");
    let global = work.path().join("global.gitconfig");
    std::fs::write(&global, b"")
        .map_err(|error| Error::io("write qualification Git config", &error))?;
    let run = |args: &[&str]| -> Result<std::process::Output> {
        let mut cmd = aterm_uds::spawnfd::command(&git);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", work.path())
            .env("GIT_CONFIG_GLOBAL", &global)
            .args(["-c", "core.hooksPath=/dev/null", "-c", "credential.helper="])
            .args(args)
            .current_dir(work.path());
        let out = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .map_err(|error| Error::cannot_run(format!("GNU Git qualification: {error}")))?;
        if !out.status.success() {
            return Err(Error::refused(format!(
                "GNU Git qualification {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(out)
    };
    let anonymous_read = |args: &[&str]| -> Result<std::process::Output> {
        anonymous_git_qualification(
            |_, trace| {
                let mut cmd = aterm_uds::spawnfd::command(&git);
                cmd.env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", work.path())
                    .env("GIT_CONFIG_GLOBAL", &global)
                    .args(["-c", "core.hooksPath=/dev/null", "-c", "credential.helper="])
                    .args(args)
                    .current_dir(work.path());
                trace.configure(&mut cmd);
                let out = aterm_uds::deadline::output_until(
                    &mut cmd,
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                )
                .map_err(|error| match error {
                    aterm_uds::deadline::RunError::TimedOut => {
                        aterm_update_core::git_https::Failure::timeout(
                            "GNU Git qualification did not finish in 60 seconds",
                        )
                    }
                    other => aterm_update_core::git_https::Failure::local(format!(
                        "GNU Git qualification: {other}"
                    )),
                })?;
                if out.status.success() {
                    Ok(out)
                } else {
                    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    Err(aterm_update_core::git_https::Failure::diagnostic(
                        if said.is_empty() {
                            format!(
                                "GNU Git qualification {} exited {:?}",
                                args.first().unwrap_or(&""),
                                out.status.code()
                            )
                        } else {
                            said
                        },
                    ))
                }
            },
            |retry| std::thread::sleep(retry.pause),
        )
    };
    let version = run(&["--no-lazy-fetch", "--version"])?;
    if String::from_utf8_lossy(&version.stdout).trim() != format!("git version {}", result.version)
    {
        return Err(Error::refused(
            "GNU Git executable version differs from the pinned package",
        ));
    }
    let default_ca = run(&["config", "--path", "--get", "http.sslCAInfo"])?;
    if String::from_utf8_lossy(&default_ca.stdout).trim()
        != stage
            .join("share/ca-certificates/cacert.pem")
            .to_string_lossy()
    {
        return Err(Error::refused(
            "GNU Git default certificate bundle did not follow its relocated prefix",
        ));
    }
    std::fs::write(
        &global,
        b"[http]\nsslCAInfo = /explicit/user/certificate.pem\n",
    )
    .map_err(|error| Error::io("write explicit qualification Git SSL choice", &error))?;
    let explicit_ca = run(&["config", "--path", "--get", "http.sslCAInfo"])?;
    if String::from_utf8_lossy(&explicit_ca.stdout).trim() != "/explicit/user/certificate.pem" {
        return Err(Error::refused(
            "GNU Git default certificate config overrode an explicit global choice",
        ));
    }
    std::fs::write(&global, b"")
        .map_err(|error| Error::io("remove qualification SSL override", &error))?;
    run(&["init", "source"])?;
    std::fs::write(work.path().join("source/probe"), b"GNU Git relocation\n")
        .map_err(|error| Error::io("write qualification input", &error))?;
    run(&["-C", "source", "add", "probe"])?;
    run(&[
        "-C",
        "source",
        "-c",
        "user.name=Package qualification",
        "-c",
        "user.email=package@example.invalid",
        "commit",
        "--no-gpg-sign",
        "-m",
        "probe",
    ])?;
    run(&["clone", "--no-hardlinks", "source", "checkout"])?;
    if std::fs::read(work.path().join("checkout/probe"))
        .ok()
        .as_deref()
        != Some(b"GNU Git relocation\n")
    {
        return Err(Error::refused(
            "GNU Git local clone/checkout did not preserve its input",
        ));
    }
    anonymous_read(&["ls-remote", "https://github.com/git/git.git", "HEAD"])?;
    if Triple::host() == Some(result.triple) {
        let layout = atpkg::store::Layout {
            prefix: work.path().join("native-client-acceptance"),
        };
        let native = layout.build_dir(&result.program, result.build);
        atpkg::verify_and_stage(
            row,
            &result.asset,
            &native,
            &atpkg::install::StageHooks::NONE,
        )
        .map_err(|error| Error::refused(format!("native GNU client admission: {error}")))?;
        if !atpkg::store::ready_marker_path(&native).is_some_and(|marker| marker.is_file()) {
            return Err(Error::refused(
                "native GNU client admission did not produce its ready marker",
            ));
        }
    }
    Ok(())
}

/// Prepare a qualified upstream target without release credentials or a publishing effect.
///
/// # Errors
/// Malformed options or any acquisition/packing/qualification refusal.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut args = args.iter();
    let program = args
        .next()
        .filter(|name| !name.starts_with('-'))
        .ok_or_else(|| Error::usage(SYNOPSIS))?
        .clone();
    let (mut version, mut build, mut triple, mut merge_from, mut out) =
        (None, None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| Error::usage(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--version" if version.is_none() => version = Some(value.clone()),
            "--build" if build.is_none() => {
                build = Some(
                    value
                        .parse()
                        .map_err(|_| Error::usage("--build needs a positive integer"))?,
                )
            }
            "--triple" if triple.is_none() => triple = Some(Triple::parse(value)?),
            "--merge-from" if merge_from.is_none() => merge_from = Some(PathBuf::from(value)),
            "--out" if out.is_none() => out = Some(PathBuf::from(value)),
            _ => {
                return Err(Error::usage(format!(
                    "unsupported or repeated option {flag}; {SYNOPSIS}"
                )));
            }
        }
    }
    let result = pack(
        ctx,
        &Request {
            program,
            version: version.ok_or_else(|| Error::usage("--version is required"))?,
            build: build
                .filter(|build| *build > 0)
                .ok_or_else(|| Error::usage("--build must be positive"))?,
            triple: triple.ok_or_else(|| Error::usage("--triple is required"))?,
            merge_from,
            out: out.unwrap_or_else(|| ctx.state_dir.join("upstream-prepared")),
        },
    )?;
    println!(
        "qualified {}@{} {} for {} (unsigned, not uploaded): {}",
        result.program,
        result.build,
        result.version,
        result.triple,
        result.manifest.display()
    );
    Ok(())
}

#[cfg(test)]
#[path = "upstream_tests.rs"]
mod tests;

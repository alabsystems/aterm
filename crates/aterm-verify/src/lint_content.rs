// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content proofs for carrying one package's qualification lint across release trees.
//!
//! Cargo's resolved graph supplies a conservative source closure, without package opt-ins.
//! Every byte in that closure participates, so dependency implementation and public-item
//! changes both invalidate consumers. Executable readers (build scripts, proc macros and
//! native links) and opaque source readers remain unbounded, including in external dependencies.
//! Qualification owns the immutable candidate; pre-dispatch and final reads detect drift.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use aterm_uds::output::CommandOutput as _;
use serde::{Deserialize, Serialize};

use crate::exec::Cmd;

/// The lint-package proof wire format.
pub const SCHEMA: u32 = 1;

/// One prior package lint which completed successfully.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarryProof {
    pub schema: u32,
    pub spec: String,
    pub outcome: String,
    pub content_key: Option<String>,
    pub inputs_bounded: bool,
    pub input_reason: Option<String>,
    pub execution_s: f64,
}

/// Prior package lint successes, by exact Cargo package spec.
pub type Passed = BTreeMap<String, CarryProof>;

/// The current complete-input identity of one package lint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub content_key: Option<String>,
    pub reason: Option<String>,
}

impl Evidence {
    #[must_use]
    pub const fn bounded(&self) -> bool {
        self.content_key.is_some() && self.reason.is_none()
    }
}

impl CarryProof {
    /// A proof row from an execution which passed.
    #[must_use]
    pub fn passed(spec: impl Into<String>, evidence: &Evidence, execution_s: f64) -> Self {
        Self {
            schema: SCHEMA,
            spec: spec.into(),
            outcome: "ok".into(),
            content_key: evidence.content_key.clone(),
            inputs_bounded: evidence.bounded(),
            input_reason: evidence.reason.clone(),
            execution_s,
        }
    }

    fn validate(&self, line: usize) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "line {line} has proof schema {}, expected {SCHEMA}",
                self.schema
            ));
        }
        if self.spec.trim().is_empty() || self.spec != self.spec.trim() {
            return Err(format!(
                "line {line} has an empty or untrimmed package spec"
            ));
        }
        if self.outcome != "ok" {
            return Err(format!(
                "line {line} has outcome {:?}; only a prior ok may be carried",
                self.outcome
            ));
        }
        if !self.execution_s.is_finite() || self.execution_s < 0.0 {
            return Err(format!("line {line} has an invalid execution_s"));
        }
        if self
            .content_key
            .as_deref()
            .is_some_and(|key| !aterm_codec::hex::is_lower(key, 64))
        {
            return Err(format!("line {line} has an invalid content key"));
        }
        if self.inputs_bounded {
            if self.content_key.is_none() || self.input_reason.is_some() {
                return Err(format!(
                    "line {line} says inputs are bounded without a key, or with a reason"
                ));
            }
        } else if self.content_key.is_some()
            || self
                .input_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(format!(
                "line {line} says inputs are unbounded without exactly one nonempty reason"
            ));
        }
        Ok(())
    }
}

/// Read versioned JSONL package proofs.
///
/// # Errors
/// A row is malformed, is not a prior `ok`, contradicts its boundedness, or repeats a spec.
pub fn read_passed(text: &str) -> Result<Passed, String> {
    let mut passed = Passed::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = index + 1;
        let proof: CarryProof = aterm_json::from_str(line)
            .map_err(|error| format!("line {line_number} is not a lint-package proof: {error}"))?;
        proof.validate(line_number)?;
        if passed.insert(proof.spec.clone(), proof).is_some() {
            return Err(format!("line {line_number} repeats a package spec"));
        }
    }
    Ok(passed)
}

/// Encode package proofs in spec order as versioned JSONL.
///
/// # Errors
/// A caller constructed a proof [`read_passed`] would refuse, or JSON serialization fails.
pub fn encode_passed(passed: &Passed) -> Result<String, String> {
    let mut text = String::new();
    for proof in passed.values() {
        proof.validate(text.lines().count() + 1)?;
        text.push_str(&aterm_json::to_string(proof).map_err(|error| error.to_string())?);
        text.push('\n');
    }
    Ok(text)
}

/// One current workspace package and its freshly computed proof input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Package {
    pub spec: String,
    pub dir: String,
    pub evidence: Evidence,
    pub commands: Vec<Cmd>,
}

/// Current package evidence and the graph used to invalidate reverse dependents.
pub(crate) struct Plan {
    pub packages: Vec<Package>,
    reverse: Option<BTreeMap<String, BTreeSet<String>>>,
}

/// A release lint's partition. `run` is the only scope Tippy receives.
pub(crate) struct Selection {
    pub run: Vec<Package>,
    pub carried: Vec<(Package, CarryProof)>,
}

impl Plan {
    /// Build a deterministic package graph for the production scheduler's line-timeline tests.
    #[cfg(test)]
    pub(crate) fn fixture(
        packages: Vec<Package>,
        reverse: Option<BTreeMap<String, BTreeSet<String>>>,
    ) -> Self {
        Self { packages, reverse }
    }

    /// Carry only exact bounded prior OKs which are not reverse dependents of any changed or
    /// unbounded package. An unreadable dependency graph widens to running every package.
    pub(crate) fn select(self, passed: &Passed) -> Selection {
        let exact: BTreeSet<String> = self
            .packages
            .iter()
            .filter(|package| {
                package.evidence.bounded()
                    && passed.get(&package.spec).is_some_and(|prior| {
                        prior.outcome == "ok"
                            && prior.inputs_bounded
                            && prior.content_key == package.evidence.content_key
                    })
            })
            .map(|package| package.spec.clone())
            .collect();
        if exact.len() == self.packages.len() {
            return partition(self.packages, passed, &BTreeSet::new());
        }
        if exact.is_empty() {
            return Selection {
                run: self.packages,
                carried: Vec::new(),
            };
        }
        let mut invalidated: BTreeSet<String> = self
            .packages
            .iter()
            .filter(|package| !exact.contains(&package.spec))
            .map(|package| package.spec.clone())
            .collect();
        let Some(reverse) = self.reverse else {
            return Selection {
                run: self.packages,
                carried: Vec::new(),
            };
        };
        let mut pending: VecDeque<String> = invalidated.iter().cloned().collect();
        while let Some(seed) = pending.pop_front() {
            let Some(dependents) = reverse.get(&seed) else {
                continue;
            };
            for dependent in dependents {
                if invalidated.insert(dependent.clone()) {
                    pending.push_back(dependent.clone());
                }
            }
        }
        partition(self.packages, passed, &invalidated)
    }
}

fn partition(packages: Vec<Package>, passed: &Passed, invalidated: &BTreeSet<String>) -> Selection {
    let (mut run, mut carried) = (Vec::new(), Vec::new());
    for package in packages {
        let prior = passed.get(&package.spec);
        let matches = !invalidated.contains(&package.spec)
            && package.evidence.bounded()
            && prior.is_some_and(|prior| {
                prior.outcome == "ok"
                    && prior.inputs_bounded
                    && prior.content_key == package.evidence.content_key
            });
        if matches {
            carried.push((package, prior.expect("matched lint proof").clone()));
        } else {
            run.push(package);
        }
    }
    Selection { run, carried }
}

/// Compute every current workspace package's evidence. `commands` must return every finalized,
/// canonical Tippy invocation used for that one package.
pub(crate) fn plan(
    ctx: &crate::Ctx,
    mut commands: impl FnMut(&str) -> Result<Vec<Cmd>, String>,
) -> Result<Plan, String> {
    let members = crate::changed::read_members(&ctx.root, &ctx.tools, &ctx.path_env);
    if members.is_empty() {
        return Err("Cargo did not report any workspace member".into());
    }
    let graph = CargoGraph::read(ctx)?;
    let builder = EvidenceBuilder::new(ctx, &graph);
    let member_packages = members.lint_packages();
    let reverse = graph.reverse_packages(&ctx.root, &member_packages);
    let mut packages = Vec::new();
    for (spec, dir) in member_packages {
        let (evidence, package_commands) = match commands(&spec) {
            Ok(package_commands) => (
                builder.evidence(&spec, &dir, &package_commands),
                package_commands,
            ),
            Err(why) => (
                Evidence {
                    content_key: None,
                    reason: Some(format!("the exact lint commands are unbounded: {why}")),
                },
                Vec::new(),
            ),
        };
        packages.push(Package {
            spec,
            dir,
            evidence,
            commands: package_commands,
        });
    }
    Ok(Plan { packages, reverse })
}

struct EvidenceBuilder<'a> {
    ctx: &'a crate::Ctx,
    graph: &'a CargoGraph,
    context: Result<&'a str, String>,
    verifier: Result<&'static str, String>,
    compiler_tools: OnceLock<Result<String, String>>,
    workspace_settings: OnceLock<Result<String, String>>,
}

impl<'a> EvidenceBuilder<'a> {
    fn new(ctx: &'a crate::Ctx, graph: &'a CargoGraph) -> Self {
        Self {
            ctx,
            graph,
            context: ctx
                .carry_context
                .as_deref()
                .filter(|value| aterm_codec::hex::is_lower(value, 64))
                .ok_or_else(|| "the release supplied no valid carry context".to_owned()),
            verifier: crate::report::running_driver()
                .filter(|value| aterm_codec::hex::is_lower(value, 64))
                .ok_or_else(|| "the running verifier cannot be hashed".to_owned()),
            compiler_tools: OnceLock::new(),
            workspace_settings: OnceLock::new(),
        }
    }

    fn evidence(&self, spec: &str, dir: &str, commands: &[Cmd]) -> Evidence {
        match self.content_key(spec, dir, commands) {
            Ok(key) => Evidence {
                content_key: Some(key),
                reason: None,
            },
            Err(reason) => Evidence {
                content_key: None,
                reason: Some(reason),
            },
        }
    }

    fn content_key(&self, spec: &str, dir: &str, commands: &[Cmd]) -> Result<String, String> {
        let context = self.context.clone()?;
        let verifier = self.verifier.clone()?;
        let tippy = shared_command_program(commands)?;
        if commands
            .iter()
            .any(|command| !command.exact_env || !command.runtime_environment_is_finalized())
        {
            return Err(
                "a lint command does not declare its finalized complete environment".into(),
            );
        }
        let root = package_root(&self.ctx.root, dir)?;
        let closure = self.graph.closure(&root)?;
        let mut hash = aterm_digest::Sha256::new();
        part(&mut hash, b"domain", b"aterm-lint-package-content-v3");
        part(&mut hash, b"spec", spec.as_bytes());
        part(&mut hash, b"carry-context", context.as_bytes());
        part(&mut hash, b"verifier", verifier.as_bytes());
        part(
            &mut hash,
            b"locked-metadata",
            self.graph.package_metadata(&closure)?.as_bytes(),
        );
        part(
            &mut hash,
            b"lockfile-slice",
            self.graph
                .lockfile_slice(&self.ctx.root, &closure)?
                .as_bytes(),
        );
        let settings = self
            .workspace_settings
            .get_or_init(|| self.graph.workspace_settings_digest())
            .clone()?;
        part(&mut hash, b"workspace-settings", settings.as_bytes());
        for root in &closure {
            part(&mut hash, b"dependency-root", &os_bytes(root.as_os_str()));
            source_readers_bounded(root)?;
            hash_tree(&mut hash, root, root)?;
        }
        hash_global_files(&mut hash, self.ctx, commands, &closure, tippy)?;
        let compiler_tools = self
            .compiler_tools
            .get_or_init(|| compiler_tools_digest(self.ctx, tippy))
            .clone()?;
        part(
            &mut hash,
            b"compiler-tool-closure",
            compiler_tools.as_bytes(),
        );
        for command in commands {
            hash_command_identity(&mut hash, self.ctx, command)?;
        }
        Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
    }
}

/// Common inputs freshly captured once at a lint stability boundary.
///
/// The scheduler captures one pre-dispatch snapshot after the unbounded batch and before its first
/// bounded miss, then a distinct final snapshot after all lint execution. Locked metadata and
/// compiler executable identities are common and read once in each
/// snapshot. [`Self::evidence`] still rereads each package closure and effective config. The final
/// snapshot detects persistent common-input drift. Qualification must keep its candidate
/// immutable while judging; no pair of snapshots can detect transient mutate-and-restore activity.
pub(crate) struct BoundarySnapshot<'a> {
    ctx: &'a crate::Ctx,
    graph: CargoGraph,
    context: &'a str,
    verifier: &'static str,
    compiler_tools: String,
    workspace_settings: String,
    tippy_program: PathBuf,
    tippy_physical: PathBuf,
}

impl<'a> BoundarySnapshot<'a> {
    pub(crate) fn new(ctx: &'a crate::Ctx, tippy: &Path) -> Result<Self, String> {
        let context = ctx
            .carry_context
            .as_deref()
            .filter(|value| aterm_codec::hex::is_lower(value, 64))
            .ok_or_else(|| "the release supplied no valid carry context".to_owned())?;
        let verifier = crate::report::running_driver()
            .filter(|value| aterm_codec::hex::is_lower(value, 64))
            .ok_or_else(|| "the running verifier cannot be hashed".to_owned())?;
        let graph = CargoGraph::read(ctx)?;
        let compiler_tools = compiler_tools_digest(ctx, tippy)?;
        let workspace_settings = graph.workspace_settings_digest()?;
        let tippy_physical = tippy
            .canonicalize()
            .map_err(|error| format!("Tippy executable {}: {error}", tippy.display()))?;
        Ok(Self {
            ctx,
            graph,
            context,
            verifier,
            compiler_tools,
            workspace_settings,
            tippy_program: tippy.to_path_buf(),
            tippy_physical,
        })
    }

    #[must_use]
    pub(crate) fn evidence(&self, spec: &str, dir: &str, commands: &[Cmd]) -> Evidence {
        if let Err(reason) =
            boundary_program_matches(&self.tippy_program, &self.tippy_physical, commands)
        {
            return Evidence {
                content_key: None,
                reason: Some(reason),
            };
        }
        EvidenceBuilder {
            ctx: self.ctx,
            graph: &self.graph,
            context: Ok(self.context),
            verifier: Ok(self.verifier),
            compiler_tools: OnceLock::from(Ok(self.compiler_tools.clone())),
            workspace_settings: OnceLock::from(Ok(self.workspace_settings.clone())),
        }
        .evidence(spec, dir, commands)
    }
}

fn boundary_program_matches(
    expected_program: &Path,
    expected_physical: &Path,
    commands: &[Cmd],
) -> Result<(), String> {
    let program = shared_command_program(commands)?;
    if program != expected_program {
        return Err(format!(
            "the lint command program {} differs from boundary Tippy {}",
            program.display(),
            expected_program.display()
        ));
    }
    let physical = program
        .canonicalize()
        .map_err(|error| format!("Tippy executable {}: {error}", program.display()))?;
    if physical != expected_physical {
        return Err(format!(
            "the lint command program {} resolves away from boundary Tippy {}",
            program.display(),
            expected_physical.display()
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    workspace_members: Vec<String>,
    resolve: Option<MetadataResolve>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<MetadataTarget>,
    links: Option<String>,
}

#[derive(Deserialize)]
struct MetadataTarget {
    kind: Vec<String>,
    #[serde(default)]
    src_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct MetadataResolve {
    nodes: Vec<MetadataNode>,
}

#[derive(Deserialize)]
struct MetadataNode {
    id: String,
    dependencies: Vec<String>,
}

/// The locked graph supplies source closures and resolved node features. Each package key
/// binds its projection, including features unified by otherwise unrelated workspace members.
struct CargoGraph {
    digest: String,
    metadata: Option<aterm_json::Value>,
    workspace: BTreeSet<String>,
    packages: BTreeMap<String, MetadataPackage>,
    nodes: BTreeMap<String, Vec<String>>,
    package_by_root: BTreeMap<PathBuf, String>,
}

impl CargoGraph {
    fn read(ctx: &crate::Ctx) -> Result<Self, String> {
        let output = aterm_uds::spawnfd::command(&ctx.tools.targo)
            .current_dir(&ctx.root)
            .env("PATH", &ctx.path_env)
            .args(locked_metadata_args())
            .output_owned()
            .map_err(|error| format!("cannot read locked Cargo metadata: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "locked Cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| "locked Cargo metadata is not UTF-8".to_owned())?;
        Self::parse(&text)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let metadata: Metadata = aterm_json::from_str(text)
            .map_err(|error| format!("locked Cargo metadata does not decode: {error}"))?;
        let resolve = metadata
            .resolve
            .ok_or_else(|| "locked Cargo metadata has no resolved graph".to_owned())?;
        let workspace: BTreeSet<String> = metadata.workspace_members.into_iter().collect();
        let mut packages = BTreeMap::new();
        let mut package_by_root = BTreeMap::new();
        for package in metadata.packages {
            let manifest = package.manifest_path.canonicalize().map_err(|error| {
                format!(
                    "metadata manifest {} cannot be resolved: {error}",
                    package.manifest_path.display()
                )
            })?;
            let root = manifest
                .parent()
                .ok_or_else(|| format!("metadata manifest {} has no parent", manifest.display()))?
                .to_path_buf();
            package_by_root.insert(root, package.id.clone());
            packages.insert(package.id.clone(), package);
        }
        let nodes = resolve
            .nodes
            .into_iter()
            .map(|node| (node.id, node.dependencies))
            .collect();
        let mut digest = aterm_digest::Sha256::new();
        part(&mut digest, b"locked-metadata-json", text.as_bytes());
        Ok(Self {
            digest: aterm_codec::hex::encode_lower(&digest.finalize()),
            metadata: Some(aterm_json::from_str(text).map_err(|e| e.to_string())?),
            workspace,
            packages,
            nodes,
            package_by_root,
        })
    }

    /// A bounded package's complete resolved source closure. Build scripts, proc macros and
    /// native-link packages remain unbounded: their source bytes do not bound what they execute.
    fn closure(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        let id = self.package_by_root.get(root).ok_or_else(|| {
            format!(
                "locked Cargo metadata does not identify package root {}",
                root.display()
            )
        })?;
        let mut pending = VecDeque::from([id.clone()]);
        let mut seen = BTreeSet::new();
        let mut roots = Vec::new();
        while let Some(id) = pending.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let package = self
                .packages
                .get(&id)
                .ok_or_else(|| format!("locked Cargo metadata omits package {id:?}"))?;
            if package.links.is_some() {
                return Err(format!(
                    "the lint dependency {id:?} declares native links and has unbounded tool inputs"
                ));
            }
            if package
                .targets
                .iter()
                .flat_map(|target| &target.kind)
                .any(|kind| matches!(kind.as_str(), "custom-build" | "proc-macro"))
            {
                return Err(format!(
                    "the lint dependency {id:?} executes a build script or proc macro with undeclared runtime inputs"
                ));
            }
            let manifest = package.manifest_path.canonicalize().map_err(|error| {
                format!(
                    "metadata manifest {} cannot be resolved: {error}",
                    package.manifest_path.display()
                )
            })?;
            let root = manifest
                .parent()
                .ok_or_else(|| format!("metadata manifest {} has no parent", manifest.display()))?
                .to_path_buf();
            for target in &package.targets {
                let source = target
                    .src_path
                    .as_ref()
                    .ok_or_else(|| format!("metadata has no source path for a target of {id:?}"))?
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                if !source.starts_with(&root) {
                    return Err(format!(
                        "target {} lies outside its package",
                        source.display()
                    ));
                }
                if source.extension().is_none_or(|extension| extension != "rs") {
                    return Err(format!(
                        "target {} has an unknown source kind",
                        source.display()
                    ));
                }
            }
            roots.push(root);
            let dependencies = self
                .nodes
                .get(&id)
                .ok_or_else(|| format!("locked Cargo resolve omits package {id:?}"))?;
            pending.extend(dependencies.iter().cloned());
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }

    fn reverse_packages(
        &self,
        workspace_root: &Path,
        members: &[(String, String)],
    ) -> Option<BTreeMap<String, BTreeSet<String>>> {
        let mut id_to_spec = BTreeMap::new();
        for (spec, directory) in members {
            let root = workspace_root.join(directory).canonicalize().ok()?;
            let id = self.package_by_root.get(&root)?;
            id_to_spec.insert(id.clone(), spec.clone());
        }
        if self
            .workspace
            .iter()
            .any(|id| !id_to_spec.contains_key(id) || !self.nodes.contains_key(id))
        {
            return None;
        }
        let mut reverse: BTreeMap<String, BTreeSet<String>> = id_to_spec
            .values()
            .map(|spec| (spec.clone(), BTreeSet::new()))
            .collect();
        for (dependent, dependencies) in &self.nodes {
            let Some(dependent_spec) = id_to_spec.get(dependent) else {
                continue;
            };
            for dependency in dependencies {
                let Some(dependency_spec) = id_to_spec.get(dependency) else {
                    continue;
                };
                reverse
                    .get_mut(dependency_spec)?
                    .insert(dependent_spec.clone());
            }
        }
        Some(reverse)
    }

    fn package_metadata(&self, closure: &[PathBuf]) -> Result<String, String> {
        let Some(metadata) = &self.metadata else {
            return Ok(self.digest.clone());
        };
        let ids = self.closure_ids(closure)?;
        let mut projection = Vec::new();
        for field in ["packages", "resolve"] {
            let rows = if field == "packages" {
                &metadata[field]
            } else {
                &metadata[field]["nodes"]
            };
            let rows = rows
                .as_array()
                .ok_or_else(|| format!("metadata has no {field} rows"))?;
            for row in rows {
                if row["id"].as_str().is_some_and(|id| ids.contains(id)) {
                    projection.push(row.clone());
                }
            }
        }
        aterm_json::to_string(&projection).map_err(|e| e.to_string())
    }

    /// Cargo's feature unification may make another member's dependency settings relevant.
    /// Until the compiler supplies a per-command feature projection, bind those settings
    /// conservatively. Each closure's source and lock rows retain its own exact version.
    fn workspace_settings_digest(&self) -> Result<String, String> {
        let mut hash = aterm_digest::Sha256::new();
        for id in &self.workspace {
            let package = self
                .packages
                .get(id)
                .ok_or("workspace package missing from metadata")?;
            let text =
                std::fs::read_to_string(&package.manifest_path).map_err(|e| e.to_string())?;
            let mut value: aterm_toml::Value =
                aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
            remove_field(&mut value, &["package", "version"]);
            part(
                &mut hash,
                b"manifest",
                &os_bytes(package.manifest_path.as_os_str()),
            );
            part(
                &mut hash,
                b"settings",
                aterm_toml::to_string(&value)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            );
        }
        Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
    }

    fn lockfile_slice(&self, root: &Path, closure: &[PathBuf]) -> Result<String, String> {
        let Some(metadata) = &self.metadata else {
            return Ok(self.digest.clone());
        };
        let ids = self.closure_ids(closure)?;
        let text = std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| e.to_string())?;
        let lock: aterm_toml::Value = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
        let rows = lock
            .get("package")
            .and_then(aterm_toml::Value::as_array)
            .ok_or_else(|| "lockfile has no package rows".to_owned())?;
        let packages = metadata["packages"]
            .as_array()
            .ok_or_else(|| "metadata has no packages".to_owned())?;
        let mut projection = Vec::new();
        for package in packages
            .iter()
            .filter(|p| p["id"].as_str().is_some_and(|id| ids.contains(id)))
        {
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| {
                    ["name", "version", "source"].into_iter().all(|field| {
                        row.get(field).and_then(aterm_toml::Value::as_str)
                            == package[field].as_str()
                    })
                })
                .collect();
            if matches.len() != 1 {
                return Err("a dependency has no unique locked package identity".into());
            }
            projection.push(matches[0].clone());
        }
        aterm_toml::to_string(&aterm_toml::Value::Table(
            [("package".to_owned(), aterm_toml::Value::Array(projection))]
                .into_iter()
                .collect(),
        ))
        .map_err(|e| e.to_string())
    }

    fn closure_ids(&self, closure: &[PathBuf]) -> Result<BTreeSet<String>, String> {
        closure
            .iter()
            .map(|root| {
                self.package_by_root.get(root).cloned().ok_or_else(|| {
                    format!("the resolved graph has no identity for {}", root.display())
                })
            })
            .collect()
    }
}

fn locked_metadata_args() -> [&'static str; 7] {
    [
        "metadata",
        "--locked",
        "--all-features",
        "--format-version",
        "1",
        "--color",
        "never",
    ]
}

fn hash_global_files(
    hash: &mut aterm_digest::Sha256,
    ctx: &crate::Ctx,
    commands: &[Cmd],
    source_roots: &[PathBuf],
    tippy: &Path,
) -> Result<(), String> {
    for file in [
        "Cargo.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
        "trust.lock",
        "clippy.toml",
        ".clippy.toml",
    ] {
        if file == "Cargo.toml" {
            // Cargo.lock is represented by the resolved metadata and each closure's exact
            // package rows, including source identities and registry checksums.
            // Only the product version can leave the root manifest projection: all other
            // settings stay exact, and any closure inheriting the version retains it too.
            let text = std::fs::read_to_string(ctx.root.join(file)).map_err(|e| e.to_string())?;
            let mut value: aterm_toml::Value =
                aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
            if let Some(profile) = value.get("profile") {
                bounded_config_readers(profile)?;
            }
            let inherits_version = source_roots.iter().any(|root| {
                std::fs::read_to_string(root.join("Cargo.toml"))
                    .ok()
                    .and_then(|text| aterm_toml::from_str::<aterm_toml::Value>(&text).ok())
                    .map(|value| {
                        value
                            .get("package")
                            .and_then(|v| v.get("version"))
                            .and_then(|v| v.get("workspace"))
                            .and_then(aterm_toml::Value::as_bool)
                            .unwrap_or(false)
                    })
                    .unwrap_or(true)
            });
            if !inherits_version {
                remove_field(&mut value, &["workspace", "package", "version"]);
            }
            part(
                hash,
                b"workspace-global",
                aterm_toml::to_string(&value)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            );
        } else {
            hash_optional_file(hash, b"workspace-global", &ctx.root.join(file))?;
        }
    }

    let allowed_tools = allowed_compiler_tools(ctx, tippy)?;
    let mut config_dirs = BTreeSet::new();
    let mut clippy_dirs = BTreeSet::new();
    let mut ancestor = Some(ctx.root.as_path());
    while let Some(directory) = ancestor {
        config_dirs.insert(directory.join(".cargo"));
        clippy_dirs.insert(directory.to_path_buf());
        ancestor = directory.parent();
    }
    for source_root in source_roots {
        let mut ancestor = Some(source_root.as_path());
        while let Some(directory) = ancestor {
            clippy_dirs.insert(directory.to_path_buf());
            ancestor = directory.parent();
        }
    }
    for command in commands {
        let env = command_environment(command);
        for (name, value) in &env {
            let name = String::from_utf8_lossy(name);
            let value = String::from_utf8_lossy(value);
            if name == "RUSTFLAGS" || name.ends_with("_RUSTFLAGS") {
                bounded_rustflags(&value)?;
            }
            if name == "RUST_TARGET_PATH" && !value.is_empty() {
                return Err("RUST_TARGET_PATH can select an unbounded target definition".into());
            }
            if name == "CARGO_BUILD_TARGET" {
                bounded_target(&value)?;
            }
            if !value.is_empty() && compiler_reader_environment(&name) {
                return Err(format!("{name} can select unbounded compiler inputs"));
            }
        }
        hash_environment_tool_selectors(hash, &env, &allowed_tools)?;
        if let Some(value) = env.get(b"CLIPPY_CONF_DIR".as_slice()) {
            clippy_dirs.insert(absolute_directory("CLIPPY_CONF_DIR", value)?);
        }
        let cargo_home = env
            .get(b"CARGO_HOME".as_slice())
            .map(|value| PathBuf::from(os_from_bytes(value)))
            .or_else(|| {
                env.get(b"HOME".as_slice())
                    .map(|value| PathBuf::from(os_from_bytes(value)).join(".cargo"))
            })
            .ok_or_else(|| {
                "the exact lint environment declares neither CARGO_HOME nor HOME".to_owned()
            })?;
        if !cargo_home.is_absolute() {
            return Err(format!(
                "the exact lint CARGO_HOME {} is not absolute",
                cargo_home.display()
            ));
        }
        config_dirs.insert(cargo_home);

        let cwd = command
            .cwd
            .as_deref()
            .ok_or_else(|| "the exact lint command has no working directory".to_owned())?;
        if !cwd.is_absolute() {
            return Err(format!(
                "the exact lint working directory {} is not absolute",
                cwd.display()
            ));
        }
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("lint working directory {}: {error}", cwd.display()))?;
        if !cwd.is_dir() {
            return Err(format!(
                "the exact lint working directory {} is not a directory",
                cwd.display()
            ));
        }
        let mut ancestor = Some(cwd.as_path());
        while let Some(directory) = ancestor {
            config_dirs.insert(directory.join(".cargo"));
            clippy_dirs.insert(directory.to_path_buf());
            ancestor = directory.parent();
        }
        hash_command_configs(hash, command, &allowed_tools, &mut clippy_dirs)?;
    }
    for directory in config_dirs {
        for name in ["config", "config.toml"] {
            hash_cargo_config(
                hash,
                &directory.join(name),
                &allowed_tools,
                &mut clippy_dirs,
            )?;
        }
    }
    for directory in clippy_dirs {
        hash_clippy_directory(hash, &directory)?;
    }
    Ok(())
}

fn remove_field(value: &mut aterm_toml::Value, path: &[&str]) {
    let Some((name, tail)) = path.split_first() else {
        return;
    };
    let Some(table) = value.as_table_mut() else {
        return;
    };
    let Some(mut child) = table.remove(name) else {
        return;
    };
    if !tail.is_empty() {
        remove_field(&mut child, tail);
        table.insert((*name).into(), child);
    }
}

const TOOL_ENV_SELECTORS: [&str; 6] = [
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

fn compiler_tool_paths(ctx: &crate::Ctx, tippy: &Path) -> Vec<PathBuf> {
    let driver = tippy
        .parent()
        .unwrap_or(&ctx.tools.stage2_dir)
        .join(crate::toolchain::tool_file("tippy-driver"));
    vec![
        tippy.to_path_buf(),
        driver,
        ctx.tools.targo.clone(),
        ctx.tools
            .stage2_dir
            .join(crate::toolchain::tool_file("trustc")),
        ctx.tools
            .stage2_dir
            .join(crate::toolchain::tool_file("trustd")),
    ]
}

fn allowed_compiler_tools(ctx: &crate::Ctx, tippy: &Path) -> Result<BTreeSet<PathBuf>, String> {
    compiler_tool_paths(ctx, tippy)
        .into_iter()
        .map(|path| {
            path.canonicalize()
                .map_err(|error| format!("declared compiler tool {}: {error}", path.display()))
        })
        .collect()
}

fn hash_environment_tool_selectors(
    hash: &mut aterm_digest::Sha256,
    environment: &BTreeMap<Vec<u8>, Vec<u8>>,
    allowed: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    for name in TOOL_ENV_SELECTORS {
        if let Some(value) = environment.get(name.as_bytes()) {
            hash_selected_tool(hash, name, value, allowed)?;
        }
    }
    Ok(())
}

fn hash_selected_tool(
    hash: &mut aterm_digest::Sha256,
    selector: &str,
    value: &[u8],
    allowed: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    if value.is_empty() && selector.contains("WRAPPER") {
        part(hash, b"empty-tool-selector", selector.as_bytes());
        return Ok(());
    }
    let path = PathBuf::from(os_from_bytes(value));
    if !path.is_absolute() {
        return Err(format!(
            "{selector} selects the unbounded non-absolute compiler tool {}",
            path.display()
        ));
    }
    let physical = path
        .canonicalize()
        .map_err(|error| format!("{selector} compiler tool {}: {error}", path.display()))?;
    if !allowed.contains(&physical) {
        return Err(format!(
            "{selector} selects undeclared compiler tool {}",
            path.display()
        ));
    }
    part(hash, b"compiler-tool-selector", selector.as_bytes());
    part(hash, b"selected-tool-path", &os_bytes(path.as_os_str()));
    hash_compiler_tool(hash, &physical)
}

fn absolute_directory(name: &str, value: &[u8]) -> Result<PathBuf, String> {
    let path = PathBuf::from(os_from_bytes(value));
    if !path.is_absolute() {
        return Err(format!(
            "{name} directory {} is not absolute",
            path.display()
        ));
    }
    let physical = path
        .canonicalize()
        .map_err(|error| format!("{name} directory {}: {error}", path.display()))?;
    if !physical.is_dir() {
        return Err(format!("{name} {} is not a directory", path.display()));
    }
    Ok(physical)
}

fn hash_command_configs(
    hash: &mut aterm_digest::Sha256,
    command: &Cmd,
    allowed: &BTreeSet<PathBuf>,
    clippy_dirs: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let mut arguments = command.args.iter();
    while let Some(argument) = arguments.next() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        let config = if argument == "--config" {
            arguments
                .next()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "lint --config has no UTF-8 value".to_owned())?
        } else if let Some(value) = argument.strip_prefix("--config=") {
            value
        } else {
            continue;
        };
        if config.contains('=') {
            let document: aterm_toml::Value = aterm_toml::from_str(config)
                .map_err(|error| format!("lint --config {config:?}: {error}"))?;
            inspect_cargo_config(hash, &document, "lint --config", allowed, clippy_dirs)?;
        } else {
            let path = PathBuf::from(config);
            let path = if path.is_absolute() {
                path
            } else {
                command
                    .cwd
                    .as_deref()
                    .ok_or_else(|| "lint --config has no working directory".to_owned())?
                    .join(path)
            };
            hash_cargo_config(hash, &path, allowed, clippy_dirs)?;
        }
    }
    Ok(())
}

fn hash_cargo_config(
    hash: &mut aterm_digest::Sha256,
    path: &Path,
    allowed: &BTreeSet<PathBuf>,
    clippy_dirs: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    hash_optional_file(hash, b"cargo-config", path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cargo config {}: {error}", path.display())),
    };
    let document: aterm_toml::Value = aterm_toml::from_str(&text)
        .map_err(|error| format!("Cargo config {}: {error}", path.display()))?;
    inspect_cargo_config(
        hash,
        &document,
        &format!("Cargo config {}", path.display()),
        allowed,
        clippy_dirs,
    )
}

fn inspect_cargo_config(
    hash: &mut aterm_digest::Sha256,
    document: &aterm_toml::Value,
    origin: &str,
    allowed: &BTreeSet<PathBuf>,
    clippy_dirs: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    bounded_config_readers(document)?;
    if let Some(build) = document.get("build").and_then(aterm_toml::Value::as_table) {
        for (key, selector) in [
            ("rustc", "CARGO_BUILD_RUSTC"),
            ("rustc-wrapper", "CARGO_BUILD_RUSTC_WRAPPER"),
            (
                "rustc-workspace-wrapper",
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            ),
        ] {
            if let Some(value) = build.get(key) {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("{origin} build.{key} is not a compiler-tool string"))?;
                hash_selected_tool(hash, selector, value.as_bytes(), allowed)?;
            }
        }
    }
    if let Some(environment) = document.get("env").and_then(aterm_toml::Value::as_table) {
        for selector in TOOL_ENV_SELECTORS {
            if let Some(value) = environment.get(selector) {
                let value = cargo_config_env_string(value)
                    .ok_or_else(|| format!("{origin} env.{selector} has no string value"))?;
                hash_selected_tool(hash, selector, value.as_bytes(), allowed)?;
            }
        }
        if let Some(value) = environment.get("CLIPPY_CONF_DIR") {
            let value = cargo_config_env_string(value)
                .ok_or_else(|| format!("{origin} env.CLIPPY_CONF_DIR has no string value"))?;
            clippy_dirs.insert(absolute_directory("CLIPPY_CONF_DIR", value.as_bytes())?);
        }
    }
    Ok(())
}

fn cargo_config_env_string(value: &aterm_toml::Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_table()
            .and_then(|table| table.get("value"))
            .and_then(aterm_toml::Value::as_str)
    })
}

/// File-opening compiler options need a read closure. Only modeled scalar options may carry.
/// Link arguments are inert here: Tippy checks metadata and executable readers are unbounded.
fn bounded_rustflags(flags: &str) -> Result<(), String> {
    for flag in flags
        .split([' ', '\t', '\n', '\x1f'])
        .filter(|s| !s.is_empty())
    {
        if ![
            "-Zthreads=",
            "-Ztrust-verify=",
            "-Clink-arg=",
            "-Cdebug-assertions=",
            "-Coverflow-checks=",
            "-Cdebuginfo=",
            "-Copt-level=",
            "-Ccodegen-units=",
            "-Cstrip=",
            "-Cpanic=",
            "-Ctarget-cpu=",
            "-Ctarget-feature=",
            "--cfg=",
            "--check-cfg=",
            "--cap-lints=",
        ]
        .iter()
        .any(|prefix| flag.starts_with(prefix))
        {
            return Err(format!("compiler flag {flag:?} has no bounded input model"));
        }
    }
    Ok(())
}

fn bounded_target(target: &str) -> Result<(), String> {
    if target.contains(['/', '\\']) || target.ends_with(".json") {
        Err("a custom target definition has unbounded file inputs".into())
    } else {
        Ok(())
    }
}

fn compiler_reader_environment(name: &str) -> bool {
    matches!(
        name,
        "SYSROOT" | "RUST_SYSROOT" | "RUSTC_SYSROOT" | "RUSTC_CODEGEN_BACKEND" | "CLIPPY_ARGS"
    ) || loader_environment(name)
}

pub(crate) fn loader_environment(name: &str) -> bool {
    matches!(name, "LD_PRELOAD" | "LD_LIBRARY_PATH") || name.starts_with("DYLD_")
}

fn bounded_config_readers(value: &aterm_toml::Value) -> Result<(), String> {
    match value {
        aterm_toml::Value::Table(table) => {
            for (name, value) in table {
                if matches!(name.as_str(), "include" | "build-std" | "codegen-backend") {
                    return Err(format!(
                        "Cargo configuration {name} has unbounded file inputs"
                    ));
                }
                let flags =
                    name == "rustflags" || name == "RUSTFLAGS" || name.ends_with("_RUSTFLAGS");
                let target = name == "target" || name == "CARGO_BUILD_TARGET";
                if flags || target {
                    let check = if flags {
                        bounded_rustflags
                    } else {
                        bounded_target
                    };
                    if let Some(value) = cargo_config_env_string(value) {
                        check(value)?;
                    } else if let Some(values) = value.as_array() {
                        for value in values {
                            check(value.as_str().ok_or("non-string compiler configuration")?)?;
                        }
                    } else if flags {
                        return Err("compiler flags have an unknown configuration shape".into());
                    }
                }
                if name == "RUST_TARGET_PATH" {
                    return Err("RUST_TARGET_PATH has unbounded file inputs".into());
                }
                if compiler_reader_environment(name)
                    && cargo_config_env_string(value).is_none_or(|value| !value.is_empty())
                {
                    return Err(format!("{name} can select unbounded compiler inputs"));
                }
                bounded_config_readers(value)?;
            }
        }
        aterm_toml::Value::Array(values) => {
            for value in values {
                bounded_config_readers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn hash_clippy_directory(hash: &mut aterm_digest::Sha256, directory: &Path) -> Result<(), String> {
    part(
        hash,
        b"clippy-config-directory",
        &os_bytes(directory.as_os_str()),
    );
    for name in ["clippy.toml", ".clippy.toml"] {
        hash_optional_file(hash, b"clippy-config", &directory.join(name))?;
    }
    Ok(())
}

fn hash_optional_file(
    hash: &mut aterm_digest::Sha256,
    label: &[u8],
    path: &Path,
) -> Result<(), String> {
    part(hash, label, &os_bytes(path.as_os_str()));
    match std::fs::symlink_metadata(path) {
        Ok(_) => hash_tree(hash, path, path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            part(hash, b"absent", b"");
            Ok(())
        }
        Err(error) => Err(format!("lint input {}: {error}", path.display())),
    }
}

fn compiler_tools_digest(ctx: &crate::Ctx, tippy: &Path) -> Result<String, String> {
    let mut hash = aterm_digest::Sha256::new();
    for tool in compiler_tool_paths(ctx, tippy) {
        hash_compiler_tool(&mut hash, &tool)?;
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

fn hash_compiler_tool(hash: &mut aterm_digest::Sha256, tool: &Path) -> Result<(), String> {
    part(hash, b"compiler-tool", &os_bytes(tool.as_os_str()));
    part(hash, b"compiler-tool-bytes", digest_file(tool)?.as_bytes());
    part(
        hash,
        b"compiler-tool-runtime-identity",
        &runtime_file_identity(tool)?,
    );
    Ok(())
}

fn command_environment(command: &Cmd) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut environment = BTreeMap::new();
    for (name, value) in &command.envs {
        let mut name = os_bytes(name);
        if cfg!(windows) {
            name.make_ascii_uppercase();
        }
        environment.insert(name, os_bytes(value));
    }
    environment
}

fn shared_command_program(commands: &[Cmd]) -> Result<&Path, String> {
    let program = commands
        .first()
        .map(|command| command.program.as_path())
        .ok_or_else(|| "the package has no ordinary lint command".to_owned())?;
    if commands
        .iter()
        .any(|command| command.program.as_path() != program)
    {
        return Err("a lint package's commands select different Tippy executables".into());
    }
    Ok(program)
}

fn package_root(workspace: &Path, dir: &str) -> Result<PathBuf, String> {
    let relative = Path::new(dir);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "workspace package directory {dir:?} escapes the checkout"
        ));
    }
    let root = workspace
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("package root {dir:?}: {error}"))?;
    if !root.starts_with(workspace) {
        return Err(format!(
            "package root {} escapes the checkout",
            root.display()
        ));
    }
    Ok(root)
}

/// Pure Rust source can still read outside its crate during expansion. Refuse the
/// lexical capabilities, including inside macro definitions and inactive cfg arms:
/// a future expansion cannot acquire a reader from a token this closure never had.
/// Literal/comment contents are inert. A `path` attribute may redirect module loading;
/// assembly can load files itself; identifier concatenation can manufacture readers.
fn source_readers_bounded(root: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!("source input {} is a symlink", root.display()));
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(format!(
            "source input {} is not a regular file or directory",
            root.display()
        ));
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            source_readers_bounded(&entry.map_err(|e| e.to_string())?.path())?;
        }
    } else if root.extension().is_some_and(|ext| ext == "rs") {
        let source = std::fs::read_to_string(root).map_err(|e| e.to_string())?;
        let (code, _) = aterm_census::rust_source_views(&source);
        for token in code.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if matches!(
                token,
                "include"
                    | "include_str"
                    | "include_bytes"
                    | "env"
                    | "option_env"
                    | "path"
                    | "asm"
                    | "global_asm"
                    | "concat_idents"
                    | "concat"
            ) {
                return Err(format!(
                    "source {} has an unbounded compile-time reader {token}",
                    root.display()
                ));
            }
        }
    }
    Ok(())
}

fn normalized_environment(
    command: &Cmd,
    scratch: &Path,
    verifier_jobserver: Option<(&OsStr, u32)>,
) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, String> {
    let mut exact = BTreeMap::new();
    for (name, value) in &command.envs {
        let mut name = os_bytes(name);
        if cfg!(windows) {
            name.make_ascii_uppercase();
        }
        exact.insert(name, os_bytes(value));
    }
    if let Some((flags, max_jobs)) = verifier_jobserver {
        let expected = os_bytes(flags);
        let make = exact.get(b"MAKEFLAGS".as_slice());
        let cargo = exact.get(b"CARGO_MAKEFLAGS".as_slice());
        if make != Some(&expected) || cargo != Some(&expected) {
            return Err(
                "the finalized lint command does not contain the active verifier jobserver flags"
                    .into(),
            );
        }
        let jobs = exact
            .get(b"CARGO_BUILD_JOBS".as_slice())
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|jobs| *jobs > 0 && *jobs <= max_jobs)
            .ok_or_else(|| {
                "the finalized lint command has no valid active verifier compiler width".to_owned()
            })?;
        if exact.get(b"CARGO_BUILD_JOBS".as_slice()).map(Vec::as_slice)
            != Some(jobs.to_string().as_bytes())
        {
            return Err(
                "the finalized lint command's active verifier compiler width is not canonical"
                    .into(),
            );
        }
        // The FIFO pathname is a per-invocation capability, not a compiler input. Normalize only
        // the two exact values injected by this verifier's active pool. CARGO_BUILD_JOBS remains
        // exact and therefore binds the width; any user or unknown flags remain exact when there
        // is no active pool, and fail closed instead of borrowing this normalization when there is.
        let stable = b"-j --jobserver-auth=fifo:$VERIFIER_COMPILE_POOL".to_vec();
        exact.insert(b"MAKEFLAGS".to_vec(), stable.clone());
        exact.insert(b"CARGO_MAKEFLAGS".to_vec(), stable);
    }
    Ok(exact
        .into_iter()
        .map(|(name, value)| {
            let value = if name == crate::runs::VERIFY_RUN_ENV.as_bytes() {
                b"$VERIFIER_RUN_RECORD".to_vec()
            } else if name == aterm_capacity::COVER_ENV.as_bytes() {
                b"$VERIFIER_CAPACITY_COVER".to_vec()
            } else if matches!(name.as_slice(), b"HOME" | b"USERPROFILE" | b"TMPDIR") {
                normalize_scratch(&value, scratch)
            } else {
                value
            };
            (name, value)
        })
        .collect())
}

/// Bind one finalized lint command as [`EvidenceBuilder::content_key`] sees it. The release
/// jobserver's FIFO is an invocation-local capability, so only the exact paired flags installed
/// by the currently active verifier pool are normalized; every other command input stays exact.
fn hash_command_identity(
    hash: &mut aterm_digest::Sha256,
    ctx: &crate::Ctx,
    command: &Cmd,
) -> Result<(), String> {
    let current_jobs = crate::compile_width::current();
    let verifier_jobserver_flags = match (current_jobs, ctx.pool.as_deref()) {
        (Some(jobs), Some(pool)) => Some((pool.flags(), jobs)),
        (Some(_), None) => {
            return Err("the active verifier compiler width has no adopted jobserver".into());
        }
        (None, _) => None,
    };
    let environment = normalized_environment(
        command,
        &ctx.scratch,
        verifier_jobserver_flags
            .as_ref()
            .map(|(flags, jobs)| (OsStr::new(flags), *jobs)),
    )?;
    command_identity(hash, command, &environment);
    Ok(())
}

fn normalize_scratch(value: &[u8], scratch: &Path) -> Vec<u8> {
    let value = os_from_bytes(value);
    let path = Path::new(&value);
    path.strip_prefix(scratch).map_or_else(
        |_| os_bytes(path.as_os_str()),
        |relative| {
            let mut normalized = b"$VERIFIER_SCRATCH".to_vec();
            let suffix = os_bytes(relative.as_os_str());
            if !suffix.is_empty() {
                normalized.push(std::path::MAIN_SEPARATOR as u8);
                normalized.extend_from_slice(&suffix);
            }
            normalized
        },
    )
}

fn command_identity(
    hash: &mut aterm_digest::Sha256,
    command: &Cmd,
    env: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    part(
        hash,
        b"cwd",
        &command
            .cwd
            .as_deref()
            .map_or_else(Vec::new, |path| os_bytes(path.as_os_str())),
    );
    part(hash, b"argv", &os_bytes(command.program.as_os_str()));
    for argument in &command.args {
        part(hash, b"argv", &os_bytes(argument));
    }
    for (name, value) in env {
        part(hash, b"env-name", name);
        part(hash, b"env-value", value);
    }
}

fn hash_tree(hash: &mut aterm_digest::Sha256, root: &Path, path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("lint input {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("lint input {} is a symlink", path.display()));
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    if metadata.is_file() {
        part(hash, b"file", &os_bytes(relative.as_os_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            part(hash, b"mode", &metadata.permissions().mode().to_le_bytes());
        }
        let mut file =
            File::open(path).map_err(|error| format!("lint input {}: {error}", path.display()))?;
        let mut buffer = vec![0_u8; 1 << 20];
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => part(hash, b"bytes", &buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(format!("lint input {}: {error}", path.display()));
                }
            }
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!(
            "lint input {} is not a file or directory",
            path.display()
        ));
    }
    part(hash, b"directory", &os_bytes(relative.as_os_str()));
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| format!("lint input {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("lint input {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| os_bytes(&entry.file_name()));
    for entry in entries {
        hash_tree(hash, root, &entry.path())?;
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hash.update(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

fn runtime_file_identity(path: &Path) -> Result<Vec<u8>, String> {
    crate::test_content::runtime_file_identity(path)
}

/// One package completion event. Release keeps the last event for a spec.
#[must_use]
pub(crate) fn event(
    spec: &str,
    outcome: &str,
    evidence: &Evidence,
    execution_s: f64,
    carried: bool,
) -> String {
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::LintPackageFinished,
            aterm_json::json!({
                "spec": spec,
                "outcome": outcome,
                "content_key": evidence.content_key,
                "inputs_bounded": evidence.bounded(),
                "input_reason": evidence.reason,
                "execution_s": execution_s,
                "carried": carried,
            })
        )
    )
}

fn part(hash: &mut aterm_digest::Sha256, name: &[u8], value: &[u8]) {
    hash.update((name.len() as u64).to_le_bytes());
    hash.update(name);
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn os_from_bytes(value: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    OsStr::from_bytes(value).to_os_string()
}

#[cfg(not(unix))]
fn os_from_bytes(value: &[u8]) -> OsString {
    String::from_utf8_lossy(value).into_owned().into()
}

#[cfg(test)]
#[path = "lint_content_tests.rs"]
mod tests;

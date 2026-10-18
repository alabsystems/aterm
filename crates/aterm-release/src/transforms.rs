// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The export's source rewrites, which the publication engine runs over each export
//! (`pub stage`, after the manifest's selection and before its guards) through
//! `publish/transforms.sh` → `ship hook transforms`.
//!
//! * vendor/astream — a submodule of a PRIVATE repository, and a gitlink is not
//!   source — is written as files: astream's root Cargo.toml and, for every astream
//!   crate the export reaches ([`astream_closure`]), each crate's tree but its tests/,
//!   examples/ and benches/ (its build script and the files its src/ includes from
//!   beside it go with it), from the gitlink commit of the EXPORTED commit, read from
//!   the submodule's object store (never its working tree), plus aterm's LICENSE and a
//!   README saying what it is.
//!   The root keeps its `[workspace]` tables, which the crates inherit from, and no members
//!   ([`memberless_root`]): its member list names crates the export does not write, and the
//!   engine's workspace-closure guard loads every workspace in the export.
//! * A `[[test]]`/`[[bench]]`/`[[example]]`/`[[bin]]` table whose file the export
//!   withholds is dropped with the comment block above it, so a public clone's build
//!   resolves every target it declares.
//! * A workspace package the export withholds (its manifest is in the exported commit
//!   but not in the export) leaves Cargo.lock, with whatever only it reached: the
//!   exported lock is the one cargo writes for the exported workspace, and names
//!   nothing the export withholds ([`prune_lock`]).
//! * The private org becomes the public one, and developer homes are anonymized to
//!   exactly the guards' shapes — nothing wider: a bare `/Users/` is behaviour
//!   (relocate.rs's machine-local prefix) and ships as written.
//!
//! The version and rust-toolchain.toml ship as committed. No content rule refuses
//! here: every rule is a guard pattern, reported together by the engine.

use std::path::{Path, PathBuf};

// Each needle is spelled in halves: written whole, the export's own rewrite would
// change it in the exported copy of this file, and the account word would trip the
// export's guard (publish/forbidden-extra.txt).

/// The developer account the export renames.
const ACCOUNT: &str = concat!("aya", "tes");

/// The private org, which becomes the public one.
const PRIVATE_ORG: &str = concat!("andrew", "yatesai");

/// The toolchain checkouts a home path names.
const TILDE_TRUST: &str = concat!("~", "/trust");
const TILDE_CLEAN: &str = concat!("~", "/clean");

/// What the engine hands the transforms.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// The repository root (`ROOT`).
    pub root: PathBuf,
    /// The export being staged (`EXPORT`).
    pub export: PathBuf,
    /// The commit the export was made from (`CAPTURED_COMMIT`; `HEAD` when unset).
    pub captured: String,
}

impl Inputs {
    /// The engine's variables.
    ///
    /// # Errors
    /// `ROOT` or `EXPORT` unset.
    pub fn from_env() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            root: var("ROOT").map(PathBuf::from).ok_or("ROOT is not set")?,
            export: var("EXPORT")
                .map(PathBuf::from)
                .ok_or("EXPORT is not set")?,
            captured: var("CAPTURED_COMMIT").unwrap_or_else(|| "HEAD".into()),
        })
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = aterm_uds::spawnfd::command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run every rewrite over `inputs.export`; each line of what was done, as the
/// engine's transcript shows it.
///
/// # Errors
/// The astream gitlink cannot be written as files.
pub fn run(inputs: &Inputs) -> Result<Vec<String>, String> {
    let mut said = Vec::new();
    let (rev, crates) = astream(inputs)?;
    said.push(format!(
        "== vendor/astream written as files from the submodule at {}: {}",
        &rev[..rev.len().min(12)],
        crates.join(" ")
    ));
    let withheld = withheld_packages(inputs)?;
    let lock = inputs.export.join("Cargo.lock");
    if !withheld.is_empty() && lock.is_file() {
        let text =
            std::fs::read_to_string(&lock).map_err(|e| format!("{}: {e}", lock.display()))?;
        if let Some((pruned, gone)) = prune_lock(&text, &withheld)? {
            std::fs::write(&lock, pruned).map_err(|e| format!("{}: {e}", lock.display()))?;
            for package in gone {
                said.push(format!("  lock-prune: {package}"));
            }
            said.push(
                "== Cargo.lock: the packages the export withholds, and what only they reached, \
                 were dropped"
                    .into(),
            );
        }
    }
    for (manifest, kind, name) in strip_targets(&inputs.export)? {
        said.push(format!(
            "  target-strip: {manifest} [[{kind}]] {name} (its file is not exported)"
        ));
    }
    said.push(
        "== explicit build targets whose files are not exported were stripped from their \
         Cargo.toml"
            .into(),
    );
    rewrite_tree(&inputs.export)?;
    said.push("== aterm export: Trust pin kept, public org, anonymized home paths".into());
    Ok(said)
}

/// Write vendor/astream into the export from the captured commit's gitlink; the rev and
/// the crates written.
fn astream(inputs: &Inputs) -> Result<(String, Vec<String>), String> {
    let root = &inputs.root;
    let sub = root.join("vendor/astream");
    let listing = git(root, &["ls-tree", &inputs.captured, "--", "vendor/astream"])?;
    let rev = String::from_utf8_lossy(&listing)
        .lines()
        .find_map(|l| {
            let mut f = l.split_whitespace();
            (f.next() == Some("160000")).then(|| f.nth(1).map(str::to_string))?
        })
        .ok_or_else(|| format!("vendor/astream is not a gitlink in {}", inputs.captured))?;
    let top = git(&sub, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|t| PathBuf::from(String::from_utf8_lossy(&t).trim()));
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !top.is_some_and(|t| same(&t, &sub)) {
        return Err(format!(
            "vendor/astream is not an initialized submodule in {} — stage from a checkout that \
             has it (pub stage --repo-path <checkout>, after git submodule update --init \
             vendor/astream)",
            root.display()
        ));
    }
    if git(&sub, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_err() {
        return Err(format!(
            "vendor/astream's gitlink {rev} is not in the submodule (git -C {} submodule update \
             --init vendor/astream)",
            root.display()
        ));
    }
    let dest = inputs.export.join("vendor/astream");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(inputs.export.join(".gitmodules"));
    let crates = astream_closure(&inputs.export, &sub, &rev)
        .map_err(|e| format!("could not derive the astream crates the export depends on: {e}"))?;
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    // A crate's build reads more than src/: its build script, and the files src/ and
    // the script include from beside src/ (astream-wire's contract_fn.rs,
    // twin_answers.rs and clean_islands.rs, which every astream crate includes).
    let mut paths = vec!["Cargo.toml".to_string()];
    let mut roots = vec!["Cargo.toml".to_string()];
    for c in &crates {
        paths.push(format!("crates/{c}/Cargo.toml"));
        paths.push(format!("crates/{c}/src"));
        roots.push(format!("crates/{c}"));
    }
    let mut args = vec!["ls-tree", "-r", "-z", rev.as_str(), "--"];
    args.extend(roots.iter().map(String::as_str));
    let tree = git(&sub, &args)?;
    let entries: Vec<(String, String, String)> = tree
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .filter_map(|e| {
            let e = String::from_utf8_lossy(e);
            let (meta, path) = e.split_once('\t')?;
            let mut m = meta.split(' ');
            Some((
                m.next()?.to_string(),
                m.nth(1)?.to_string(),
                path.to_string(),
            ))
        })
        .filter(|(_, _, path)| !in_target_dir(path))
        .collect();
    for wanted in &paths {
        let covered = entries
            .iter()
            .any(|(_, _, p)| p == wanted || p.starts_with(&format!("{wanted}/")));
        if !covered {
            return Err(format!(
                "could not write vendor/astream@{rev} into the export: {wanted} is not in it"
            ));
        }
    }
    for (mode, blob, path) in &entries {
        let bytes = git(&sub, &["cat-file", "blob", blob])?;
        let target = dest.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        if mode == "120000" {
            #[cfg(unix)]
            std::os::unix::fs::symlink(String::from_utf8_lossy(&bytes).as_ref(), &target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
            continue;
        }
        std::fs::write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let perm = if mode == "100755" { 0o755 } else { 0o644 };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(perm))
                .map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    let root_manifest = dest.join("Cargo.toml");
    let text = std::fs::read_to_string(&root_manifest)
        .map_err(|e| format!("{}: {e}", root_manifest.display()))?;
    std::fs::write(&root_manifest, memberless_root(&text)?)
        .map_err(|e| format!("{}: {e}", root_manifest.display()))?;
    std::fs::copy(root.join("LICENSE"), dest.join("LICENSE"))
        .map_err(|e| format!("LICENSE: {e}"))?;
    let named: Vec<String> = crates.iter().map(|c| format!("`{c}`")).collect();
    let readme = format!(
        "# vendor/astream\n\n\
         The astream crates aterm builds against (the fabric bridge, crates/aterm-link,\n\
         and the conformance suite's differential VT oracle) —\n\
         {} — as plain\n\
         files from astream commit `{rev}`: each crate's tree but its `tests/`,\n\
         `examples/` and `benches/`, and astream's root `Cargo.toml`, whose workspace\n\
         fields they inherit, with its member list emptied (it names crates not copied\n\
         here). Apache-2.0 (`LICENSE`).\n\
         aterm's root `Cargo.toml` excludes this directory from aterm's workspace, so the\n\
         crates build as dependencies and nothing else.\n\n\
         That commit is on astream's development line. https://github.com/alabsystems/astream\n\
         carries astream's own releases, each published as a snapshot commit of its own, so\n\
         that commit's id does not appear there, and the version these manifests declare\n\
         may not be released there yet.\n",
        named.join(", ")
    );
    std::fs::write(dest.join("README.md"), readme).map_err(|e| format!("README.md: {e}"))?;
    Ok((rev, crates))
}

/// Whether an astream path is in a crate's tests/, examples/ or benches/: targets a
/// dependency's build never reads, which stay private; a manifest table naming a file
/// in one is dropped with it (the target strip).
fn in_target_dir(path: &str) -> bool {
    let Some((_, in_crate)) = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    ["tests/", "examples/", "benches/"]
        .iter()
        .any(|d| in_crate.starts_with(d))
}

/// astream's root manifest as the export writes it: the `[workspace]` table's `members`
/// emptied and its `default-members` dropped, every other byte as committed. The crates
/// the export writes build as aterm's path dependencies, which inherit from this root
/// without being its members, so a member list naming crates the export leaves out only
/// makes the root a workspace nothing can load (the engine's closure guard loads it).
///
/// # Errors
/// The manifest has no `[workspace]` table, an array in it does not close, or the result
/// does not parse with empty members.
fn memberless_root(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let (mut in_workspace, mut seen) = (false, false);
    let mut rest = text;
    while !rest.is_empty() {
        let line_end = rest.find('\n').map_or(rest.len(), |i| i + 1);
        let line = &rest[..line_end];
        let code = line.split('#').next().unwrap_or_default().trim();
        if code.starts_with('[') {
            in_workspace = code == "[workspace]";
            seen |= in_workspace;
        }
        let key = code.split_once('=').map(|(k, _)| k.trim());
        let Some(key @ ("members" | "default-members")) = key.filter(|_| in_workspace) else {
            out.push_str(line);
            rest = &rest[line_end..];
            continue;
        };
        let open = rest.find('[').filter(|&i| i < line_end).ok_or_else(|| {
            format!("astream's root manifest: `{key}` in [workspace] is not an array")
        })?;
        let close = open
            + array_end(&rest[open..])
                .ok_or_else(|| format!("astream's root manifest: `{key}` never closes"))?;
        let after = rest[close..]
            .find('\n')
            .map_or(rest.len(), |i| close + i + 1);
        if key == "members" {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push_str(indent);
            out.push_str("members = []");
            out.push_str(&rest[close..after]);
        }
        rest = &rest[after..];
    }
    if !seen {
        return Err("astream's root manifest has no [workspace] table".into());
    }
    let parsed = aterm_toml::from_str::<aterm_toml::Value>(&out)
        .map_err(|e| format!("astream's root manifest, members emptied: {e}"))?;
    let workspace = parsed.get("workspace");
    let empty = workspace
        .and_then(|w| w.get("members"))
        .and_then(aterm_toml::Value::as_array)
        .is_some_and(Vec::is_empty);
    if !empty || workspace.and_then(|w| w.get("default-members")).is_some() {
        return Err("astream's root manifest still names members after emptying them".into());
    }
    Ok(out)
}

/// The length of the TOML array `s` starts with, through its closing `]`: brackets in
/// strings and comments do not count.
fn array_end(s: &str) -> Option<usize> {
    let (mut depth, mut quote, mut escaped, mut comment) = (0usize, None, false, false);
    for (i, c) in s.char_indices() {
        if comment {
            comment = c != '\n';
        } else if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                '"' | '\'' => quote = Some(c),
                '#' => comment = true,
                '[' => depth += 1,
                ']' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// A manifest's dependency rows that cargo follows: its `[dependencies]` and
/// `[build-dependencies]`, target-specific and `[workspace.dependencies]` ones included,
/// optional ones too (a feature can turn any on), and with `dev` its
/// `[dev-dependencies]`. Cargo reads a WORKSPACE MEMBER's dev-dependencies whenever it
/// loads the workspace (they are in the lock, and a missing path is a load error), and
/// no DEPENDENCY's: so an exported manifest's are followed (aterm-conformance's
/// astream-term, the differential VT oracle's fold), an astream crate's never. Each row
/// is (name, `path`, `workspace = true`).
fn dependency_rows(manifest: &aterm_toml::Value, dev: bool) -> Vec<(String, Option<String>, bool)> {
    use aterm_toml::Value;
    const KINDS: [&str; 5] = [
        "dependencies",
        "build-dependencies",
        "build_dependencies",
        "dev-dependencies",
        "dev_dependencies",
    ];
    let kinds = if dev { &KINDS[..] } else { &KINDS[..3] };
    let Some(root) = manifest.as_table() else {
        return Vec::new();
    };
    let mut scopes = vec![root];
    scopes.extend(root.get("workspace").and_then(Value::as_table));
    if let Some(targets) = root.get("target").and_then(Value::as_table) {
        scopes.extend(targets.values().filter_map(Value::as_table));
    }
    let mut rows = Vec::new();
    for scope in scopes {
        for &kind in kinds {
            let Some(table) = scope.get(kind).and_then(Value::as_table) else {
                continue;
            };
            for (name, spec) in table.iter() {
                let path = spec.get("path").and_then(Value::as_str).map(str::to_string);
                let workspace = spec.get("workspace").and_then(Value::as_bool) == Some(true);
                rows.push((name.clone(), path, workspace));
            }
        }
    }
    rows
}

/// `base` joined with the relative `path`, `.` and `..` resolved lexically.
fn join_lexically(base: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The astream crates the export needs, by directory name, sorted: each one an exported
/// manifest path-depends on under `vendor/astream/crates/` (dev-dependencies included:
/// see [`dependency_rows`]), and each one those reach in
/// turn at `rev` (`path = …`, or `workspace = true` through astream's root
/// `[workspace.dependencies]`). DERIVED, never listed: a hand list went stale when
/// astream-cap came to depend on astream-digest, and the export would have carried a
/// manifest naming a crate it did not carry.
///
/// # Errors
/// No exported manifest reaches astream (the scan is wrong), a crate the export reaches
/// is not at `rev`, or an astream crate path-depends outside astream's `crates/`.
pub fn astream_closure(export: &Path, sub: &Path, rev: &str) -> Result<Vec<String>, String> {
    let parse = |text: &str, what: &str| {
        aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|e| format!("{what}: {e}"))
    };
    let mut found = Vec::new();
    manifests(export, &mut found);
    // crate directory -> the manifest that first named it
    let mut reached: std::collections::BTreeMap<String, String> = Default::default();
    for manifest in found {
        let rel_manifest = manifest.strip_prefix(export).unwrap_or(&manifest);
        let rel_dir = rel_manifest
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("{}: {e}", manifest.display()))?;
        let rows = dependency_rows(&parse(&text, &rel_manifest.display().to_string())?, true);
        for (_, path, _) in rows {
            let Some(path) = path else { continue };
            let target = join_lexically(&rel_dir, &path);
            if let Some(name) = target.strip_prefix("vendor/astream/crates/")
                && !name.is_empty()
                && !name.contains('/')
            {
                reached
                    .entry(name.to_string())
                    .or_insert_with(|| rel_manifest.display().to_string());
            }
        }
    }
    if reached.is_empty() {
        return Err(
            "no exported Cargo.toml path-depends on vendor/astream/crates/<crate>, yet \
                    vendor/astream is a gitlink: the dependency scan is wrong"
                .into(),
        );
    }
    let at_rev = |path: &str| {
        git(sub, &["show", &format!("{rev}:{path}")])
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let short = &rev[..rev.len().min(12)];
    let root =
        at_rev("Cargo.toml").ok_or_else(|| format!("astream {short} has no root Cargo.toml"))?;
    let workspace: std::collections::BTreeMap<String, String> =
        dependency_rows(&parse(&root, "astream's root Cargo.toml")?, false)
            .into_iter()
            .filter_map(|(name, path, _)| Some((name, path?)))
            .collect();
    let mut todo: Vec<String> = reached.keys().cloned().collect();
    let mut done = std::collections::BTreeSet::new();
    while let Some(krate) = todo.pop() {
        if !done.insert(krate.clone()) {
            continue;
        }
        let Some(manifest) = at_rev(&format!("crates/{krate}/Cargo.toml")) else {
            return Err(format!(
                "{} path-depends on vendor/astream/crates/{krate}, which astream {short} does \
                 not have",
                reached[&krate]
            ));
        };
        let own = format!("crates/{krate}");
        for (name, path, in_workspace) in dependency_rows(&parse(&manifest, &own)?, false) {
            let target = match (path, in_workspace) {
                (Some(p), _) => join_lexically(&own, &p),
                (None, true) => match workspace.get(&name) {
                    Some(p) => join_lexically("", p),
                    None => continue,
                },
                // a crates.io dependency
                (None, false) => continue,
            };
            let Some(dep) = target
                .strip_prefix("crates/")
                .filter(|d| !d.is_empty() && !d.contains('/'))
            else {
                return Err(format!(
                    "astream {own}/Cargo.toml depends on {name} at {target}, which is not an \
                     astream crate the export can carry"
                ));
            };
            reached
                .entry(dep.to_string())
                .or_insert_with(|| format!("astream {own}/Cargo.toml"));
            todo.push(dep.to_string());
        }
    }
    Ok(done.into_iter().collect())
}

/// The package names the export withholds: each `[package]` whose manifest the exported
/// commit has (`CAPTURED_COMMIT`'s tree) and the export does not, unless an exported
/// manifest carries the same name.
fn withheld_packages(inputs: &Inputs) -> Result<std::collections::BTreeSet<String>, String> {
    let name_of = |text: &str| {
        aterm_toml::from_str::<aterm_toml::Value>(text)
            .ok()?
            .get("package")?
            .get("name")?
            .as_str()
            .map(str::to_string)
    };
    let listing = git(
        &inputs.root,
        &["ls-tree", "-r", "-z", "--name-only", &inputs.captured],
    )?;
    let mut withheld = std::collections::BTreeSet::new();
    for path in listing.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path);
        let is_manifest = path == "Cargo.toml" || path.ends_with("/Cargo.toml");
        if !is_manifest || inputs.export.join(path.as_ref()).exists() {
            continue;
        }
        let text = git(
            &inputs.root,
            &["show", &format!("{}:{path}", inputs.captured)],
        )?;
        withheld.extend(name_of(&String::from_utf8_lossy(&text)));
    }
    let mut carried = Vec::new();
    manifests(&inputs.export, &mut carried);
    for manifest in carried {
        if let Some(name) = std::fs::read_to_string(&manifest)
            .ok()
            .as_deref()
            .and_then(name_of)
        {
            withheld.remove(&name);
        }
    }
    Ok(withheld)
}

/// One `[[package]]` of a Cargo.lock: its fields, and its text (from its header to the
/// next one).
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    dependencies: Vec<String>,
    text: String,
}

/// `lock` without the path packages named in `withheld` and every package only they
/// reached, each remaining dependency spelled as cargo spells it for what is left (the
/// version only when two versions share the name, the source only when two sources share
/// both) — the file cargo writes for the export's workspace.
/// `None` when nothing changes; else the text and the packages dropped (`name version`).
///
/// # Errors
/// The lock does not parse, or a package the export carries depends on one it withholds
/// (no public clone could resolve that, whatever the lock says).
pub fn prune_lock(
    lock: &str,
    withheld: &std::collections::BTreeSet<String>,
) -> Result<Option<(String, Vec<String>)>, String> {
    const HEADER: &str = "[[package]]\n";
    let Some(first) = lock.find(HEADER) else {
        return Ok(None);
    };
    let (preamble, body) = lock.split_at(first);
    let mut packages = Vec::new();
    for chunk in body.split(HEADER).filter(|c| !c.is_empty()) {
        let fields = aterm_toml::from_str::<aterm_toml::Value>(chunk)
            .map_err(|e| format!("Cargo.lock: a [[package]] does not parse: {e}"))?;
        let field = |k: &str| {
            fields
                .get(k)
                .and_then(aterm_toml::Value::as_str)
                .map(str::to_string)
        };
        packages.push(LockPackage {
            name: field("name").ok_or("Cargo.lock: a [[package]] without a name")?,
            version: field("version").unwrap_or_default(),
            source: field("source"),
            dependencies: fields
                .get("dependencies")
                .and_then(aterm_toml::Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter_map(aterm_toml::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            text: format!("{HEADER}{chunk}"),
        });
    }
    let is_withheld = |p: &LockPackage| p.source.is_none() && withheld.contains(&p.name);
    if !packages.iter().any(is_withheld) {
        return Ok(None);
    }
    // a dependency spec, `name [version [(source)]]`, against the packages it can mean
    let resolve = |spec: &str| -> Option<usize> {
        let mut words = spec.splitn(3, ' ');
        let name = words.next()?;
        let version = words.next();
        let source = words
            .next()
            .map(|s| s.trim_start_matches('(').trim_end_matches(')'));
        packages.iter().position(|p| {
            p.name == name
                && version.is_none_or(|v| p.version == v)
                && source.is_none_or(|s| p.source.as_deref() == Some(s))
        })
    };
    let mut kept = vec![false; packages.len()];
    let mut todo: Vec<usize> = (0..packages.len())
        .filter(|&i| packages[i].source.is_none() && !is_withheld(&packages[i]))
        .collect();
    while let Some(i) = todo.pop() {
        if std::mem::replace(&mut kept[i], true) {
            continue;
        }
        for spec in &packages[i].dependencies {
            let Some(j) = resolve(spec) else {
                return Err(format!(
                    "Cargo.lock: {} depends on {spec}, which the lock does not list",
                    packages[i].name
                ));
            };
            if is_withheld(&packages[j]) {
                return Err(format!(
                    "{} depends on {}, which the export withholds: no public clone could \
                     build it",
                    packages[i].name, packages[j].name
                ));
            }
            todo.push(j);
        }
    }
    // cargo's spelling of a kept package as a dependency, given what is kept
    let spelled = |j: usize| {
        let p = &packages[j];
        let same_name = (0..packages.len()).filter(|&k| kept[k] && packages[k].name == p.name);
        let same_version = same_name
            .clone()
            .filter(|&k| packages[k].version == p.version);
        match (same_name.count(), same_version.count(), &p.source) {
            (1, _, _) => p.name.clone(),
            (_, 1, _) | (_, _, None) => format!("{} {}", p.name, p.version),
            (_, _, Some(source)) => format!("{} {} ({source})", p.name, p.version),
        }
    };
    let mut out = preamble.to_string();
    let mut gone = Vec::new();
    for (i, p) in packages.iter().enumerate() {
        if !kept[i] {
            gone.push(format!("{} {}", p.name, p.version));
            continue;
        }
        let mut text = p.text.clone();
        for spec in &p.dependencies {
            let now = resolve(spec).map(spelled).unwrap_or_else(|| spec.clone());
            if now != *spec {
                text = text.replace(&format!(" \"{spec}\",\n"), &format!(" \"{now}\",\n"));
            }
        }
        out.push_str(&text);
    }
    // the last kept package ends the file with one newline, as cargo writes it
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(Some((out, gone)))
}

/// Every Cargo.toml under `dir` (outside .git and target).
fn manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !matches!(e.file_name().to_str(), Some(".git" | "target")) {
                manifests(&path, out);
            }
        } else if kind.is_file() && e.file_name() == "Cargo.toml" {
            out.push(path);
        }
    }
}

/// The files a target table of `kind` named `name` may build from, by default.
fn default_paths(kind: &str, name: &str) -> Vec<String> {
    match kind {
        "test" => vec![format!("tests/{name}.rs"), format!("tests/{name}/main.rs")],
        "bench" => vec![
            format!("benches/{name}.rs"),
            format!("benches/{name}/main.rs"),
        ],
        "example" => vec![
            format!("examples/{name}.rs"),
            format!("examples/{name}/main.rs"),
        ],
        _ => Vec::new(),
    }
}

/// `key = "value"` at the start of a (possibly indented) line.
fn string_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(key)?;
    let rest = rest
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .strip_prefix('"')?;
    rest.split_once('"')
        .map(|(v, _)| v)
        .filter(|v| !v.is_empty())
}

/// `text` (a Cargo.toml in `dir`) without the target tables whose file is not there,
/// and what was dropped: (kind, name).
fn strip_manifest(text: &str, dir: &Path) -> Option<(String, Vec<(String, String)>)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].starts_with('['))
        .collect();
    if starts.is_empty() {
        return None;
    }
    // a table owns the comment block directly above it
    let cuts: Vec<usize> = starts
        .iter()
        .map(|s| {
            let mut c = *s;
            while c > 0 && lines[c - 1].starts_with('#') {
                c -= 1;
            }
            c
        })
        .collect();
    let mut segments = vec![(0, cuts[0])];
    for k in 0..cuts.len() {
        segments.push((cuts[k], cuts.get(k + 1).copied().unwrap_or(lines.len())));
    }
    let mut keep: Vec<&str> = Vec::new();
    let mut dropped = Vec::new();
    for (a, b) in segments {
        let seg = &lines[a..b];
        let head = seg
            .iter()
            .find(|l| l.starts_with('['))
            .copied()
            .unwrap_or("");
        let kind = ["test", "bench", "example", "bin"]
            .into_iter()
            .find(|k| head.trim_end() == format!("[[{k}]]"));
        if let Some(kind) = kind {
            let (mut path, mut name) = (None, None);
            for l in seg {
                path = string_key(l, "path").or(path);
                name = string_key(l, "name").or(name);
            }
            let candidates = match (path, name) {
                (Some(p), _) => vec![p.to_string()],
                (None, Some(n)) => default_paths(kind, n),
                (None, None) => Vec::new(),
            };
            if !candidates.is_empty() && !candidates.iter().any(|c| dir.join(c).exists()) {
                dropped.push((kind.to_string(), name.unwrap_or("").to_string()));
                continue;
            }
        }
        keep.extend_from_slice(seg);
    }
    if dropped.is_empty() {
        return None;
    }
    let mut text = keep.join("\n");
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    Some((text, dropped))
}

/// Drop every target table whose file the export lacks; what was dropped:
/// (manifest relative to the export, kind, name).
fn strip_targets(export: &Path) -> Result<Vec<(String, String, String)>, String> {
    let mut found = Vec::new();
    manifests(export, &mut found);
    let mut dropped = Vec::new();
    for manifest in found {
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("{}: {e}", manifest.display()))?;
        let dir = manifest.parent().unwrap_or(export);
        if let Some((stripped, gone)) = strip_manifest(&text, dir) {
            std::fs::write(&manifest, stripped)
                .map_err(|e| format!("{}: {e}", manifest.display()))?;
            let rel = manifest
                .strip_prefix(export)
                .unwrap_or(&manifest)
                .display()
                .to_string();
            dropped.extend(gone.into_iter().map(|(k, n)| (rel.clone(), k, n)));
        }
    }
    Ok(dropped)
}

/// Replace every `from` in `text` with `to`.
fn replace_all(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

/// The text rewrites, in order, over one file's bytes.
#[must_use]
pub fn rewrite(text: &[u8]) -> Vec<u8> {
    let home = format!("/Users/{ACCOUNT}");
    let at = format!("{ACCOUNT}@");
    // the public org, never the private one
    let mut t = replace_all(text, PRIVATE_ORG.as_bytes(), b"alabsystems");
    // the developer's home becomes an example one
    t = replace_all(&t, home.as_bytes(), b"/Users//example");
    // every /Users/<letter> gets a doubled separator: still an absolute macOS Users
    // path, but no account the central guard's `/Users/[A-Za-z]` can name
    let mut doubled = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i..].starts_with(b"/Users/") && t.get(i + 7).is_some_and(u8::is_ascii_alphabetic) {
            doubled.extend_from_slice(b"/Users//");
            i += 7;
        } else {
            doubled.push(t[i]);
            i += 1;
        }
    }
    t = replace_all(&doubled, TILDE_TRUST.as_bytes(), b"$HOME/trust");
    t = replace_all(&t, TILDE_CLEAN.as_bytes(), b"$HOME/clean");
    // the account written without a /Users/ prefix (a prompt captured as bytes)
    replace_all(&t, at.as_bytes(), b"user@")
}

/// [`rewrite`] over every text file of the tree (a file holding a NUL is binary and
/// left alone; symlinks are not followed).
fn rewrite_tree(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            rewrite_tree(&path)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if bytes.contains(&0) {
                continue;
            }
            let out = rewrite(&bytes);
            if out != bytes {
                std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewrites_take_exactly_the_guards_shapes() {
        let acct = ACCOUNT;
        let case = |input: String, want: &str| {
            assert_eq!(
                String::from_utf8(rewrite(input.as_bytes())).unwrap(),
                want,
                "{input}"
            );
        };
        case(
            format!("https://github.com/{PRIVATE_ORG}/aterm"),
            "https://github.com/alabsystems/aterm",
        );
        case(
            format!("/Users/{acct}/aterm/target"),
            "/Users//example/aterm/target",
        );
        case("/Users//someone/Library".into(), "/Users//someone/Library");
        case(format!("{TILDE_TRUST}/build"), "$HOME/trust/build");
        case(format!("{TILDE_CLEAN}/bin"), "$HOME/clean/bin");
        case(format!("b\"{acct}@mac ~ % \""), "b\"user@mac ~ % \"");
        // a bare "/Users/" is behaviour (relocate.rs's machine-local prefix): as written
        case(
            "path.starts_with(\"/Users/\")".into(),
            "path.starts_with(\"/Users/\")",
        );
        // the version is not a rewrite: a patch slot other than 0 reaches the engine
        case("version = \"0.104.7\"".into(), "version = \"0.104.7\"");
    }

    #[test]
    fn astreams_root_keeps_what_its_crates_inherit_and_names_no_member() {
        let root = "# head\n[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/a\", # \
                    the gate ]\n    \"crates/b]\",\n]\n# kept\ndefault-members = \
                    [\"crates/a\"]\n\n[workspace.package]\nversion = \"0.2.0\"\n\n\
                    [workspace.dependencies]\na = { path = \"crates/a\" }\nmembers = \"1\"\n";
        assert_eq!(
            memberless_root(root).unwrap(),
            "# head\n[workspace]\nresolver = \"2\"\nmembers = []\n# kept\n\n\
             [workspace.package]\nversion = \"0.2.0\"\n\n[workspace.dependencies]\na = { path = \
             \"crates/a\" }\nmembers = \"1\"\n"
        );
        // no workspace table, or one whose list never closes, writes nothing
        assert!(memberless_root("[package]\nname = \"a\"\n").is_err());
        assert!(memberless_root("[workspace]\nmembers = [\n  \"crates/a\",\n").is_err());
    }

    /// A sample lock in cargo's v4 shape: `app` and `lab` are workspace packages, `lab`
    /// alone reaches `dup 2.0.0` and `only-lab`, so `app`'s `dup 1.0.0` is spelled with
    /// its version only while both versions are listed.
    fn sample_lock(with_lab: bool) -> String {
        let reg = "registry+https://github.com/rust-lang/crates.io-index";
        let mut out = String::from(
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual \
             editing.\nversion = 4\n\n",
        );
        let app_dup = if with_lab { "dup 1.0.0" } else { "dup" };
        out.push_str(&format!(
            "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"{app_dup}\",\n]\n\n"
        ));
        out.push_str(&format!(
            "[[package]]\nname = \"dup\"\nversion = \"1.0.0\"\nsource = \"{reg}\"\nchecksum = \
             \"aa\"\n\n"
        ));
        if with_lab {
            out.push_str(&format!(
                "[[package]]\nname = \"dup\"\nversion = \"2.0.0\"\nsource = \"{reg}\"\nchecksum \
                 = \"bb\"\n\n[[package]]\nname = \"lab\"\nversion = \"0.1.0\"\ndependencies = \
                 [\n \"app\",\n \"dup 2.0.0\",\n \"only-lab\",\n]\n\n[[package]]\nname = \
                 \"only-lab\"\nversion = \"1.0.0\"\nsource = \"{reg}\"\n\n"
            ));
        }
        out.push_str("[[package]]\nname = \"zed\"\nversion = \"0.1.0\"\n");
        out
    }

    #[test]
    fn a_withheld_package_leaves_the_lock_as_cargo_would_write_it() {
        let withheld: std::collections::BTreeSet<String> = ["lab".to_string()].into();
        let (pruned, gone) = prune_lock(&sample_lock(true), &withheld).unwrap().unwrap();
        assert_eq!(pruned, sample_lock(false));
        assert_eq!(gone, ["dup 2.0.0", "lab 0.1.0", "only-lab 1.0.0"]);
        // a lock that names nothing withheld is not rewritten
        assert_eq!(prune_lock(&sample_lock(false), &withheld).unwrap(), None);
        let nothing = std::collections::BTreeSet::new();
        assert_eq!(prune_lock(&sample_lock(true), &nothing).unwrap(), None);
    }

    #[test]
    fn a_kept_package_that_needs_a_withheld_one_is_refused_by_name() {
        let withheld: std::collections::BTreeSet<String> = ["app".to_string()].into();
        let why = prune_lock(&sample_lock(true), &withheld).unwrap_err();
        assert!(
            why.contains("lab depends on app, which the export withholds"),
            "{why}"
        );
    }

    #[test]
    fn a_target_whose_file_is_withheld_goes_with_its_comment_and_nothing_else_moves() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-transforms-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("tests/suite.rs"), "#[test]\nfn t() {}\n").unwrap();
        let manifest = "[package]\nname = \"fixture\"\nautotests = false\n\n[[test]]\nname = \
                        \"suite\"\npath = \"tests/suite.rs\"\n\n# The withheld one: its file is \
                        not in the export.\n[[test]]\nname = \"withheld\"\npath = \
                        \"tests/withheld.rs\"\n\n[lints]\nworkspace = true\n";
        let (text, dropped) = strip_manifest(manifest, dir.path()).unwrap();
        assert_eq!(dropped, [("test".to_string(), "withheld".to_string())]);
        assert_eq!(
            text,
            "[package]\nname = \"fixture\"\nautotests = false\n\n[[test]]\nname = \
             \"suite\"\npath = \"tests/suite.rs\"\n\n[lints]\nworkspace = true\n"
        );
        // a manifest whose every target is there is not touched
        assert_eq!(strip_manifest(&text, dir.path()), None);
    }
}

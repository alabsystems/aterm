// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one shared-lock composition case: the isolated product's version.

use super::{Command, Git, LOCAL, display_git_path};
use aterm_toml::edit::{DocumentMut, Item};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Output;
use std::time::Instant;

const PRODUCT: &[u8] = b"crates/aterm/Cargo.toml";
const IDENTITY: [&[u8]; 3] = [
    b"crates/aterm/build.rs",
    b"crates/aterm-stamp/src/lib.rs",
    b"crates/aterm-census/src/guards/identity_read.rs",
];
/// The four trees [`independent_lock`] reads, in its argument order, as a refusal names them.
const ENDPOINTS: [&str; 4] = ["the base", "new main", "the checked tree", "the merge"];

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    object: String,
}

type Tree = BTreeMap<Vec<u8>, Entry>;

struct Field {
    value: String,
    before: String,
    after: String,
}

impl Field {
    fn same_surroundings(&self, other: &Self) -> bool {
        self.before == other.before && self.after == other.after
    }
}

struct Version {
    workspace: Field,
    lock: Field,
    product: String,
}

/// Why the isolated product-version exception did not compose a sole shared `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The trees were read, and the change is not that move: what differs.
    Differs(String),
    /// The exception could not be judged, and why: a Git read failed or ran out of time, or the
    /// evidence has a shape this exception does not read. It decides nothing about the move.
    Unjudged(String),
}

/// The isolated product version's move, old to new, where a sole shared `Cargo.lock` carries
/// exactly that; else why not. No unavailable, malformed or unsupported evidence authorizes
/// composition, and none is reported as a decided difference ([`Refusal::Unjudged`]).
pub(super) fn independent_lock(
    git: &Git<'_>,
    base: &str,
    main: &str,
    checked: &str,
    merged: &str,
) -> Result<(String, String), Refusal> {
    let until = Instant::now() + LOCAL;
    let mut trees = Vec::with_capacity(ENDPOINTS.len());
    for (id, endpoint) in [base, main, checked, merged].into_iter().zip(ENDPOINTS) {
        trees.push(
            tree(git, id, until).map_err(|why| Refusal::Unjudged(format!("{endpoint}: {why}")))?,
        );
    }
    // These are the existing source boundary and writer, not a claim that versions are
    // inert. A changed boundary needs a judgment rather than this classification.
    for path in IDENTITY.into_iter().chain([PRODUCT]) {
        let shown = display_git_path(path);
        let Some(original) = regular(&trees[0], path) else {
            return Err(Refusal::Unjudged(format!(
                "the base has no regular file {shown}"
            )));
        };
        if let Some(endpoint) = differing(&trees, |tree| regular(tree, path) != Some(original)) {
            return Err(Refusal::Differs(format!(
                "{endpoint}'s {shown} differs from the base's"
            )));
        }
    }
    let mut objects = blobs(git, &trees, until).map_err(Refusal::Unjudged)?;
    close_submodules(git, &mut trees, &mut objects, until)?;
    let view = |index: usize| {
        version(&trees[index], &objects).ok_or_else(|| {
            Refusal::Unjudged(format!(
                "{}'s workspace manifests or Cargo.lock are outside the shapes this exception \
                 reads",
                ENDPOINTS[index]
            ))
        })
    };
    let (base, main, checked, merged) = (view(0)?, view(1)?, view(2)?, view(3)?);
    let differs = |why: &str| Err(Refusal::Differs(why.to_owned()));
    if base.workspace.value == main.workspace.value {
        return differs("new main does not move the workspace version");
    }
    if base.workspace.value != checked.workspace.value {
        return differs("the checked tree's workspace version is not the base's");
    }
    if main.workspace.value != merged.workspace.value {
        return differs("the merge's workspace version is not new main's");
    }
    if base.product != checked.product {
        return differs("the checked tree changes the product's lock entry");
    }
    if !base.workspace.same_surroundings(&main.workspace) {
        return differs("new main changes the root manifest beyond its version");
    }
    if !checked.workspace.same_surroundings(&merged.workspace) {
        return differs("the merge changes the checked root manifest beyond its version");
    }
    if !base.lock.same_surroundings(&main.lock) {
        return differs("new main changes Cargo.lock beyond the product version");
    }
    if !checked.lock.same_surroundings(&merged.lock) {
        return differs("the merge changes the checked Cargo.lock beyond the product version");
    }
    Ok((base.workspace.value, main.workspace.value))
}

/// The first of the endpoints whose tree `differs` holds for, as a refusal names it.
fn differing(trees: &[Tree], differs: impl Fn(&Tree) -> bool) -> Option<&'static str> {
    trees
        .iter()
        .zip(ENDPOINTS)
        .find_map(|(tree, endpoint)| differs(tree).then_some(endpoint))
}

fn member_manifest(path: &[u8]) -> bool {
    path.strip_prefix(b"crates/")
        .and_then(|path| path.strip_suffix(b"/Cargo.toml"))
        .is_some_and(|name| !name.is_empty() && !name.contains(&b'/'))
}

fn manifest(path: &[u8]) -> bool {
    path == b"Cargo.toml" || path.ends_with(b"/Cargo.toml")
}

fn selected(path: &[u8]) -> bool {
    matches!(
        path,
        b"Cargo.lock" | b"crates/aterm/src/lib.rs" | b"crates/aterm/src/main.rs"
    ) || manifest(path)
        || IDENTITY.contains(&path)
}

fn local_command(git: &Git<'_>) -> Command {
    let mut command = (git.make)(git.root);
    command
        .arg("--no-replace-objects")
        .env("GIT_NO_LAZY_FETCH", "1")
        // Git's explicit protocol allowlist overrides per-protocol config. Empty
        // permits no transport, even if an old Git ignores the lazy-fetch setting.
        .env("GIT_ALLOW_PROTOCOL", "");
    command
}

/// A local Git read's complete output, else why there is none: the run failed, ran out of
/// time or exited unsuccessfully.
fn capture(git: &Git<'_>, args: &[&str], until: Instant) -> Result<Output, String> {
    let shown = format!("git {}", args.join(" "));
    let mut command = local_command(git);
    command.args(args);
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut command,
        until,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .map_err(|error| format!("{shown}: {error}"))?;
    succeeded(&shown, output)
}

/// `output` where its command succeeded, else what the command said, or its status.
fn succeeded(shown: &str, output: Output) -> Result<Output, String> {
    if output.status.success() {
        return Ok(output);
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    Err(if said.is_empty() {
        format!("{shown}: {}", output.status)
    } else {
        format!("{shown}: {said}")
    })
}

/// `id`'s recursive tree, holding the entries this exception reads; else why it cannot.
fn tree(git: &Git<'_>, id: &str, until: Instant) -> Result<Tree, String> {
    let args = ["ls-tree", "-r", "-z", "--full-tree", id];
    let out = capture(git, &args, until)?;
    let unread = || {
        format!(
            "git {}: output this exception does not read",
            args.join(" ")
        )
    };
    let rows = out.stdout.strip_suffix(b"\0").ok_or_else(unread)?;
    let mut entries = Tree::new();
    for row in rows.split(|byte| *byte == 0) {
        let tab = row
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(unread)?;
        let path = &row[tab + 1..];
        // Recursive tree output omits ordinary directories. An immediate entry
        // could hide a member manifest behind a symlink or another unsupported shape.
        if path
            .strip_prefix(b"crates/")
            .is_some_and(|name| !name.contains(&b'/'))
        {
            return Err(format!(
                "{} sits directly in crates/ rather than in a member directory",
                display_git_path(path)
            ));
        }
        let words: Vec<_> = std::str::from_utf8(&row[..tab])
            .map_err(|_| unread())?
            .split_whitespace()
            .collect();
        if words.len() != 3 || !aterm_types::land_trailer::object_id(words[2]) {
            return Err(unread());
        }
        if !selected(path) && matches!((words[0], words[1]), ("100644" | "100755", "blob")) {
            continue;
        }
        if !matches!(
            (words[0], words[1]),
            ("100644" | "100755" | "120000", "blob") | ("160000", "commit")
        ) {
            return Err(format!(
                "{} is a {} {} entry",
                display_git_path(path),
                words[0],
                words[1]
            ));
        }
        if entries
            .insert(
                path.to_vec(),
                Entry {
                    mode: words[0].to_owned(),
                    object: words[2].to_owned(),
                },
            )
            .is_some()
        {
            return Err(format!("{} is listed twice", display_git_path(path)));
        }
    }
    Ok(entries)
}

fn regular<'a>(tree: &'a Tree, path: &[u8]) -> Option<&'a Entry> {
    tree.get(path).filter(|entry| entry.mode == "100644")
}

/// The text of every lock and manifest blob the trees name, by object; else why not.
fn blobs(
    git: &Git<'_>,
    trees: &[Tree],
    until: Instant,
) -> Result<BTreeMap<String, String>, String> {
    let wanted: BTreeSet<_> = trees
        .iter()
        .flat_map(|tree| tree.iter())
        .filter(|(path, _)| path.as_slice() == b"Cargo.lock" || manifest(path))
        .map(|(_, entry)| entry.object.as_str())
        .collect();
    let input = wanted
        .iter()
        .map(|id| format!("{id}\n"))
        .collect::<String>();
    let shown = "git cat-file --batch";
    let mut command = local_command(git);
    command.args(["cat-file", "--batch"]);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(input.as_bytes()),
        until,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .map_err(|error| format!("{shown}: {error}"))?;
    let out = succeeded(shown, out)?;
    batch(&out.stdout, wanted).ok_or_else(|| {
        format!(
            "{shown}: an object it was asked for is missing, is not UTF-8 blob text, or was \
             cut short"
        )
    })
}

/// `cat-file --batch` output that holds exactly the `wanted` blobs, in order, as UTF-8 text.
fn batch(mut bytes: &[u8], wanted: BTreeSet<&str>) -> Option<BTreeMap<String, String>> {
    let mut objects = BTreeMap::new();
    for id in wanted {
        let newline = bytes.iter().position(|byte| *byte == b'\n')?;
        let words: Vec<_> = std::str::from_utf8(&bytes[..newline])
            .ok()?
            .split_whitespace()
            .collect();
        if words.len() != 3 || words[0] != id || words[1] != "blob" {
            return None;
        }
        let length = words[2].parse::<usize>().ok()?;
        bytes = bytes.get(newline + 1..)?;
        let text = std::str::from_utf8(bytes.get(..length)?).ok()?;
        objects.insert(id.to_owned(), text.to_owned());
        bytes = bytes.get(length..)?.strip_prefix(b"\n")?;
    }
    bytes.is_empty().then_some(objects)
}

fn text<'a>(tree: &Tree, objects: &'a BTreeMap<String, String>, path: &[u8]) -> Option<&'a str> {
    objects
        .get(&regular(tree, path)?.object)
        .map(String::as_str)
}

/// Close each unchanged child pin using its own initialized object store. Gitlink
/// ownership and an exclude entry alone do not prove a separate Cargo workspace.
fn close_submodules(
    git: &Git<'_>,
    trees: &mut [Tree],
    objects: &mut BTreeMap<String, String>,
    until: Instant,
) -> Result<(), Refusal> {
    let pins = |tree: &Tree| {
        tree.iter()
            .filter(|(_, entry)| entry.mode == "160000")
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Tree>()
    };
    let original = pins(&trees[0]);
    if let Some(endpoint) = differing(trees, |tree| pins(tree) != original) {
        return Err(Refusal::Differs(format!(
            "{endpoint}'s submodule pins differ from the base's"
        )));
    }
    if original.is_empty() {
        return Ok(());
    }
    let mut targets = BTreeSet::new();
    for (tree, endpoint) in trees.iter().zip(ENDPOINTS) {
        targets.extend(path_targets(tree, objects).ok_or_else(|| {
            Refusal::Unjudged(format!(
                "{endpoint}'s path dependencies are outside the shapes this exception reads"
            ))
        })?);
    }
    for (prefix, pin) in original {
        close_submodule(git, trees, objects, &targets, &prefix, &pin, until).map_err(|why| {
            Refusal::Unjudged(format!("submodule {}: {why}", display_git_path(&prefix)))
        })?;
    }
    Ok(())
}

/// Every local path dependency `tree`'s manifests name, normalized from the root.
fn path_targets(tree: &Tree, objects: &BTreeMap<String, String>) -> Option<BTreeSet<String>> {
    let mut targets = BTreeSet::new();
    for path in tree.keys().filter(|path| manifest(path)) {
        let path_text = std::str::from_utf8(path).ok()?;
        let directory = path_text.strip_suffix("/Cargo.toml").unwrap_or("");
        let doc = text(tree, objects, path)?.parse::<DocumentMut>().ok()?;
        targets.extend(dependency_targets(&doc, directory)?);
    }
    Some(targets)
}

/// Close one unchanged pin at `prefix` over its child workspace's manifests; else why not.
fn close_submodule(
    git: &Git<'_>,
    trees: &mut [Tree],
    objects: &mut BTreeMap<String, String>,
    targets: &BTreeSet<String>,
    prefix: &[u8],
    pin: &Entry,
    until: Instant,
) -> Result<(), String> {
    let prefix = std::str::from_utf8(prefix)
        .ok()
        .filter(|prefix| local_path("", prefix).as_deref() == Some(*prefix))
        .ok_or("its path is not canonical")?;
    for tree in trees.iter() {
        match excluded(tree, objects, prefix) {
            Some(true) => {}
            Some(false) => {
                return Err("a root manifest's workspace.exclude does not name it".to_owned());
            }
            None => {
                return Err(
                    "a root manifest has no workspace.exclude array this exception reads"
                        .to_owned(),
                );
            }
        }
    }
    let child_root = git.root.join(prefix);
    let child_git = Git {
        root: &child_root,
        make: git.make,
    };
    let commit = format!("{}^{{commit}}", pin.object);
    let identity = capture(
        &child_git,
        &["rev-parse", "--show-prefix", "--verify", &commit],
        until,
    )?;
    if identity.stdout.as_slice() != format!("\n{}\n", pin.object).as_bytes() {
        return Err(format!(
            "git rev-parse: its pin {} is not a commit at the top of its own checkout",
            pin.object
        ));
    }
    let mut child = tree(&child_git, &pin.object, until)?;
    if child
        .values()
        .any(|entry| !matches!(entry.mode.as_str(), "100644" | "100755"))
    {
        return Err("its tree holds an entry that is not a regular file".to_owned());
    }
    child.retain(|path, _| manifest(path));
    let child_objects = blobs(&child_git, std::slice::from_ref(&child), until)?;
    let closure = child_closure(&child, &child_objects, targets, prefix)
        .ok_or("its workspace is outside the shapes this exception reads")?;
    for (joined, entry, text) in closure {
        for tree in trees.iter_mut() {
            if tree.insert(joined.clone(), entry.clone()).is_some() {
                return Err(format!(
                    "{} is in the parent's tree too",
                    display_git_path(&joined)
                ));
            }
        }
        objects.insert(entry.object, text);
    }
    for tree in trees.iter_mut() {
        tree.remove(prefix.as_bytes())
            .ok_or("its gitlink is missing from a tree")?;
    }
    Ok(())
}

/// Whether `tree`'s root manifest lists `prefix` in `workspace.exclude`; `None` where that
/// manifest cannot be read or parsed, or has no `workspace.exclude` array.
fn excluded(tree: &Tree, objects: &BTreeMap<String, String>, prefix: &str) -> Option<bool> {
    let root = text(tree, objects, b"Cargo.toml")?
        .parse::<DocumentMut>()
        .ok()?;
    Some(
        root.get("workspace")?
            .get("exclude")?
            .as_array()?
            .iter()
            .any(|entry| entry.as_str() == Some(prefix)),
    )
}

/// The child workspace's manifests the parent reaches by path (its root's included), each as
/// its joined parent path, entry and text; `None` where the child is not a separate Cargo
/// workspace this exception reads.
fn child_closure(
    child: &Tree,
    child_objects: &BTreeMap<String, String>,
    targets: &BTreeSet<String>,
    prefix: &str,
) -> Option<Vec<(Vec<u8>, Entry, String)>> {
    let root = text(child, child_objects, b"Cargo.toml")?
        .parse::<DocumentMut>()
        .ok()?;
    root.get("workspace")?.as_table_like()?;
    root.get("workspace")?
        .get("package")?
        .get("version")?
        .as_str()?;
    let mut edges = BTreeMap::new();
    let mut pending = BTreeSet::new();
    for path in child.keys() {
        let path_text = std::str::from_utf8(path).ok()?;
        let directory = path_text.strip_suffix("/Cargo.toml").unwrap_or("");
        let doc = text(child, child_objects, path)?
            .parse::<DocumentMut>()
            .ok()?;
        if doc
            .get("package")
            .is_some_and(|package| package.get("workspace").is_some())
        {
            return None;
        }
        // This inspects even unused fixture manifests for escaping paths. Missing
        // local targets matter only when the manifest is in the path-edge closure.
        edges.insert(directory.to_owned(), dependency_targets(&doc, directory)?);
        if let Some(workspace) = doc.get("workspace") {
            workspace.as_table_like()?;
            pending.insert(directory.to_owned());
        }
    }
    for target in targets {
        if beneath(target, prefix) {
            pending.insert(
                target
                    .strip_prefix(prefix)?
                    .trim_start_matches('/')
                    .to_owned(),
            );
        }
    }
    let mut reached = BTreeSet::new();
    while let Some(directory) = pending.pop_first() {
        if reached.insert(directory.clone()) {
            pending.extend(edges.get(&directory)?.iter().cloned());
        }
    }
    let mut closure = Vec::new();
    for directory in reached {
        let path = if directory.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{directory}/Cargo.toml")
        };
        let entry = regular(child, path.as_bytes())?;
        let text = child_objects.get(&entry.object)?;
        closure.push((
            format!("{prefix}/{path}").into_bytes(),
            entry.clone(),
            text.clone(),
        ));
    }
    Some(closure)
}

/// Only the raw content of a canonical product version may differ. The parser's
/// scalar span includes its quotes; keep those, comments and every other byte.
fn field(text: &str, item: &Item) -> Option<Field> {
    let value = item.as_str()?;
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || part.parse::<u64>().is_err()
        })
    {
        return None;
    }
    let span = item.span()?;
    let raw = text.get(span.clone())?;
    let quote = *raw.as_bytes().first()?;
    if !matches!(quote, b'\'' | b'"')
        || raw.as_bytes().last() != Some(&quote)
        || raw.get(1..raw.len().checked_sub(1)?)? != value
    {
        return None;
    }
    Some(Field {
        value: value.to_owned(),
        before: text.get(..span.start + 1)?.to_owned(),
        after: text.get(span.end - 1..)?.to_owned(),
    })
}

fn beneath(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn local_path(directory: &str, path: &str) -> Option<String> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':']) {
        return None;
    }
    let mut parts: Vec<_> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Normalize only Cargo's dependency tables, rejecting absolute or escaping paths
/// without ambient lookups, feature selection or dependency-version resolution.
fn dependency_targets(manifest: &DocumentMut, directory: &str) -> Option<BTreeSet<String>> {
    let mut scopes = vec![manifest.as_item()];
    if let Some(workspace) = manifest.get("workspace") {
        scopes.push(workspace);
    }
    if let Some(target) = manifest.get("target") {
        scopes.extend(target.as_table_like()?.iter().map(|(_, scope)| scope));
    }
    let mut groups = Vec::new();
    for scope in scopes {
        let scope = scope.as_table_like()?;
        if scope.get("dev_dependencies").is_some() || scope.get("build_dependencies").is_some() {
            return None;
        }
        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(group) = scope.get(key) {
                groups.push(group);
            }
        }
    }
    if let Some(patch) = manifest.get("patch") {
        groups.extend(patch.as_table_like()?.iter().map(|(_, group)| group));
    }
    if let Some(replace) = manifest.get("replace") {
        groups.push(replace);
    }
    let mut targets = BTreeSet::new();
    for group in groups {
        for (_, dependency) in group.as_table_like()?.iter() {
            if dependency.as_str().is_some() {
                continue;
            }
            if let Some(path) = dependency.as_table_like()?.get("path") {
                targets.insert(local_path(directory, path.as_str()?)?);
            }
        }
    }
    Some(targets)
}

fn dependency_paths(manifest: &DocumentMut, directory: &str, tree: &Tree) -> Option<()> {
    for target in dependency_targets(manifest, directory)? {
        regular(tree, format!("{target}/Cargo.toml").as_bytes())?;
    }
    Some(())
}

fn version(tree: &Tree, objects: &BTreeMap<String, String>) -> Option<Version> {
    let root_text = text(tree, objects, b"Cargo.toml")?;
    let root = root_text.parse::<DocumentMut>().ok()?;
    if root.get("package").is_some() {
        return None;
    }
    let workspace = root.get("workspace")?;
    let members = workspace.get("members")?.as_array()?;
    if members.len() != 1 || members.get(0)?.as_str()? != "crates/*" {
        return None;
    }
    let excluded = match workspace.get("exclude") {
        Some(item) => item
            .as_array()?
            .iter()
            .map(|item| item.as_str())
            .collect::<Option<BTreeSet<_>>>()?,
        None => BTreeSet::new(),
    };
    if excluded.iter().any(|path| {
        path.is_empty()
            || path.starts_with('/')
            || path.contains(['*', '?', '[', ']', '\\', ':'])
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    }) {
        return None;
    }
    // Every excluded package must now have an actual manifest. Opaque Gitlinks
    // were closed over their immutable child graph before this inspection.
    for directory in &excluded {
        regular(tree, format!("{directory}/Cargo.toml").as_bytes())?;
    }
    if tree
        .values()
        .any(|entry| !matches!(entry.mode.as_str(), "100644" | "100755"))
    {
        return None;
    }
    let mut manifests = BTreeMap::new();
    for path in tree
        .keys()
        .filter(|path| manifest(path) && path.as_slice() != b"Cargo.toml")
    {
        let name = std::str::from_utf8(path).ok()?;
        let manifest = text(tree, objects, path)?.parse::<DocumentMut>().ok()?;
        if manifest
            .get("package")
            .is_some_and(|package| package.get("workspace").is_some())
            || manifest
                .get("workspace")
                .is_some_and(|workspace| !workspace.is_table_like())
        {
            return None;
        }
        dependency_paths(&manifest, name.strip_suffix("/Cargo.toml")?, tree)?;
        manifests.insert(name, manifest);
    }
    dependency_paths(&root, "", tree)?;
    let nested: Vec<_> = manifests
        .iter()
        .filter_map(|(path, manifest)| {
            manifest
                .get("workspace")
                .and_then(|_| path.strip_suffix("/Cargo.toml"))
        })
        .collect();
    let mut products = 0;
    for (path, manifest) in &manifests {
        let directory = path.strip_suffix("/Cargo.toml")?;
        if !member_manifest(path.as_bytes()) || excluded.contains(directory) {
            let isolated = nested.iter().any(|directory| beneath(path, directory))
                || excluded.contains(directory)
                    && manifest.get("package")?.get("version")?.as_str().is_some();
            if !isolated {
                return None;
            }
            continue;
        }
        let package = manifest.get("package")?;
        if manifest.get("workspace").is_some() {
            return None;
        }
        let inherited = package
            .get("version")?
            .get("workspace")
            .and_then(Item::as_bool);
        if path.as_bytes() == PRODUCT {
            if package.get("name")?.as_str()? != "aterm"
                || inherited != Some(true)
                || package
                    .get("build")
                    .is_some_and(|build| build.as_str() != Some("build.rs"))
                || manifest.get("lib").is_some()
                || tree.contains_key(b"crates/aterm/src/lib.rs".as_slice())
                || regular(tree, b"crates/aterm/src/main.rs").is_none()
                || !manifest
                    .get("bin")?
                    .as_array_of_tables()?
                    .iter()
                    .any(|bin| {
                        bin.get("name").and_then(Item::as_str) == Some("aterm")
                            && bin.get("path").and_then(Item::as_str) == Some("src/main.rs")
                    })
            {
                return None;
            }
            products += 1;
        } else if package.get("name")?.as_str()? == "aterm"
            || package.get("version")?.as_str().is_none()
        {
            return None;
        }
    }
    if products != 1 {
        return None;
    }
    let workspace = field(root_text, workspace.get("package")?.get("version")?)?;
    let lock_text = text(tree, objects, b"Cargo.lock")?;
    let lock = lock_text.parse::<DocumentMut>().ok()?;
    if lock.get("version")?.as_integer()? != 4 {
        return None;
    }
    let packages: Vec<_> = lock.get("package")?.as_array_of_tables()?.iter().collect();
    let mut identities = BTreeSet::new();
    let mut product = None;
    for (index, package) in packages.iter().enumerate() {
        let name = package.get("name")?.as_str()?;
        let version = package.get("version")?.as_str()?;
        let source = match package.get("source") {
            Some(item) => Some(item.as_str()?),
            None => None,
        };
        if !identities.insert((name, version, source)) {
            return None;
        }
        if let Some(dependencies) = package.get("dependencies") {
            for dependency in dependencies.as_array()?.iter() {
                let name = dependency.as_str()?.split_ascii_whitespace().next()?;
                if name == "aterm"
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    return None;
                }
            }
        }
        if name == "aterm" {
            if product.is_some() || source.is_some() || package.get("checksum").is_some() {
                return None;
            }
            let version = field(lock_text, package.get("version")?)?;
            if version.value != workspace.value {
                return None;
            }
            let start = package.span()?.start;
            let end = packages
                .get(index + 1)
                .map_or(Some(lock_text.len()), |next| {
                    next.span().map(|span| span.start)
                })?;
            product = Some((version, lock_text.get(start..end)?.to_owned()));
        }
    }
    let (lock, product) = product?;
    Some(Version {
        workspace,
        lock,
        product,
    })
}

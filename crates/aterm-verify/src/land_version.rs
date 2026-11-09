// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one shared-lock composition case: the isolated product's version.

use super::{Command, Git, LOCAL};
use aterm_toml::edit::{DocumentMut, Item};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

const PRODUCT: &[u8] = b"crates/aterm/Cargo.toml";
const IDENTITY: [&[u8]; 3] = [
    b"crates/aterm/build.rs",
    b"crates/aterm-stamp/src/lib.rs",
    b"crates/aterm-census/src/guards/identity_read.rs",
];

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

/// No unavailable, malformed or unsupported evidence authorizes composition.
pub(super) fn independent_lock(
    git: &Git<'_>,
    base: &str,
    main: &str,
    checked: &str,
    merged: &str,
) -> Option<(String, String)> {
    let until = Instant::now() + LOCAL;
    let mut trees = [base, main, checked, merged]
        .map(|id| tree(git, id, until))
        .into_iter()
        .collect::<Option<Vec<_>>>()?;
    // These are the existing source boundary and writer, not a claim that versions are
    // inert. A changed boundary needs a judgment rather than this classification.
    for path in IDENTITY.into_iter().chain([PRODUCT]) {
        let original = regular(&trees[0], path)?;
        if trees
            .iter()
            .any(|tree| regular(tree, path) != Some(original))
        {
            return None;
        }
    }
    let mut objects = blobs(git, &trees, until)?;
    close_submodules(git, &mut trees, &mut objects, until)?;
    let views = trees
        .iter()
        .map(|tree| version(tree, &objects))
        .collect::<Option<Vec<_>>>()?;
    let [base, main, checked, merged] = views.as_slice() else {
        return None;
    };
    if base.workspace.value == main.workspace.value
        || base.workspace.value != checked.workspace.value
        || main.workspace.value != merged.workspace.value
        || base.product != checked.product
        || !base.workspace.same_surroundings(&main.workspace)
        || !checked.workspace.same_surroundings(&merged.workspace)
        || !base.lock.same_surroundings(&main.lock)
        || !checked.lock.same_surroundings(&merged.lock)
    {
        return None;
    }
    Some((base.workspace.value.clone(), main.workspace.value.clone()))
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

fn capture(git: &Git<'_>, args: &[&str], until: Instant) -> Option<std::process::Output> {
    let mut command = local_command(git);
    command.args(args);
    aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut command,
        until,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .ok()
    .filter(|output| output.status.success())
}

fn tree(git: &Git<'_>, id: &str, until: Instant) -> Option<Tree> {
    let out = capture(git, &["ls-tree", "-r", "-z", "--full-tree", id], until)?;
    if !out.stdout.ends_with(b"\0") {
        return None;
    }
    let mut entries = Tree::new();
    for row in out.stdout.strip_suffix(b"\0")?.split(|byte| *byte == 0) {
        let tab = row.iter().position(|byte| *byte == b'\t')?;
        let path = &row[tab + 1..];
        // Recursive tree output omits ordinary directories. An immediate entry
        // could hide a member manifest behind a symlink or another unsupported shape.
        if path
            .strip_prefix(b"crates/")
            .is_some_and(|name| !name.contains(&b'/'))
        {
            return None;
        }
        let words: Vec<_> = std::str::from_utf8(&row[..tab])
            .ok()?
            .split_whitespace()
            .collect();
        if words.len() != 3 || !aterm_types::land_trailer::object_id(words[2]) {
            return None;
        }
        if !selected(path) && matches!((words[0], words[1]), ("100644" | "100755", "blob")) {
            continue;
        }
        if !matches!(
            (words[0], words[1]),
            ("100644" | "100755" | "120000", "blob") | ("160000", "commit")
        ) || entries
            .insert(
                path.to_vec(),
                Entry {
                    mode: words[0].to_owned(),
                    object: words[2].to_owned(),
                },
            )
            .is_some()
        {
            return None;
        }
    }
    Some(entries)
}

fn regular<'a>(tree: &'a Tree, path: &[u8]) -> Option<&'a Entry> {
    tree.get(path).filter(|entry| entry.mode == "100644")
}

fn blobs(git: &Git<'_>, trees: &[Tree], until: Instant) -> Option<BTreeMap<String, String>> {
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
    let mut command = local_command(git);
    command.args(["cat-file", "--batch"]);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(input.as_bytes()),
        until,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut bytes = out.stdout.as_slice();
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
) -> Option<()> {
    let pins = |tree: &Tree| {
        tree.iter()
            .filter(|(_, entry)| entry.mode == "160000")
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Tree>()
    };
    let original = pins(trees.first()?);
    if trees.iter().any(|tree| pins(tree) != original) {
        return None;
    }
    if original.is_empty() {
        return Some(());
    }
    let mut targets = BTreeSet::new();
    for tree in trees.iter() {
        for path in tree.keys().filter(|path| manifest(path)) {
            let path_text = std::str::from_utf8(path).ok()?;
            let directory = path_text.strip_suffix("/Cargo.toml").unwrap_or("");
            let doc = text(tree, objects, path)?.parse::<DocumentMut>().ok()?;
            targets.extend(dependency_targets(&doc, directory)?);
        }
    }
    for (prefix, pin) in original {
        let prefix = std::str::from_utf8(&prefix).ok()?;
        if local_path("", prefix)? != prefix {
            return None;
        }
        for tree in trees.iter() {
            let root = text(tree, objects, b"Cargo.toml")?
                .parse::<DocumentMut>()
                .ok()?;
            if !root
                .get("workspace")?
                .get("exclude")?
                .as_array()?
                .iter()
                .any(|entry| entry.as_str() == Some(prefix))
            {
                return None;
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
            return None;
        }
        let mut child = tree(&child_git, &pin.object, until)?;
        if child
            .values()
            .any(|entry| !matches!(entry.mode.as_str(), "100644" | "100755"))
        {
            return None;
        }
        child.retain(|path, _| manifest(path));
        let child_objects = blobs(&child_git, std::slice::from_ref(&child), until)?;
        let root = text(&child, &child_objects, b"Cargo.toml")?
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
            let doc = text(&child, &child_objects, path)?
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
        for target in &targets {
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
        for directory in reached {
            let path = if directory.is_empty() {
                "Cargo.toml".to_owned()
            } else {
                format!("{directory}/Cargo.toml")
            };
            let entry = regular(&child, path.as_bytes())?;
            let joined = format!("{prefix}/{path}").into_bytes();
            for tree in trees.iter_mut() {
                if tree.insert(joined.clone(), entry.clone()).is_some() {
                    return None;
                }
            }
            objects.insert(
                entry.object.clone(),
                child_objects.get(&entry.object)?.clone(),
            );
        }
        for tree in trees.iter_mut() {
            tree.remove(prefix.as_bytes())?;
        }
    }
    Some(())
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

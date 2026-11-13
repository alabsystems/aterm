// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Whether a member's change is confined to the member itself ([`confined`]): no dependent's
//! build reads any of it, so no dependent's build, lint or test can see it, and the cone judges
//! that member alone, with all of its own rows ([`crate::changed::select`]). A dependent links the
//! member's library as it builds without `cfg(test)`; only the member's own test targets compile
//! its test code, and only the member's own lint and doctests read its comments.
//!
//! A changed file is confined when all of these hold:
//! - It is a `.rs` file of the package's library, binary or integration-test sources: not under
//!   `benches/` or `examples/`, no `[[bench]]` or `[[example]]` path, not the build script. Or it
//!   is the package's own `Cargo.toml` and its TOML values are the same before and after, so
//!   Cargo reads it as nothing changed.
//! - Either the code a build without tests compiles of it is the same, token for token, each on
//!   the same line after the same text, before and after, comments apart
//!   ([`aterm_census::items::Source::code`]: every other hunk is inside a test-only item or a
//!   comment), and each comment that changed is one no macro may read; or no build without tests
//!   compiles it, before or after: an integration-test root or a module declaration in test code
//!   names it, none outside test code may, and no `include!` outside test code may read it. A
//!   declaration is matched to files by name alone, ignoring ASCII case as the Mac's file system
//!   does (`mod x;` may be any `x.rs` or `x/mod.rs` in the package, `#[path = "…/y.rs"]` any
//!   `y.rs`), and one whose path or name this reader cannot settle may be any file. A file no
//!   declaration names may be one a macro declares, so it is judged by its tokens.
//! - A changed comment is one no macro may read: none of the census reader's carriers holds it in
//!   either version ([`aterm_census::items::Comment::carried`]: a macro's input, an item a derive,
//!   an attribute macro or a `doc = …` sits on, a Trust `clean { … }` island, and what that
//!   reader cannot read); no declaration a macro may read, or whose path this reader cannot
//!   settle, may mount the file, nor any file such a declaration mounts; no `include!` outside
//!   test code may read its tokens; and in each version it is placed: a target root (the
//!   library's, a binary's, an integration test's) or a file a declaration this reader places
//!   names (its shipped or tested set). Plain comments are no tokens, and a doc comment no macro
//!   reads is a doc string only the member's own lint and doctests read.
//! - Nothing outside test code may embed its bytes (`include_str!`, `include_bytes!`, an include
//!   this reader cannot settle) or names it in a string literal.
//! - The build script cannot read it: the package has no build-dependencies (whose code may declare
//!   inputs this reader cannot see), the script and every file it mounts declare its inputs with
//!   literal `cargo:rerun-if-changed` lines, none of which is or holds the file, none of their
//!   string literals names it or a folder holding it, and none of them finds the package's folder
//!   (`CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `current_dir`) to walk it.
//!
//! Anything this reader cannot settle is a [`Doubt`], and a doubt keeps the whole cone. What this
//! reader takes on trust: a macro from another crate declares no module from a file a test-only
//! declaration may also name, and no dependent reads the member's sources as files at run time
//! without naming them. What other members read is [`crate::changed::resolve_paths`]'s to
//! judge: a member file that reads another's by a `../` literal, or names its path, is not
//! confined.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

use aterm_census::items::{Comment, IncludeArg, IncludeKind, Mount, MountPath, Source};

/// A checkout's files as [`confined`] reads them, by repo-relative path.
pub trait Files {
    /// Every file under the directory `dir`, by repo-relative path.
    fn under(&self, dir: &str) -> Vec<String>;

    /// The file's text now, or `None` where it does not exist.
    ///
    /// # Errors
    /// It exists but cannot be read as text.
    fn now(&self, path: &str) -> Result<Option<String>, String>;

    /// The file's text at the base the change is measured from, or `None` where it did not exist.
    ///
    /// # Errors
    /// It cannot be read there.
    fn before(&self, path: &str) -> Result<Option<String>, String>;
}

/// Why a dependent's build may read a changed file. Each keeps the whole cone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doubt {
    /// It is no Rust source: a manifest, a fixture, a document.
    NotRust(String),
    /// It is a bench or example target's file, the build script, or outside the package.
    OtherTarget(String),
    /// A file this reads does not read or lex, or the manifest does not parse.
    Unread(String),
    /// What a build without tests compiles of it changed: a token other than a comment, or a
    /// token's place.
    Shipped(String),
    /// Its comments changed where a macro or another front end may read them: in a macro's
    /// input, on or in an item a macro reads, in a Trust `clean { … }` island
    /// ([`aterm_census::items::Comment::carried`]), or anywhere in a file a macro may mount or
    /// that, in a version, is neither a target root nor named by a declaration this reader
    /// places.
    Carried(String),
    /// Its comments changed, and an `include!` outside the tests may read its tokens.
    Included(String),
    /// Code outside the tests may embed its bytes.
    Embedded(String),
    /// A string literal outside the tests names it.
    Named(String),
    /// The build script may read it.
    BuildInput(String),
}

/// Whether every one of `changed`, the repo-relative paths that changed in the package whose
/// manifest is in `dir`, is confined to the package itself (the module's rules), or the first
/// doubt.
///
/// # Errors
/// The first [`Doubt`] of a changed file.
pub fn confined(files: &dyn Files, dir: &str, changed: &[&str]) -> Result<(), Doubt> {
    let manifest_path = join(dir, "Cargo.toml");
    if let Some(path) = changed.iter().find(|path| {
        !path.ends_with(".rs") && (**path != manifest_path || !same_values(files, path))
    }) {
        return Err(Doubt::NotRust((*path).to_string()));
    }
    let manifest = files
        .now(&manifest_path)
        .ok()
        .flatten()
        .and_then(|text| Manifest::parse(&text))
        .ok_or_else(|| Doubt::Unread(manifest_path.clone()))?;
    if let Some(path) = changed
        .iter()
        .find(|path| within(dir, path).is_none_or(|rel| manifest.other_target(rel)))
    {
        return Err(Doubt::OtherTarget((*path).to_string()));
    }
    let changed: BTreeSet<&str> = changed.iter().copied().collect();
    let listed: BTreeSet<String> = files
        .under(dir)
        .into_iter()
        .filter(|path| path.ends_with(".rs"))
        .chain(changed.iter().map(|path| (*path).to_string()))
        .collect();
    let mut reader = Reader {
        files,
        changed: &changed,
        now: BTreeMap::new(),
        before: BTreeMap::new(),
    };
    let build = BuildInputs::read(&mut reader, dir, &manifest);
    let reaches =
        [When::Before, When::Now].map(|when| Reach::of(&mut reader, dir, &manifest, &listed, when));
    let [before, now] = reaches;
    let (before, now) = (before?, now?);
    for &path in &changed {
        if build.as_ref().is_none_or(|inputs| inputs.may_read(path)) {
            return Err(Doubt::BuildInput(path.to_string()));
        }
        if before.embeds(path) || now.embeds(path) {
            return Err(Doubt::Embedded(path.to_string()));
        }
        if before.named.contains(path) || now.named.contains(path) {
            return Err(Doubt::Named(path.to_string()));
        }
        // The package's manifest, whose values are the same before and after.
        if !path.ends_with(".rs") {
            continue;
        }
        // A file no declaration names in a version it exists in may be one a macro declares.
        let unplaced = |reach: &Reach, when: When| -> Result<bool, Doubt> {
            Ok(!reach.tested.contains(path) && reader.text(path, when)?.is_some())
        };
        if before.shipped.contains(path)
            || now.shipped.contains(path)
            || unplaced(&before, When::Before)?
            || unplaced(&now, When::Now)?
        {
            let (Some(was), Some(is)) = (
                reader.text(path, When::Before)?,
                reader.text(path, When::Now)?,
            ) else {
                return Err(Doubt::Shipped(path.to_string()));
            };
            let (Some(was), Some(is)) = (Source::read(was), Source::read(is)) else {
                return Err(Doubt::Unread(path.to_string()));
            };
            if was.code() != is.code() {
                return Err(Doubt::Shipped(path.to_string()));
            }
            let edited = edited(&was, &is);
            if edited.is_empty() {
                continue;
            }
            if before.included.contains(path) || now.included.contains(path) {
                return Err(Doubt::Included(path.to_string()));
            }
            let placed =
                |reach: &Reach| reach.shipped.contains(path) || reach.tested.contains(path);
            if before.carried.contains(path)
                || now.carried.contains(path)
                || !placed(&before)
                || !placed(&now)
                || edited.iter().any(|comment| comment.carried)
            {
                return Err(Doubt::Carried(path.to_string()));
            }
        }
    }
    Ok(())
}

/// The comments of either version that sit among the code where the two differ: every one, of
/// both, between two code tokens whose comments are not the same, each in the same place. The
/// code must be the same, so each gap is the same place in both.
fn edited<'a>(was: &'a Source, is: &'a Source) -> Vec<Comment<'a>> {
    let by_gap = |source: &'a Source| {
        let mut gaps: BTreeMap<usize, Vec<Comment<'a>>> = BTreeMap::new();
        for comment in source.comments() {
            gaps.entry(comment.gap).or_default().push(comment);
        }
        gaps
    };
    let (was, is) = (by_gap(was), by_gap(is));
    let places = |comments: Option<&Vec<Comment<'a>>>| {
        comments.map_or_else(Vec::new, |comments| comments.iter().map(|c| c.at).collect())
    };
    was.keys()
        .chain(is.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|gap| places(was.get(gap)) != places(is.get(gap)))
        .flat_map(|gap| {
            was.get(gap)
                .into_iter()
                .chain(is.get(gap))
                .flatten()
                .copied()
        })
        .collect()
}

/// Whether the manifest at `path` holds the same TOML values before and after, so Cargo reads it
/// as if it had not changed. A manifest either version lacks, or one that does not read or
/// parse, holds no such values.
fn same_values(files: &dyn Files, path: &str) -> bool {
    let value = |text: Result<Option<String>, String>| {
        aterm_toml::from_str::<aterm_toml::Value>(&text.ok()??).ok()
    };
    let before = value(files.before(path));
    before.is_some() && before == value(files.now(path))
}

/// What a package's manifest says of its targets, by package-relative path.
struct Manifest {
    /// What a build without tests compiles from: the library's root and each binary's.
    roots: Vec<String>,
    /// The explicit `[[test]]` paths. Default integration-test roots are recognized separately.
    tests: Vec<String>,
    /// Whether Cargo discovers default integration-test roots.
    autotests: bool,
    /// The `[[bench]]` and `[[example]]` paths.
    others: Vec<String>,
    /// The build script's path (`build.rs` unless `build` names another), or empty for `build =
    /// false`.
    build: String,
    /// It has build-dependencies, on any target.
    build_deps: bool,
}

impl Manifest {
    fn parse(text: &str) -> Option<Self> {
        use aterm_toml::Value;
        let Value::Table(table) = aterm_toml::from_str::<Value>(text).ok()? else {
            return None;
        };
        let path_of = |target: &Value| target.get("path").and_then(Value::as_str).map(normalize);
        // Without a literal path, default discovery below may establish the root; otherwise
        // it remains unknown and keeps the conservative cone.
        let paths_of = |key: &str| match table.get(key) {
            None => Some(Vec::new()),
            Some(Value::Array(targets)) => Some(targets.iter().filter_map(path_of).collect()),
            Some(_) => None,
        };
        let mut roots = vec!["src/lib.rs".to_string(), "src/main.rs".to_string()];
        roots.extend(table.get("lib").and_then(path_of));
        roots.extend(paths_of("bin")?);
        let mut others = Vec::new();
        for key in ["bench", "example"] {
            others.extend(paths_of(key)?);
        }
        let build = match table
            .get("package")
            .and_then(|package| package.get("build"))
        {
            None | Some(Value::Boolean(true)) => "build.rs".to_string(),
            Some(Value::Boolean(false)) => String::new(),
            Some(Value::String(path)) => normalize(path),
            Some(_) => return None,
        };
        let deps = |t: &aterm_toml::Table| {
            t.get("build-dependencies").is_some() || t.get("build_dependencies").is_some()
        };
        let build_deps = deps(&table)
            || table
                .get("target")
                .and_then(Value::as_table)
                .is_some_and(|targets| {
                    targets
                        .values()
                        .any(|target| target.as_table().is_some_and(deps))
                });
        Some(Self {
            roots,
            tests: paths_of("test")?,
            autotests: match table.get("package").and_then(|p| p.get("autotests")) {
                None => true,
                Some(Value::Boolean(enabled)) => *enabled,
                _ => return None,
            },
            others,
            build,
            build_deps,
        })
    }

    /// Whether this target's location requires the conservative whole cone.
    fn other_target(&self, rel: &str) -> bool {
        ["benches/", "examples/"]
            .iter()
            .any(|dir| rel.starts_with(dir))
            || self.others.iter().any(|other| other == rel)
            || rel == self.build
    }

    /// A Cargo integration-test root: an explicitly named path, or one discovered by Cargo.
    fn test_root(&self, rel: &str) -> bool {
        self.tests.iter().any(|test| test == rel)
            || self.autotests
                && rel.strip_prefix("tests/").is_some_and(|rest| {
                    !rest.contains('/')
                        || rest
                            .strip_suffix("/main.rs")
                            .is_some_and(|dir| !dir.contains('/'))
                })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum When {
    Before,
    Now,
}

/// What a file brings into a build, for [`Reach::of`]: its module declarations and includes, each
/// saying whether it sits in test code, and the string literals outside its test code.
#[derive(Clone)]
struct Read {
    mounts: Vec<Mount>,
    includes: Vec<(IncludeKind, IncludeArg, bool)>,
    strings: Vec<Option<String>>,
}

impl Read {
    fn of(source: &Source) -> Self {
        Self {
            mounts: source.mounts().to_vec(),
            includes: source
                .includes()
                .iter()
                .map(|include| {
                    (
                        include.kind,
                        include.arg.clone(),
                        source.test_only(include.at),
                    )
                })
                .collect(),
            strings: source
                .strings()
                .filter(|&(_, at)| !source.test_only(at))
                .map(|(value, _)| value.map(str::to_string))
                .collect(),
        }
    }
}

/// The files, read once each: a changed file in each version, any other file once.
struct Reader<'a> {
    files: &'a dyn Files,
    changed: &'a BTreeSet<&'a str>,
    now: BTreeMap<String, Option<Option<Read>>>,
    before: BTreeMap<String, Option<Option<Read>>>,
}

impl Reader<'_> {
    /// `path`'s text in the version `when` reads.
    fn text(&self, path: &str, when: When) -> Result<Option<String>, Doubt> {
        let read = if when == When::Before && self.changed.contains(path) {
            self.files.before(path)
        } else {
            self.files.now(path)
        };
        read.map_err(|_| Doubt::Unread(path.to_string()))
    }

    fn exists_now(&self, path: &str) -> Result<bool, Doubt> {
        Ok(!path.ends_with('/') && self.text(path, When::Now)?.is_some())
    }

    /// What `path` brings into a build in the version `when` reads, or `None` where it does not
    /// exist there.
    fn read(&mut self, path: &str, when: When) -> Result<Option<Read>, Doubt> {
        let before = when == When::Before && self.changed.contains(path);
        if let Some(found) = (if before { &self.before } else { &self.now }).get(path) {
            return found.clone().ok_or_else(|| Doubt::Unread(path.to_string()));
        }
        let found = match self.text(path, when)? {
            None => Some(None),
            Some(text) => Source::read(text).map(|source| Some(Read::of(&source))),
        };
        (if before {
            &mut self.before
        } else {
            &mut self.now
        })
        .insert(path.to_string(), found.clone());
        found.ok_or_else(|| Doubt::Unread(path.to_string()))
    }
}

/// What a build without tests reads of the package in one version.
#[derive(Default)]
struct Reach {
    /// The files it may compile.
    shipped: BTreeSet<String>,
    /// The files among those an `include!` may read the tokens of.
    included: BTreeSet<String>,
    /// The files among those a macro may read the tokens of, mounted by a declaration a macro
    /// may read ([`Mount::carried`]) or whose path this reader cannot settle, or by any
    /// declaration or `include!` such a file holds.
    carried: BTreeSet<String>,
    /// The files a declaration or `include!` in test code may bring in, and theirs in turn.
    tested: BTreeSet<String>,
    /// The files it may embed the bytes of.
    embedded: BTreeSet<String>,
    /// It may embed any file.
    embeds_any: bool,
    /// The changed files a string literal it compiles names.
    named: BTreeSet<String>,
}

impl Reach {
    fn of(
        reader: &mut Reader<'_>,
        dir: &str,
        manifest: &Manifest,
        listed: &BTreeSet<String>,
        when: When,
    ) -> Result<Self, Doubt> {
        let mut reach = Self::default();
        // Each with whether a macro may read its tokens.
        let mut queue: Vec<(String, bool)> = listed
            .iter()
            .filter(|path| {
                within(dir, path).is_some_and(|rel| {
                    manifest.roots.iter().any(|root| root == rel) || rel.starts_with("src/bin/")
                })
            })
            .map(|path| (path.clone(), false))
            .collect();
        // Only a Rust source declares a module or brings in tokens.
        let sources = || listed.iter().filter(|path| path.ends_with(".rs"));
        // What test code of a shipped file brings in, and everything those files bring in.
        let mut tested: Vec<String> = listed
            .iter()
            .filter(|path| within(dir, path).is_some_and(|rel| manifest.test_root(rel)))
            .cloned()
            .collect();
        while let Some((file, carried)) = queue.pop() {
            if reach.shipped.contains(&file) && (!carried || reach.carried.contains(&file)) {
                continue;
            }
            let Some(read) = reader.read(&file, when)? else {
                continue;
            };
            reach.shipped.insert(file.clone());
            if carried {
                reach.carried.insert(file);
            }
            for mount in &read.mounts {
                let named = sources().filter(|path| may_mount(mount, path)).cloned();
                if mount.test {
                    tested.extend(named);
                } else {
                    let carried = carried
                        || mount.carried
                        || mount.name.is_none()
                        || mount.path == MountPath::Unsettled;
                    queue.extend(named.map(|path| (path, carried)));
                }
            }
            for (kind, arg, test) in &read.includes {
                let targets = included(listed, arg);
                if *test {
                    if *kind == IncludeKind::Tokens {
                        tested.extend(targets.into_iter().filter(|path| path.ends_with(".rs")));
                    }
                } else if *kind == IncludeKind::Tokens {
                    let targets: Vec<String> = targets
                        .into_iter()
                        .filter(|path| path.ends_with(".rs"))
                        .collect();
                    reach.included.extend(targets.iter().cloned());
                    queue.extend(targets.into_iter().map(|path| (path, carried)));
                } else {
                    reach.embeds_any |= *arg == IncludeArg::Unknown;
                    reach.embedded.extend(targets);
                }
            }
            for value in &read.strings {
                reach.named.extend(
                    reader
                        .changed
                        .iter()
                        .filter(|path| value.as_deref().is_none_or(|value| names(value, path)))
                        .map(|path| (*path).to_string()),
                );
            }
        }
        while let Some(file) = tested.pop() {
            if !reach.tested.insert(file.clone()) {
                continue;
            }
            let Some(read) = reader.read(&file, when)? else {
                continue;
            };
            for mount in &read.mounts {
                tested.extend(sources().filter(|path| may_mount(mount, path)).cloned());
            }
            for (kind, arg, _) in &read.includes {
                if *kind == IncludeKind::Tokens {
                    tested.extend(
                        included(listed, arg)
                            .into_iter()
                            .filter(|path| path.ends_with(".rs")),
                    );
                }
            }
        }
        Ok(reach)
    }

    fn embeds(&self, path: &str) -> bool {
        self.embeds_any || self.embedded.contains(path)
    }
}

/// What the build script reads: the files it compiles and the paths it declares.
struct BuildInputs {
    /// The package's repo-relative directory, the script's working directory.
    dir: String,
    /// Every file the script compiles or embeds, by repo-relative path.
    sources: BTreeSet<String>,
    /// Each `cargo:rerun-if-changed` path, repo-relative.
    declared: Vec<String>,
    /// Every other string literal of its sources.
    strings: Vec<String>,
}

/// How rustc reached a build script's file, which says where its module declarations' files are.
#[derive(Clone, Copy)]
enum Loaded {
    /// A crate root, a `mod.rs` or a `#[path]` file: beside it.
    Root,
    /// `<name>.rs` by default: in the folder `<name>`.
    Named,
    /// By `include!`: where the includer's are, which this reader does not follow.
    Included,
}

impl BuildInputs {
    /// What the package's build script reads, or `None` where there is a script and this reader
    /// cannot settle what it reads. A package with no script reads nothing.
    fn read(reader: &mut Reader<'_>, dir: &str, manifest: &Manifest) -> Option<Self> {
        let script = join(dir, &manifest.build);
        let mut inputs = Self {
            dir: dir.to_string(),
            sources: BTreeSet::new(),
            declared: Vec::new(),
            strings: Vec::new(),
        };
        if manifest.build.is_empty() || !reader.exists_now(&script).ok()? {
            return Some(inputs);
        }
        if manifest.build_deps {
            return None;
        }
        let mut queue = vec![(script, Loaded::Root)];
        while let Some((file, loaded)) = queue.pop() {
            if !inputs.sources.insert(file.clone()) {
                continue;
            }
            let source = Source::read(reader.text(&file, When::Now).ok()??)?;
            let folder = parent(&file);
            for mount in source.mounts().iter().filter(|mount| !mount.test) {
                let name = mount.name.as_deref().filter(|_| mount.top)?;
                match (&mount.path, loaded) {
                    (_, Loaded::Included) | (MountPath::Unsettled, _) => return None,
                    (MountPath::Literal(path), _) => {
                        queue.push((normalize(&join(folder, path)), Loaded::Root));
                    }
                    (MountPath::Default, _) => {
                        let base = match loaded {
                            Loaded::Root => folder.to_string(),
                            _ => join(folder, stem(&file)),
                        };
                        let found = [
                            (join(&base, &format!("{name}.rs")), Loaded::Named),
                            (join(&base, &format!("{name}/mod.rs")), Loaded::Root),
                        ]
                        .into_iter()
                        .find(|(path, _)| reader.exists_now(path).unwrap_or(false))?;
                        queue.push(found);
                    }
                }
            }
            for include in source.includes() {
                if source.test_only(include.at) {
                    continue;
                }
                let path = match &include.arg {
                    IncludeArg::Relative(path) if !include.in_macro => {
                        normalize(&join(folder, path))
                    }
                    IncludeArg::ManifestDir(path) => normalize(&join(dir, path)),
                    _ => return None,
                };
                if include.kind == IncludeKind::Tokens {
                    queue.push((path, Loaded::Included));
                } else {
                    inputs.sources.insert(path);
                }
            }
            // A script that finds its package's folder may walk all of it.
            if source.strings().any(|(value, _)| {
                matches!(value, Some("CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH"))
            }) || reader.text(&file, When::Now).ok()??.contains("current_dir")
            {
                return None;
            }
            for (value, _) in source.strings() {
                let value = value?;
                if value.contains("rerun-if-changed") {
                    let path = value
                        .strip_prefix("cargo:rerun-if-changed=")
                        .or_else(|| value.strip_prefix("cargo::rerun-if-changed="))
                        .filter(|path| {
                            !path.is_empty() && !path.starts_with('/') && !path.contains(['{', '}'])
                        })?;
                    inputs.declared.push(normalize(&join(dir, path)));
                } else {
                    inputs.strings.push(value.to_string());
                }
            }
        }
        // A script that declares nothing reruns on any change in its package.
        (!inputs.declared.is_empty()).then_some(inputs)
    }

    /// Whether the script may read the repo-relative `path`: it compiles or embeds it, declares it
    /// or a folder holding it, or a string literal of it names it ([`names`]) or, from the
    /// script's working directory, it or a folder holding it (after a leading `{…}/`, as in
    /// `format!("{}/src", dir)`).
    fn may_read(&self, path: &str) -> bool {
        let holds = |dir: &str| path == dir || path.starts_with(&format!("{dir}/"));
        self.sources.contains(path)
            || self.declared.iter().any(|declared| holds(declared))
            || self.strings.iter().any(|value| {
                let rest = value
                    .strip_prefix('{')
                    .and_then(|value| value.split_once('}'))
                    .map_or(value.as_str(), |(_, rest)| rest);
                let folded = normalize(&join(&self.dir, rest.trim().trim_start_matches('/')));
                names(value, path) || (!rest.trim().is_empty() && holds(&folded))
            })
    }
}

/// `rel` under the repo-relative `dir`.
fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// The repo-relative `path` relative to `dir`, or `None` outside it.
fn within<'a>(dir: &str, path: &'a str) -> Option<&'a str> {
    if dir.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(dir)?.strip_prefix('/')
    }
}

fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..at])
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(path: &str) -> &str {
    let name = file_name(path);
    name.strip_suffix(".rs").unwrap_or(name)
}

/// `path` with its `.` and `..` components folded and `\` read as `/` (a `..` past the start
/// stays, so it names nothing in the repository).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Whether `named`, a path a declaration or include spells, may be the file at `path`: it ends in
/// that file's name, in any ASCII case.
fn same_name(named: &str, path: &str) -> bool {
    let name = file_name(path);
    [named.rsplit('/').next(), named.rsplit(['/', '\\']).next()]
        .into_iter()
        .flatten()
        .any(|last| last.eq_ignore_ascii_case(name))
}

/// Whether the module declaration `mount` may name the file at `path` (the module's rules).
fn may_mount(mount: &Mount, path: &str) -> bool {
    match (&mount.path, mount.name.as_deref()) {
        (MountPath::Default, Some(name)) => {
            file_name(path).eq_ignore_ascii_case(&format!("{name}.rs"))
                || (file_name(path) == "mod.rs"
                    && file_name(parent(path)).eq_ignore_ascii_case(name))
        }
        (MountPath::Literal(named), Some(_)) => same_name(named, path),
        _ => true,
    }
}

/// The listed files an include of `arg` may read: by name ([`same_name`]), none for what the
/// build script wrote (its inputs are [`BuildInputs`]'), any for a path this reader cannot settle.
fn included(listed: &BTreeSet<String>, arg: &IncludeArg) -> Vec<String> {
    match arg {
        IncludeArg::OutDir => Vec::new(),
        IncludeArg::Relative(named) | IncludeArg::ManifestDir(named) => listed
            .iter()
            .filter(|path| same_name(named, path))
            .cloned()
            .collect(),
        IncludeArg::Unknown => listed.iter().cloned().collect(),
    }
}

/// Whether a string literal's `value` names the file at `path`: it is the file's name or ends in a
/// separator and that name, as any path to it does wherever it starts.
fn names(value: &str, path: &str) -> bool {
    let value = value.trim();
    let name = file_name(path);
    [value.rsplit('/').next(), value.rsplit(['/', '\\']).next()]
        .into_iter()
        .flatten()
        .any(|last| last.eq_ignore_ascii_case(name))
}

/// A checkout's [`Files`]: the working tree now, and `base` through git.
pub struct Checkout<'a> {
    pub root: &'a Path,
    pub path_env: &'a OsStr,
    pub base: &'a str,
    /// Every file the checkout holds, tracked or not ([`crate::changed`]'s listing).
    pub listed: &'a [String],
}

impl Files for Checkout<'_> {
    fn under(&self, dir: &str) -> Vec<String> {
        self.listed
            .iter()
            .filter(|path| within(dir, path).is_some())
            .cloned()
            .collect()
    }

    fn now(&self, path: &str) -> Result<Option<String>, String> {
        match crate::read_regular_text(self.root.join(path)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{path}: {e}")),
        }
    }

    fn before(&self, path: &str) -> Result<Option<String>, String> {
        let object = format!("{}:{path}", self.base);
        let held = crate::changed::git(self.root, self.path_env)
            .args(["cat-file", "-e", &object])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("git cat-file: {e}"))?;
        if !held.success() {
            return Ok(None);
        }
        let out = crate::changed::git(self.root, self.path_env)
            .args(["cat-file", "blob", &object])
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("git cat-file: {e}"))?;
        if !out.status.success() {
            return Err(format!("git cat-file blob {object} failed"));
        }
        String::from_utf8(out.stdout)
            .map(Some)
            .map_err(|_| format!("{object} is not UTF-8"))
    }
}

#[cfg(test)]
#[path = "own_tests_tests.rs"]
mod tests;

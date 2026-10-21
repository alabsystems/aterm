// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Formula and cask models parsed from formulae.brew.sh JSON, plus
//! Homebrew-shaped version comparison (design §3, §6).

use std::cmp::Ordering;

use aterm_json::Value;

use super::machine::Machine;

/// A formula as abrew needs it. Parsed from the API object; fields
/// abrew does not use are dropped on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: String,
    pub homepage: String,
    pub license: String,
    pub version: String,
    pub revision: u64,
    pub stable_url: String,
    pub stable_sha256: String,
    /// Bottle files by tag (`arm64_tahoe`, `all`), sorted.
    pub bottle_files: Vec<Bottle>,
    /// Why no bottle of this formula pours natively whatever its cellar: a
    /// post-install recipe, a pour condition, requirements or conflicts abrew
    /// cannot evaluate, a variation it cannot resolve. Such a pour, like a
    /// relocating bottle's, uses the announced brew fallback.
    pub pour_needs: Option<String>,
    /// `conflicts_with` names, evaluated against abrew's census at install.
    pub conflicts: Vec<String>,
    /// Requirements in the plain data forms abrew evaluates at install.
    pub requirements: Vec<Requirement>,
    /// `pour_bottle_only_if: :clt_installed`.
    pub pour_requires_clt: bool,
    /// Per-system deviations, resolved by `resolve_variations` before use.
    pub variations: Vec<(String, FormulaVariation)>,
    pub dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub keg_only: bool,
    pub deprecate: bool,
    pub disable: bool,
    pub caveats: String,
    pub aliases: Vec<String>,
    pub oldnames: Vec<String>,
}

/// One bottle file of a formula's stable version.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bottle {
    pub tag: String,
    pub url: String,
    pub sha256: String,
    /// Where the bottle was built to live: only `:any_skip_relocation`
    /// needs no binary relocation.
    pub cellar: String,
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn empty_metadata(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(items)) => items.is_empty(),
        _ => false,
    }
}

/// A present array must retain every supplied string; filtering cannot prove completeness.
fn plain_strings(value: Option<&Value>) -> Option<Vec<String>> {
    match value {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

fn plain_names(value: Option<&Value>) -> Option<Vec<String>> {
    if empty_metadata(value) {
        return Some(Vec::new());
    }
    let names = plain_strings(value)?;
    names
        .iter()
        .all(|name| super::valid_name(name))
        .then_some(names)
}

pub(super) fn numeric_macos_version(version: &str) -> bool {
    !version.is_empty()
        && version.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u64>().is_ok()
        })
}

/// The supported Generic destination mapping without a concrete prefix. Shared
/// by dual-target admission and the actual cask destination producer.
pub(super) fn generic_target_relative(
    target: &str,
) -> Option<std::borrow::Cow<'_, std::path::Path>> {
    // $APPDIR is the cask appdir: brew's /Applications, abrew's
    // prefix Applications (where app artifacts already land).
    if let Some(rest) = target.strip_prefix("$APPDIR/") {
        let rest_path = std::path::Path::new(rest);
        if rest.is_empty()
            || rest_path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        return Some(std::borrow::Cow::Owned(
            std::path::PathBuf::from("Applications").join(rest_path),
        ));
    }
    let relative = target
        .strip_prefix("$HOMEBREW_PREFIX/")
        .or_else(|| target.strip_prefix("~/"))
        .or_else(|| target.strip_prefix("/$HOME/"))
        .or_else(|| target.strip_prefix("$HOME/"))?;
    let path = std::path::Path::new(relative);
    if relative.is_empty()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || (!target.starts_with("$HOMEBREW_PREFIX/") && !relative.starts_with("Library/"))
    {
        return None;
    }
    Some(std::borrow::Cow::Borrowed(path))
}

/// One formula `variations` entry: the fields that can change what a
/// pour needs. Build/test/head-only fields are pour-irrelevant and
/// dropped at parse; anything else marks the entry unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormulaVariation {
    pub dependencies: Option<Vec<String>>,
    pub requirements: Option<Vec<Requirement>>,
    pub requirements_unsupported: bool,
    pub conflicts: Option<Vec<String>>,
    pub unsupported: bool,
}

fn parse_formula_variations(v: Option<&Value>) -> Vec<(String, FormulaVariation)> {
    let mut out = Vec::new();
    let Some(Value::Object(map)) = v else {
        if !empty_metadata(v) {
            out.push((
                String::new(),
                FormulaVariation {
                    unsupported: true,
                    ..FormulaVariation::default()
                },
            ));
        }
        return out;
    };
    for (key, entry) in map {
        let mut variation = FormulaVariation::default();
        let Value::Object(fields) = entry else {
            variation.unsupported = true;
            out.push((key.clone(), variation));
            continue;
        };
        for (field, value) in fields {
            match field.as_str() {
                "dependencies" => match plain_names(Some(value)) {
                    Some(names) => variation.dependencies = Some(names),
                    None => variation.unsupported = true,
                },
                "conflicts_with" => match plain_names(Some(value)) {
                    Some(names) => variation.conflicts = Some(names),
                    None => variation.unsupported = true,
                },
                "requirements" => match parse_requirements(Some(value)) {
                    Ok(reqs) => variation.requirements = Some(reqs),
                    Err(()) => variation.requirements_unsupported = true,
                },
                // Pour-irrelevant: build/test/head inputs and prose.
                "build_dependencies"
                | "head_dependencies"
                | "test_dependencies"
                | "caveats"
                | "patches"
                | "no_linkage_dependencies"
                | "conflicts_with_reasons"
                | "link_overwrites" => {}
                _ => variation.unsupported = true,
            }
        }
        out.push((key.clone(), variation));
    }
    out
}

/// A formula requirement in a form abrew can evaluate from data alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// `depends_on :macos` with an optional numeric floor.
    MacosMin(Option<String>),
    /// `depends_on maximum_macos: X`.
    MacosMax(String),
    /// `depends_on arch: :arm64` / `:x86_64`.
    Arch(String),
    /// `depends_on xcode:` with an optional numeric floor, probed
    /// with `xcodebuild -version` at preflight like brew probes it.
    XcodeMin(Option<String>),
}

/// Parse the API `requirements` list. Build/test contexts and
/// non-stable specs never gate a bottle pour and are dropped. `Err`
/// (unit) marks any richer shape unsupported.
fn parse_requirements(v: Option<&Value>) -> Result<Vec<Requirement>, ()> {
    let mut out = Vec::new();
    let Some(items) = v.and_then(Value::as_array) else {
        return if empty_metadata(v) { Ok(out) } else { Err(()) };
    };
    for item in items {
        let Value::Object(_) = item else {
            return Err(());
        };
        let specs = plain_strings(item.get("specs")).ok_or(())?;
        if !specs.is_empty() && !specs.iter().any(|s| s == "stable") {
            continue;
        }
        let contexts = plain_strings(item.get("contexts")).ok_or(())?;
        // Build and test contexts never gate a bottle pour: abrew
        // neither builds from source nor runs `brew test`.
        if !contexts.is_empty()
            && contexts
                .iter()
                .all(|c| matches!(c.as_str(), "build" | "test"))
        {
            continue;
        }
        if !contexts.is_empty() {
            return Err(());
        }
        let version = match item.get("version") {
            None | Some(Value::Null) => None,
            Some(Value::String(version)) => Some(version.as_str()),
            _ => return Err(()),
        };
        match item.get("name").and_then(Value::as_str) {
            Some("macos") if version.is_none_or(numeric_macos_version) => {
                out.push(Requirement::MacosMin(version.map(str::to_owned)));
            }
            Some("maximum_macos") if version.is_some_and(numeric_macos_version) => {
                out.push(Requirement::MacosMax(version.ok_or(())?.to_string()));
            }
            Some("arch") if matches!(version, Some("arm64" | "x86_64")) => {
                out.push(Requirement::Arch(version.unwrap_or_default().to_string()));
            }
            Some("xcode") if version.is_none_or(numeric_macos_version) => {
                out.push(Requirement::XcodeMin(version.map(str::to_owned)));
            }
            _ => return Err(()),
        }
    }
    Ok(out)
}

impl Formula {
    /// Parse one formulae.brew.sh formula object.
    pub fn from_api(v: &Value) -> Result<Formula, String> {
        let name = s(v.get("name"));
        if name.is_empty() {
            return Err("abrew: formula JSON has no name".to_string());
        }
        let versions = v.get("versions");
        let stable = v.pointer("/urls/stable");
        let mut bottle_files = Vec::new();
        let pour = v.get("pour_bottle_only_if").and_then(Value::as_str);
        let pour_requires_clt = pour == Some("clt_installed");
        let requirements = parse_requirements(v.get("requirements"));
        let parsed_conflicts = plain_names(v.get("conflicts_with"));
        let conflicts_plain = parsed_conflicts.is_some();
        let conflicts = parsed_conflicts.unwrap_or_default();
        let pour_needs = if !matches!(
            v.get("post_install_defined"),
            None | Some(Value::Bool(false))
        ) {
            Some("a post_install recipe".to_string())
        } else if !(matches!(v.get("pour_bottle_only_if"), None | Some(Value::Null))
            || pour_requires_clt)
        {
            Some(match pour {
                Some(condition) => format!("pour_bottle_only_if :{condition}"),
                None => "a pour condition abrew cannot read".to_string(),
            })
        } else if !empty_metadata(v.get("post_install_steps")) {
            Some("post-install steps".to_string())
        } else if requirements.is_err() {
            Some("requirements abrew cannot evaluate".to_string())
        } else if !conflicts_plain {
            Some("conflicts abrew cannot evaluate".to_string())
        } else {
            None
        };
        if let Some(files) = v.pointer("/bottle/stable/files")
            && let Value::Object(map) = files
        {
            for (tag, f) in map {
                bottle_files.push(Bottle {
                    tag: tag.clone(),
                    url: s(f.get("url")),
                    sha256: s(f.get("sha256")),
                    cellar: s(f.get("cellar")),
                });
            }
        }
        bottle_files.sort();
        Ok(Formula {
            name: name.clone(),
            full_name: {
                let f = s(v.get("full_name"));
                if f.is_empty() { name } else { f }
            },
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            license: s(v.get("license")),
            version: s(versions.and_then(|x| x.get("stable"))),
            revision: v.get("revision").and_then(Value::as_u64).unwrap_or(0),
            stable_url: s(stable.and_then(|x| x.get("url"))),
            stable_sha256: s(stable.and_then(|x| x.get("checksum"))),
            bottle_files,
            pour_needs,
            conflicts,
            requirements: requirements.unwrap_or_default(),
            pour_requires_clt,
            variations: parse_formula_variations(v.get("variations")),
            dependencies: str_list(v.get("dependencies")),
            build_dependencies: str_list(v.get("build_dependencies")),
            keg_only: v.get("keg_only").and_then(Value::as_bool).unwrap_or(false),
            deprecate: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disable: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            caveats: s(v.get("caveats")),
            aliases: str_list(v.get("aliases")),
            oldnames: str_list(v.get("oldnames")),
        })
    }

    /// The bottle file for this machine, best tag first (design §3):
    /// the first compatible OS tag supplied by the caller, then `all`.
    #[must_use]
    pub fn bottle_for(&self, tags: &[String]) -> Option<&Bottle> {
        tags.iter()
            .map(String::as_str)
            .chain(["all"])
            .find_map(|want| self.bottle_files.iter().find(|b| b.tag == want))
    }

    /// The bottle for `tags` that pours natively, or why the pour needs
    /// Homebrew's recipe: no bottle for this machine, what the formula as a
    /// whole needs ([`Formula::pour_needs`]), or a bottle that relocates.
    pub fn native_bottle(&self, tags: &[String]) -> Result<&Bottle, String> {
        let bottle = self
            .bottle_for(tags)
            .ok_or_else(|| "no bottle for this machine".to_string())?;
        if let Some(needs) = &self.pour_needs {
            return Err(needs.clone());
        }
        if bottle.cellar != ":any_skip_relocation" {
            return Err(format!("its bottle relocates: cellar {}", bottle.cellar));
        }
        Ok(bottle)
    }

    /// Apply the variation for `machine`, mirroring cask selection:
    /// the API's top-level fields are the default arm64-mac system, so
    /// only an exact-system deviation applies (`arm64_<release>`, Intel
    /// `x86_64_<release>`/bare `<release>`, `<arch>_linux`). An
    /// applicable-but-unsupported entry makes every pour need Homebrew
    /// (delegate), as does an unnameable system with variations present.
    pub(super) fn resolve_variations(&mut self, machine: &Machine) -> bool {
        if self.variations.is_empty() {
            return true;
        }
        if self
            .variations
            .iter()
            .any(|(key, entry)| key.is_empty() && entry.unsupported)
        {
            return self.resolve_variations_for_keys(&[]);
        }
        let Some(keys) = machine.variation_keys() else {
            self.pour_needs
                .get_or_insert_with(|| "variations for a system abrew cannot name".to_string());
            self.variations.clear();
            return false;
        };
        self.resolve_variations_for_keys(&keys)
    }

    /// Apply already-classified system keys. False preserves unknown variation work
    /// as a refusal for checked dependency readers, not an empty dependency set.
    fn resolve_variations_for_keys(&mut self, keys: &[String]) -> bool {
        if self
            .variations
            .iter()
            .any(|(key, entry)| key.is_empty() && entry.unsupported)
        {
            self.pour_needs
                .get_or_insert_with(|| "variations abrew cannot read".to_string());
            self.variations.clear();
            return false;
        }
        let found = keys.iter().find_map(|key| {
            self.variations
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, variation)| variation.clone())
        });
        self.variations.clear();
        let Some(variation) = found else {
            return true;
        };
        if variation.unsupported || variation.requirements_unsupported {
            self.pour_needs
                .get_or_insert_with(|| "a variation abrew cannot resolve".to_string());
            return false;
        }
        if let Some(deps) = variation.dependencies {
            self.dependencies = deps;
        }
        if let Some(reqs) = variation.requirements {
            self.requirements = reqs;
        }
        if let Some(conflicts) = variation.conflicts {
            self.conflicts = conflicts;
        }
        true
    }

    /// `name`'s short form without a tap prefix.
    #[must_use]
    pub fn short_name(full: &str) -> &str {
        full.rsplit('/').next().unwrap_or(full)
    }
}

/// A cask as abrew needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cask {
    pub token: String,
    pub deprecated: bool,
    pub disabled: bool,
    pub name: String,
    pub desc: String,
    pub homepage: String,
    pub version: String,
    pub url: String,
    /// `None` when the cask says `no_check`.
    pub sha256: Option<String>,
    pub artifacts: Vec<Artifact>,
    pub auto_updates: bool,
    /// The first top-level constraint the native resolver cannot evaluate
    /// (`container`, `depends_on`, `variations`, `conflicts_with`).
    pub unsupported_constraints: Option<&'static str>,
    /// The `depends_on macos: ">= X"` floor, when the API states one in
    /// plain numeric form. Evaluated against the running OS at install.
    pub macos_min: Option<String>,
    /// The `depends_on macos: "== X"` exact-release bound.
    pub macos_exact: Option<String>,
    /// `conflicts_with` cask/formula names; evaluated against abrew's
    /// own installed census at install.
    pub conflicts: Vec<String>,
    /// The `depends_on arch:` requirement ("arm64"/"x86_64"), when the
    /// API states exactly one plain architecture.
    pub arch: Option<String>,
    /// `depends_on formula:` names, installed before this cask.
    pub depends_formula: Vec<String>,
    /// `depends_on cask:` tokens, installed before this cask.
    pub depends_cask: Vec<String>,
    /// Per-system overrides (`variations`), selected at install time by
    /// macOS release and architecture. Empty for most casks.
    pub variations: Vec<(String, Variation)>,
    /// `container type: :naked` — the download is the payload file itself.
    pub naked: bool,
    /// `container nested: "<path>"` — the real payload archive sits
    /// at this path inside the downloaded outer archive.
    pub nested: Option<String>,
}

/// One admitted `variations` entry: plain field overrides only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variation {
    /// This entry uses fields/shapes abrew cannot evaluate; it blocks
    /// only when it is the entry selected for this machine.
    pub unsupported: bool,
    pub url: Option<String>,
    /// Outer None: field absent. Inner None: the variation says `no_check`.
    pub sha256: Option<Option<String>>,
    pub version: Option<String>,
    pub artifacts: Option<Vec<Artifact>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    /// Source path inside the payload and the installed name the cask's
    /// `target:` rename asks for, when it names a plain file name.
    App(String, Option<String>),
    Binary(String, Option<String>),
    Font(String, Option<String>),
    Pkg(String),
    Manpage(String, Option<String>),
    Suite(String, Option<String>),
    KeyboardLayout(String, Option<String>),
    ScreenSaver(String, Option<String>),
    PrefPane(String, Option<String>),
    Service(String, Option<String>),
    /// Generic `artifact` with its full stated target (mapped into the
    /// prefix at install; unmappable targets delegate).
    Generic(String, Option<String>),
    /// `command_wrapper`: wrapper name, executable payload source,
    /// fixed leading args, installed-name rename. The wrapper script
    /// itself is generated inside the payload at install.
    Wrapper(String, String, Vec<String>, Option<String>),
    /// A plugin/completion artifact linked into a fixed prefix-relative
    /// directory (`Library/QuickLook`, `share/zsh/site-functions`, …):
    /// source, rename, directory.
    Library(String, Option<String>, String),
    Other(String),
}

/// One cask `depends_on` in the plain data forms abrew evaluates.
#[derive(Debug, Default)]
struct Depends {
    floor: Option<String>,
    exact: Option<String>,
    arch: Option<String>,
    formula: Vec<String>,
    cask: Vec<String>,
}

/// The admitted `depends_on` shapes: an optional `macos` floor
/// (`{"macos": {">=": ["X"]}}` or an empty macos object), an optional
/// single plain `arch` requirement, and plain `formula`/`cask`
/// dependency name lists (installed first, like brew). None for
/// every richer shape.
fn parse_depends(v: Option<&Value>) -> Option<Depends> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut out = Depends::default();
    for (key, value) in map {
        match key.as_str() {
            "macos" => {
                let Value::Object(macos) = value else {
                    return None;
                };
                if macos.is_empty() {
                    continue;
                }
                if macos.len() != 1 {
                    return None;
                }
                let (op, bounds) = match (macos.get(">="), macos.get("==")) {
                    (Some(Value::Array(bounds)), None) => (">=", bounds),
                    (None, Some(Value::Array(bounds))) => ("==", bounds),
                    _ => return None,
                };
                let [Value::String(bound)] = bounds.as_slice() else {
                    return None;
                };
                if !numeric_macos_version(bound) {
                    return None;
                }
                if op == ">=" {
                    out.floor = Some(bound.clone());
                } else {
                    out.exact = Some(bound.clone());
                }
            }
            "arch" => {
                let Value::Array(items) = value else {
                    return None;
                };
                let [Value::Object(spec)] = items.as_slice() else {
                    return None;
                };
                if spec.len() != 2 {
                    return None;
                }
                let ty = spec.get("type").and_then(Value::as_str)?;
                let bits = spec.get("bits").and_then(Value::as_u64)?;
                out.arch = Some(
                    match (ty, bits) {
                        ("arm", 64) => "arm64",
                        ("intel", 64) => "x86_64",
                        _ => return None,
                    }
                    .to_string(),
                );
            }
            "formula" | "cask" => {
                let Value::Array(items) = value else {
                    return None;
                };
                let names = if key == "formula" {
                    &mut out.formula
                } else {
                    &mut out.cask
                };
                for item in items {
                    let Value::String(name) = item else {
                        return None;
                    };
                    if !super::valid_component(name) {
                        return None;
                    }
                    names.push(name.clone());
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The admitted `conflicts_with` shape: `{"cask": [...], "formula": [...]}`
/// with plain name strings (either key optional).
fn parse_conflicts(v: Option<&Value>) -> Option<Vec<String>> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut names = Vec::new();
    for (kind, vals) in map {
        if !matches!(kind.as_str(), "cask" | "formula") {
            return None;
        }
        let Value::Array(items) = vals else {
            return None;
        };
        for item in items {
            let Value::String(name) = item else {
                return None;
            };
            if !super::valid_name(name) {
                return None;
            }
            names.push(name.clone());
        }
    }
    Some(names)
}

/// Parse one API `artifacts` array (shared by casks and their variations).
fn parse_artifacts(v: Option<&Value>) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    if let Some(arr) = v.and_then(Value::as_array) {
        for a in arr {
            if !matches!(a, Value::Object(_))
                || a.get("target")
                    .is_some_and(|value| !matches!(value, Value::String(_)))
            {
                artifacts.push(Artifact::Other(
                    "unsupported artifact row or target".to_string(),
                ));
                continue;
            }
            // `target` is brew's computed destination for the sibling
            // artifact in the same object. Rename-only artifacts use its
            // file name; Generic artifacts compare their full mapping below.
            // `uninstall`/`zap` entries
            // describe removing a real-brew install and are not
            // artifacts to place, so they never block a native install.
            let raw_target = a.get("target").and_then(Value::as_str).map(str::to_owned);
            let dest_name = a.get("target").and_then(Value::as_str).map(|t| {
                std::path::Path::new(t)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            });
            if let Value::Object(map) = a {
                for (kind, vals) in map {
                    if matches!(kind.as_str(), "target" | "uninstall" | "zap") {
                        continue;
                    }
                    if kind == "command_wrapper" {
                        // ["name", {"executable": "…", "args": […]}]: the
                        // wrapper script is generated at install; arbitrary
                        // `content` wrappers delegate.
                        let parsed = (|| {
                            let items = vals.as_array()?;
                            let [Value::String(name), Value::Object(opts)] = items.as_slice()
                            else {
                                return None;
                            };
                            if !opts
                                .keys()
                                .all(|k| matches!(k.as_str(), "executable" | "args"))
                            {
                                return None;
                            }
                            // Args may reference the install tree only
                            // through the two rewritable markers.
                            if let Some(Value::Array(items)) = opts.get("args") {
                                for item in items {
                                    let arg = item.as_str()?;
                                    let scrubbed = arg
                                        .replace("$HOMEBREW_PREFIX/", "")
                                        .replace("$APPDIR/", "");
                                    if scrubbed.contains('$') {
                                        return None;
                                    }
                                }
                            }
                            let exe = opts.get("executable").and_then(Value::as_str)?;
                            let exe = exe.strip_prefix("$APPDIR/").unwrap_or(exe);
                            let args = match opts.get("args") {
                                None => Vec::new(),
                                Some(Value::Array(items)) => {
                                    let mut out = Vec::new();
                                    for item in items {
                                        out.push(item.as_str()?.to_string());
                                    }
                                    out
                                }
                                Some(_) => return None,
                            };
                            let dest = match &dest_name {
                                Some(Some(name))
                                    if !name.is_empty()
                                        && !name.contains('/')
                                        && !name.contains('\\') =>
                                {
                                    Some(name.clone())
                                }
                                Some(_) => return None,
                                None => None,
                            };
                            Some(Artifact::Wrapper(name.clone(), exe.to_string(), args, dest))
                        })();
                        artifacts.push(parsed.unwrap_or_else(|| {
                            Artifact::Other(
                                "command_wrapper: unsupported artifact options".to_string(),
                            )
                        }));
                        continue;
                    }
                    let items = vals.as_array();
                    let simple = items
                        .and_then(|items| items.first())
                        .and_then(Value::as_str);
                    // The two-item form carries the rename in-line:
                    // ["src", {"target": "name"}]. Only that one
                    // option is data abrew can act on.
                    let inline_target = items
                        .and_then(|items| items.get(1))
                        .and_then(|options| options.get("target"))
                        .and_then(Value::as_str);
                    if kind == "artifact"
                        && raw_target.as_deref().zip(inline_target).is_some_and(|(sibling, inline)| {
                            // Identical spellings cannot conflict;
                            // whether the shared target maps into the
                            // prefix is map_generic_target's question,
                            // answered (and messaged) there.
                            sibling != inline
                                && !matches!((generic_target_relative(sibling), generic_target_relative(inline)),
                                    (Some(a), Some(b)) if a == b)
                        })
                    {
                        artifacts.push(Artifact::Other("artifact: conflicting full targets".to_string()));
                        continue;
                    }
                    let raw_target = raw_target.clone().or_else(|| {
                        items
                            .and_then(|items| items.get(1))
                            .and_then(|opts| opts.get("target"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
                    let inline_dest = match items.map(Vec::as_slice) {
                        Some([_]) => None,
                        Some([_, Value::Object(opts)]) if opts.len() == 1 => {
                            Some(opts.get("target").and_then(Value::as_str).and_then(|t| {
                                std::path::Path::new(t)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                            }))
                        }
                        _ => Some(None),
                    };
                    let (Some(first), inline_dest) = (simple, inline_dest) else {
                        artifacts.push(Artifact::Other(format!(
                            "{kind}: unsupported artifact options"
                        )));
                        continue;
                    };
                    // $APPDIR names the staged .app of the same cask;
                    // in the payload it is simply payload-relative.
                    let first = first.strip_prefix("$APPDIR/").unwrap_or(first).to_owned();
                    if matches!(inline_dest, Some(None))
                        || matches!((&dest_name, &inline_dest), (Some(Some(a)), Some(Some(b))) if a != b)
                    {
                        artifacts.push(Artifact::Other(format!(
                            "{kind}: unsupported artifact options"
                        )));
                        continue;
                    }
                    let dest = match dest_name.clone().or(inline_dest) {
                        Some(Some(name))
                            if !name.is_empty() && !name.contains('/') && !name.contains('\\') =>
                        {
                            Some(name)
                        }
                        Some(_) => {
                            artifacts
                                .push(Artifact::Other(format!("{kind}: unusable target rename")));
                            continue;
                        }
                        None => None,
                    };
                    artifacts.push(match kind.as_str() {
                        "app" => Artifact::App(first, dest),
                        "binary" => Artifact::Binary(first, dest),
                        "font" => Artifact::Font(first, dest),
                        "pkg" => Artifact::Pkg(first),
                        "manpage" => Artifact::Manpage(first, dest),
                        "suite" => Artifact::Suite(first, dest),
                        "keyboard_layout" => Artifact::KeyboardLayout(first, dest),
                        "screen_saver" => Artifact::ScreenSaver(first, dest),
                        "prefpane" => Artifact::PrefPane(first, dest),
                        "service" => Artifact::Service(first, dest),
                        "artifact" => Artifact::Generic(first, raw_target.clone()),
                        "qlplugin" => Artifact::Library(first, dest, "Library/QuickLook".into()),
                        "colorpicker" => {
                            Artifact::Library(first, dest, "Library/ColorPickers".into())
                        }
                        "dictionary" => {
                            Artifact::Library(first, dest, "Library/Dictionaries".into())
                        }
                        "input_method" => {
                            Artifact::Library(first, dest, "Library/Input Methods".into())
                        }
                        "internet_plugin" => {
                            Artifact::Library(first, dest, "Library/Internet Plug-Ins".into())
                        }
                        "mdimporter" => Artifact::Library(first, dest, "Library/Spotlight".into()),
                        "audio_unit_plugin" => Artifact::Library(
                            first,
                            dest,
                            "Library/Audio/Plug-Ins/Components".into(),
                        ),
                        "vst_plugin" => {
                            Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST".into())
                        }
                        "vst3_plugin" => {
                            Artifact::Library(first, dest, "Library/Audio/Plug-Ins/VST3".into())
                        }
                        "bash_completion" => Artifact::Library(
                            first,
                            dest,
                            "share/bash-completion/completions".into(),
                        ),
                        "zsh_completion" => {
                            Artifact::Library(first, dest, "share/zsh/site-functions".into())
                        }
                        "fish_completion" => {
                            Artifact::Library(first, dest, "share/fish/vendor_completions.d".into())
                        }
                        other => Artifact::Other(format!("{other}:{first}")),
                    });
                }
            }
        }
    }
    artifacts
}

/// The admitted `variations` shape: an object whose every entry holds
/// only plain overrides (url/sha256/version/artifacts; `skip_livecheck`
/// is update metadata and ignored). Anything richer delegates.
fn parse_variations(v: Option<&Value>) -> Option<Vec<(String, Variation)>> {
    let Value::Object(map) = v? else {
        return None;
    };
    let mut out = Vec::new();
    for (key, entry) in map {
        // Per-entry admission: a rich entry for another system must not
        // poison this machine's pour; it delegates only if selected.
        let mut variation = Variation::default();
        let Value::Object(fields) = entry else {
            variation.unsupported = true;
            out.push((key.clone(), variation));
            continue;
        };
        for (field, value) in fields {
            match field.as_str() {
                "url" => match value.as_str() {
                    Some(u) => variation.url = Some(u.to_string()),
                    None => variation.unsupported = true,
                },
                "version" => match value.as_str() {
                    Some(v) => variation.version = Some(v.to_string()),
                    None => variation.unsupported = true,
                },
                "sha256" => match value.as_str() {
                    Some("no_check") => variation.sha256 = Some(None),
                    Some(raw) if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) => {
                        variation.sha256 = Some(Some(raw.to_string()));
                    }
                    _ => variation.unsupported = true,
                },
                "artifacts" => variation.artifacts = Some(parse_artifacts(Some(value))),
                "skip_livecheck" => {}
                _ => variation.unsupported = true,
            }
        }
        out.push((key.clone(), variation));
    }
    Some(out)
}

impl Cask {
    /// The variation Homebrew would pick for `machine`. The API's
    /// top-level fields are generated for the default system (arm64,
    /// newest macOS), so they already ARE that system's variation:
    /// bare release keys hold the Intel deviations and `arm64_<release>`
    /// keys the older-OS arm deviations. The API materializes an entry
    /// for every differing system, so selection is exact-OS. None means the
    /// top-level fields stand.
    #[must_use]
    pub fn variation_for(&self, machine: &Machine) -> Option<&Variation> {
        machine.variation_keys()?.iter().find_map(|key| {
            self.variations
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, variation)| variation)
        })
    }

    /// Parse one formulae.brew.sh cask object.
    pub fn from_api(v: &Value) -> Result<Cask, String> {
        let token = s(v.get("token"));
        if token.is_empty() {
            return Err("abrew: cask JSON has no token".to_string());
        }
        let sha_raw = s(v.get("sha256"));
        let mut artifacts = parse_artifacts(v.get("artifacts"));
        // Only the complete one-field naked container and a plain
        // `container nested:` naming a supported inner archive are
        // data. A richer container must remain unsupported through
        // artifact overrides.
        let naked = matches!(v.get("container"), Some(Value::Object(fields))
            if fields.len() == 1 && fields.get("type").and_then(Value::as_str) == Some("naked"));
        let nested = match v.get("container") {
            Some(Value::Object(fields)) if fields.len() == 1 => fields
                .get("nested")
                .and_then(Value::as_str)
                .filter(|p| {
                    !p.is_empty()
                        && std::path::Path::new(p)
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                        && super::fetch::archive_supported(p)
                })
                .map(str::to_owned),
            _ => None,
        };
        let mut unsupported_constraints = None;
        if !empty_metadata(v.get("container")) && !naked && nested.is_none() {
            unsupported_constraints = unsupported_constraints.or(Some("container"));
            artifacts.push(Artifact::Other("unsupported container".to_string()));
        }
        // `depends_on`/`conflicts_with` are admitted only in the plain
        // data forms abrew can evaluate itself; anything richer stays a
        // delegation (Artifact::Other), exactly like before.
        let mut macos_min = None;
        let mut macos_exact = None;
        let mut arch = None;
        let mut depends_formula = Vec::new();
        let mut depends_cask = Vec::new();
        if !empty_metadata(v.get("depends_on")) {
            match parse_depends(v.get("depends_on")) {
                Some(deps) => {
                    macos_min = deps.floor;
                    macos_exact = deps.exact;
                    arch = deps.arch;
                    depends_formula = deps.formula;
                    depends_cask = deps.cask;
                }
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("depends_on"));
                    artifacts.push(Artifact::Other("unsupported depends_on".to_string()));
                }
            }
        }
        let mut variations = Vec::new();
        if !empty_metadata(v.get("variations")) {
            match parse_variations(v.get("variations")) {
                Some(parsed) => variations = parsed,
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("variations"));
                    artifacts.push(Artifact::Other("unsupported variations".to_string()));
                }
            }
        }
        let mut conflicts = Vec::new();
        if !empty_metadata(v.get("conflicts_with")) {
            match parse_conflicts(v.get("conflicts_with")) {
                Some(names) => conflicts = names,
                None => {
                    unsupported_constraints = unsupported_constraints.or(Some("conflicts_with"));
                    artifacts.push(Artifact::Other("unsupported conflicts_with".to_string()));
                }
            }
        }
        let sha256 = if sha_raw == "no_check" {
            None
        } else if sha_raw.len() == 64 && sha_raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(sha_raw)
        } else {
            // Absent or malformed is not `no_check`: never install
            // unverified bytes on a truncated API field.
            return Err(format!(
                "abrew: cask {token} has no usable sha256 (missing or malformed)"
            ));
        };
        let names = str_list(v.get("name"));
        Ok(Cask {
            deprecated: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            token,
            name: names.into_iter().next().unwrap_or_default(),
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            version: s(v.get("version")),
            url: s(v.get("url")),
            sha256,
            artifacts,
            auto_updates: v
                .get("auto_updates")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unsupported_constraints,
            macos_min,
            macos_exact,
            conflicts,
            arch,
            depends_formula,
            depends_cask,
            variations,
            naked,
            nested,
        })
    }
}

/// Homebrew-shaped version comparison (design §6): dotted tokens,
/// numeric tokens compare as numbers and outrank alpha tokens at the
/// same slot except pre-release words (alpha < beta < rc < release),
/// `_N`/`-N` revisions compare last. Exotic strings fall back to a
/// tokenwise compare that never claims equality spuriously.
#[must_use]
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a_main, a_rev) = split_revision(a);
    let (b_main, b_rev) = split_revision(b);
    let at = tokens(&a_main);
    let bt = tokens(&b_main);
    for i in 0..at.len().max(bt.len()) {
        let x = at.get(i).map_or("", String::as_str);
        let y = bt.get(i).map_or("", String::as_str);
        let ord = token_cmp(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a_rev.cmp(&b_rev)
}

fn split_revision(v: &str) -> (String, u64) {
    for sep in ['_', '-'] {
        if let Some((head, tail)) = v.rsplit_once(sep)
            && let Ok(n) = tail.parse::<u64>()
            && tail.chars().all(|c| c.is_ascii_digit())
            && !head.is_empty()
        {
            return (head.to_string(), n);
        }
    }
    (v.to_string(), 0)
}

fn tokens(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits: Option<bool> = None;
    for c in v.chars() {
        if matches!(c, '.' | '-' | '_' | '+') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            digits = None;
            continue;
        }
        let d = c.is_ascii_digit();
        if digits.is_some_and(|was| was != d) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        digits = Some(d);
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn prerelease_rank(t: &str) -> Option<u8> {
    match t.to_ascii_lowercase().as_str() {
        "alpha" | "a" => Some(0),
        "beta" | "b" => Some(1),
        "pre" | "preview" | "rc" => Some(2),
        _ => None,
    }
}

fn token_cmp(x: &str, y: &str) -> Ordering {
    if x == y {
        return Ordering::Equal;
    }
    let xn = x.parse::<u128>().ok();
    let yn = y.parse::<u128>().ok();
    match (xn, yn) {
        (Some(a), Some(b)) => a.cmp(&b),
        // An empty slot is a release: it outranks a pre-release word
        // (1.0 > 1.0rc1) and equals a zero (1.0 == 1.0.0).
        (None, Some(0)) | (Some(0), None) if x.is_empty() || y.is_empty() => Ordering::Equal,
        (Some(_), None) => {
            if y.is_empty() || prerelease_rank(y).is_some() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (None, Some(_)) => {
            if x.is_empty() || prerelease_rank(x).is_some() {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (None, None) => match (prerelease_rank(x), prerelease_rank(y)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => x.cmp(y),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottles_never_require_a_newer_os_or_another_architecture() {
        let bottle = |tag: &str, url: &str| Bottle {
            tag: tag.into(),
            url: url.into(),
            ..Bottle::default()
        };
        let mut formula = Formula {
            bottle_files: vec![
                bottle("arm64_tahoe", "newer"),
                bottle("arm64_sonoma", "older"),
                bottle("all", "universal"),
            ],
            ..Formula::default()
        };
        let tags = |macos: &str| Machine::mac("arm64", macos, None, false).bottle_tags();
        assert_eq!(formula.bottle_for(&tags("14.6")).unwrap().url, "older");
        assert_eq!(formula.bottle_for(&tags("26.0")).unwrap().url, "newer");
        assert_eq!(
            formula.bottle_for(&tags("unknown")).unwrap().url,
            "universal"
        );
        formula.bottle_files.pop();
        assert!(formula.bottle_for(&tags("unknown")).is_none());
    }

    #[test]
    fn version_ordering_common_shapes() {
        assert_eq!(version_cmp("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(version_cmp("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(version_cmp("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(version_cmp("2.0rc1", "2.0"), Ordering::Less);
        assert_eq!(version_cmp("2.0beta2", "2.0rc1"), Ordering::Less);
        assert_eq!(version_cmp("1.2.3_1", "1.2.3"), Ordering::Greater);
        assert_eq!(version_cmp("2024.01", "2023.12"), Ordering::Greater);
    }

    #[test]
    fn native_bottles_require_complete_supported_api_metadata() {
        let mut value: Value = aterm_json::from_str(r#"{"name":"demo","bottle":{"stable":{"files":{"all":{"url":"https://example.invalid/demo.tar.gz","sha256":"hash","cellar":":any_skip_relocation"}}}}}"#).unwrap();
        let native = |f: &Formula| f.native_bottle(&[]).map(|bottle| bottle.tag.clone());
        assert_eq!(
            native(&Formula::from_api(&value).unwrap()).as_deref(),
            Ok("all")
        );
        for (unsupported, needs) in [
            (
                r#"{"name":"demo","post_install_defined":true,"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
                "a post_install recipe",
            ),
            (
                r#"{"name":"demo","requirements":[{"name":"linux"}],"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
                "requirements abrew cannot evaluate",
            ),
            (
                r#"{"name":"demo","pour_bottle_only_if":"condition","bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
                "pour_bottle_only_if :condition",
            ),
            (
                r#"{"name":"demo","post_install_steps":[{"step":"x"}],"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
                "post-install steps",
            ),
            (
                r#"{"name":"demo","bottle":{"stable":{"files":{"arm64_tahoe":{"cellar":":any_skip_relocation"}}}}}"#,
                "no bottle for this machine",
            ),
        ] {
            value = aterm_json::from_str(unsupported).unwrap();
            assert_eq!(
                native(&Formula::from_api(&value).unwrap()),
                Err(needs.to_string())
            );
        }
        // Data-evaluable requirements and conflicts keep the native tag;
        // they are enforced against the machine/census at preflight.
        value = aterm_json::from_str(
            r#"{"name":"demo","conflicts_with":["other"],"requirements":[{"name":"macos","version":"13","contexts":[],"specs":["stable","head"]},{"name":"xcode","contexts":["build","test"]},{"name":"xcode","version":"14.0","contexts":[],"specs":["stable"]}],"pour_bottle_only_if":"clt_installed","bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
        )
        .unwrap();
        let f = Formula::from_api(&value).unwrap();
        assert_eq!(native(&f).as_deref(), Ok("all"));
        assert_eq!(f.conflicts, ["other"]);
        // The build/test-context xcode row drops; the runtime xcode
        // floor stays as data for the preflight probe.
        assert_eq!(
            f.requirements,
            [
                Requirement::MacosMin(Some("13".into())),
                Requirement::XcodeMin(Some("14.0".into()))
            ]
        );
        assert!(f.pour_requires_clt);
        // Variations parse as data and resolve exact-system: a Linux-only
        // deviation never touches a macOS pour, an arm64_<this OS> one does.
        value = aterm_json::from_str(
            r#"{"name":"demo","dependencies":["base"],"variations":{"arm64_linux":{"dependencies":["linuxonly"]},"x86_64_linux":{"dependencies":["linuxonly"]}},"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
        )
        .unwrap();
        let mut f = Formula::from_api(&value).unwrap();
        assert_eq!(native(&f).as_deref(), Ok("all"));
        let mut mac = f.clone();
        assert!(mac.resolve_variations_for_keys(&["arm64_tahoe".to_string()]));
        assert_eq!(mac.dependencies, ["base"]);
        assert_eq!(native(&mac).as_deref(), Ok("all"));
        assert!(f.resolve_variations_for_keys(&["arm64_linux".to_string()]));
        assert_eq!(f.dependencies, ["linuxonly"]);
        // A non-string condition/conflict is unknown metadata, not no condition.
        for field in [
            r#""pour_bottle_only_if":{"unknown":true}"#,
            r#""conflicts_with":{"formula":["other"]}"#,
            r#""requirements":[{"name":"macos","contexts":[42]}]"#,
        ] {
            let malformed: Value = aterm_json::from_str(&format!(
                r#"{{"name":"demo",{field},"bottle":{{"stable":{{"files":{{"all":{{"cellar":":any_skip_relocation"}}}}}}}}}}"#
            )).unwrap();
            assert!(native(&Formula::from_api(&malformed).unwrap()).is_err());
        }
        for variation in [r#"[42]"#, r#"{"arm64_tahoe":{"dependencies":[42]}}"#] {
            let malformed: Value = aterm_json::from_str(&format!(
                r#"{{"name":"demo","versions":{{"stable":"1"}},"revision":0,"dependencies":[],"variations":{variation},"bottle":{{"stable":{{"files":{{"all":{{"cellar":":any_skip_relocation"}}}}}}}}}}"#
            )).unwrap();
            let mut parsed = Formula::from_api(&malformed).unwrap();
            assert!(!parsed.resolve_variations_for_keys(&["arm64_tahoe".to_string()]));
            assert!(native(&parsed).is_err());
        }
        let malformed: Value = aterm_json::from_str(r#"{"name":"demo","versions":{"stable":"1"},"revision":0,"dependencies":[],"variations":[42]}"#).unwrap();
        assert!(
            super::super::api::dependency_formula(&malformed, "demo", Machine::this()).is_err()
        );
        let complete: Value = aterm_json::from_str(r#"{"name":"demo","versions":{"stable":"1"},"revision":0,"dependencies":[],"variations":{}}"#).unwrap();
        assert!(super::super::api::dependency_formula(&complete, "demo", Machine::this()).is_ok());
        for cellar in [":any", "/opt/homebrew/Cellar", ""] {
            value = aterm_json::from_str(&format!(r#"{{"name":"demo","bottle":{{"stable":{{"files":{{"all":{{"cellar":"{cellar}"}}}}}}}}}}"#)).unwrap();
            assert_eq!(
                native(&Formula::from_api(&value).unwrap()),
                Err(format!("its bottle relocates: cellar {cellar}"))
            );
        }
    }

    #[test]
    fn cask_options_and_constraints_survive_as_unsupported_work() {
        let simple: Value =
            aterm_json::from_str(r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifacts":[{"app":["Demo.app"]}]}"#).unwrap();
        assert_eq!(
            Cask::from_api(&simple).unwrap().artifacts,
            [Artifact::App("Demo.app".into(), None)]
        );
        for json in [
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","depends_on":{"macos":[">= :sonoma"]}}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","container":{"type":"dmg"}}"#,
        ] {
            let value: Value = aterm_json::from_str(json).unwrap();
            assert!(
                Cask::from_api(&value)
                    .unwrap()
                    .artifacts
                    .iter()
                    .any(|artifact| matches!(artifact, Artifact::Other(_)))
            );
        }
        // Only the literal `no_check` waives verification; a missing or
        // malformed sha256 refuses the document instead of installing
        // unverified bytes.
        for json in [
            r#"{"token":"demo","artifacts":[{"app":["Demo.app"]}]}"#,
            r#"{"token":"demo","sha256":"short","artifacts":[{"app":["Demo.app"]}]}"#,
        ] {
            let value: Value = aterm_json::from_str(json).unwrap();
            assert!(Cask::from_api(&value).is_err());
        }
        // Plain-data depends_on/conflicts_with parse as evaluated fields,
        // not unsupported work; richer shapes still refuse above.
        let constrained: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","depends_on":{{"macos":{{">=":["12"]}}}},"conflicts_with":{{"cask":["other"],"formula":["wget"]}},"artifacts":[]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        let cask = Cask::from_api(&constrained).unwrap();
        assert_eq!(cask.macos_min.as_deref(), Some("12"));
        assert_eq!(cask.conflicts, ["other", "wget"]);
        assert!(cask.artifacts.is_empty());
        let unbounded: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","depends_on":{{"macos":{{}}}},"artifacts":[]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        let cask = Cask::from_api(&unbounded).unwrap();
        assert_eq!(cask.macos_min, None);
        assert!(cask.artifacts.is_empty());
        let arched: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","depends_on":{{"arch":[{{"type":"arm","bits":64}}],"macos":{{">=":["14"]}}}},"artifacts":[{{"binary":["$APPDIR/Demo.app/Contents/MacOS/demo",{{"target":"demo"}}]}}]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        let cask = Cask::from_api(&arched).unwrap();
        assert_eq!(cask.arch.as_deref(), Some("arm64"));
        assert_eq!(cask.macos_min.as_deref(), Some("14"));
        assert_eq!(
            cask.artifacts,
            [Artifact::Binary(
                "Demo.app/Contents/MacOS/demo".into(),
                Some("demo".into())
            )]
        );
        // Variations select by release, walking down from the running OS.
        let varied: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","url":"https://x/base.zip","artifacts":[],"variations":{{"arm64_sonoma":{{"url":"https://x/sonoma.zip"}},"tahoe":{{"url":"https://x/tahoe.zip","sha256":"{}"}}}}}}"#,
            "a".repeat(64),
            "b".repeat(64)
        ))
        .unwrap();
        let cask = Cask::from_api(&varied).unwrap();
        assert_eq!(cask.variations.len(), 2);
        // Bare release keys are Intel deviations; arm64 keeps top-level
        // unless an arm64_<release> deviation applies.
        assert!(
            cask.variation_for(&Machine::mac("arm64", "26.0", None, false))
                .is_none()
        );
        assert_eq!(
            cask.variation_for(&Machine::mac("x86_64", "26.0", None, false))
                .and_then(|v| v.url.as_deref()),
            Some("https://x/tahoe.zip")
        );
        // Exact-OS only: a newer OS with no entry keeps the top-level.
        assert!(
            cask.variation_for(&Machine::mac("arm64", "27.0", None, false))
                .is_none()
        );
        assert_eq!(
            cask.variation_for(&Machine::mac("arm64", "14.0", None, false))
                .and_then(|v| v.url.as_deref()),
            Some("https://x/sonoma.zip")
        );
        assert!(
            cask.variation_for(&Machine::mac("x86_64", "13.0", None, false))
                .is_none()
        );
        // Real API artifacts carry a computed `target` (the rename) and
        // casks carry uninstall/zap stanzas; neither blocks parsing.
        let real: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","artifacts":[{{"uninstall":[{{"quit":"com.x"}}]}},{{"font":["ttf/F.ttf"],"target":"/$HOME/Library/Fonts/F.ttf"}},{{"zap":[{{"trash":["~/Library/Caches/x"]}}]}}]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        assert_eq!(
            Cask::from_api(&real).unwrap().artifacts,
            [Artifact::Font("ttf/F.ttf".into(), Some("F.ttf".into()))]
        );
        let kinds: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","artifacts":[{{"suite":["SuiteDir"]}},{{"keyboard_layout":["K.bundle"]}},{{"screen_saver":["S.saver"]}},{{"prefpane":["P.prefPane"]}},{{"service":["W.workflow"]}},{{"artifact":["tree",{{"target":"$HOMEBREW_PREFIX/share/demo/tree"}}]}},{{"binary":["$HOMEBREW_PREFIX/share/demo/tree/bin/tool"]}}]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        let wrapper: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"{}","artifacts":[{{"command_wrapper":["demo",{{"executable":"$APPDIR/Demo.app/Contents/MacOS/demo","args":["--fixed"]}}],"target":"$HOMEBREW_PREFIX/bin/demo"}}]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        assert_eq!(
            Cask::from_api(&wrapper).unwrap().artifacts,
            [Artifact::Wrapper(
                "demo".into(),
                "Demo.app/Contents/MacOS/demo".into(),
                vec!["--fixed".to_string()],
                Some("demo".into())
            )]
        );
        assert_eq!(
            Cask::from_api(&kinds).unwrap().artifacts,
            [
                Artifact::Suite("SuiteDir".into(), None),
                Artifact::KeyboardLayout("K.bundle".into(), None),
                Artifact::ScreenSaver("S.saver".into(), None),
                Artifact::PrefPane("P.prefPane".into(), None),
                Artifact::Service("W.workflow".into(), None),
                Artifact::Generic(
                    "tree".into(),
                    Some("$HOMEBREW_PREFIX/share/demo/tree".into())
                ),
                Artifact::Binary("$HOMEBREW_PREFIX/share/demo/tree/bin/tool".into(), None),
            ]
        );
        for artifact in [
            r#"42"#,
            r#"{"artifact":["src",{"target":"$HOMEBREW_PREFIX/lib/x"}],"target":"$HOMEBREW_PREFIX/bin/x"}"#,
            r#"{"binary":["tool",{"chmod":"0755"}]}"#,
            r#"{"binary":["tool",{"target":42}]}"#,
            r#"{"binary":["tool",{"chmod":"0755","extra":true}],"target":"renamed"}"#,
            r#"{"binary":["tool"],"target":42}"#,
            r#"{"binary":["tool",{"target":"first"}],"target":"second"}"#,
        ] {
            let unsupported: Value = aterm_json::from_str(&format!(
                r#"{{"token":"demo","sha256":"no_check","artifacts":[{artifact}]}}"#
            ))
            .unwrap();
            assert!(
                Cask::from_api(&unsupported)
                    .unwrap()
                    .artifacts
                    .iter()
                    .any(|artifact| matches!(artifact, Artifact::Other(_)))
            );
        }
        // Full Generic destinations compare after the same mapping used by
        // installation; rename-only artifacts keep their basename convention.
        let equivalent: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[{"artifact":["src",{"target":"$HOMEBREW_PREFIX/Library/Services/x"}],"target":"/$HOME/Library/Services/x"},{"binary":["tool",{"target":"x"}],"target":"$HOMEBREW_PREFIX/bin/x"}]}"#).unwrap();
        assert_eq!(
            Cask::from_api(&equivalent).unwrap().artifacts,
            [
                Artifact::Generic(
                    "src".to_string(),
                    Some("/$HOME/Library/Services/x".to_string())
                ),
                Artifact::Binary("tool".to_string(), Some("x".to_string())),
            ]
        );
        // R5 keeps unsupported variation work local to the selected system.
        let per_system: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[{"app":["Demo.app"]}],"variations":{"arm64_tahoe":{"unknown_recipe":true}}}"#).unwrap();
        let per_system = Cask::from_api(&per_system).unwrap();
        assert_eq!(per_system.unsupported_constraints, None);
        assert!(
            per_system
                .variation_for(&Machine::mac("arm64", "27.0", None, false))
                .is_none()
        );
        assert!(
            per_system
                .variation_for(&Machine::mac("arm64", "26.0", None, false))
                .unwrap()
                .unsupported
        );
        let naked: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","container":{"type":"naked"},"artifacts":[{"binary":["tool"]}]}"#).unwrap();
        let naked = Cask::from_api(&naked).unwrap();
        assert!(naked.naked && naked.unsupported_constraints.is_none());
        assert_eq!(
            naked.artifacts,
            [Artifact::Binary("tool".to_string(), None)]
        );
        for container in [
            r#"{"type":"naked","nested":"archive"}"#,
            r#"{"type":"naked","custom":true}"#,
        ] {
            let rich: Value = aterm_json::from_str(&format!(r#"{{"token":"demo","sha256":"no_check","container":{container},"artifacts":[{{"binary":["tool"]}}]}}"#)).unwrap();
            let rich = Cask::from_api(&rich).unwrap();
            assert!(!rich.naked && rich.unsupported_constraints == Some("container"));
            assert!(
                rich.artifacts
                    .iter()
                    .any(|artifact| matches!(artifact, Artifact::Other(_)))
            );
        }
        let waived: Value =
            aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[]}"#).unwrap();
        assert_eq!(Cask::from_api(&waived).unwrap().sha256, None);
    }

    #[test]
    fn formula_parse_from_api_shape() {
        let v: Value = aterm_json::from_str(
            r#"{"name":"wget","full_name":"wget","desc":"Get","homepage":"https://x","license":"GPL-3.0",
                "versions":{"stable":"1.25.0"},"urls":{"stable":{"url":"https://y/wget.tar.gz","checksum":"abc"}},
                "bottle":{"stable":{"files":{"arm64_tahoe":{"url":"https://ghcr/b","sha256":"def"}}}},
                "dependencies":["openssl@3"],"build_dependencies":["pkgconf"],"keg_only":false}"#,
        )
        .unwrap();
        let f = Formula::from_api(&v).unwrap();
        assert_eq!(f.version, "1.25.0");
        assert_eq!(f.dependencies, ["openssl@3"]);
        assert_eq!(
            f.bottle_for(&["arm64_tahoe".to_string()]).unwrap().sha256,
            "def"
        );
    }
}

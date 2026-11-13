// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native `Brewfile` handling and explicit cleanup of legacy Homebrew bundles.

use std::path::Path;

use super::install::InstallError;
use super::prefix::{Kind, Prefix};

/// The embedded ownership receipt written by older abrew versions.
const BUNDLE_RECEIPT: &str = ".abrew-bundle.json";

fn bundle_record(directory: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(directory)
        .ok()?
        .file_type()
        .is_dir()
    {
        return None;
    }
    let bytes =
        crate::metadata_io::read_bounded_regular(&directory.join(BUNDLE_RECEIPT), 64 * 1024)
            .ok()?;
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
    let word = |key| value.get(key).and_then(aterm_json::Value::as_str);
    if word("installed_by") != Some("abrew")
        || !word("version").is_some_and(super::valid_component)
        || !word("sha256").is_some_and(valid_sha256)
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn runnable_bundle(directory: &Path) -> bool {
    if ![directory.to_path_buf(), directory.join("bin")]
        .iter()
        .all(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
        })
    {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(directory.join("bin/brew")) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    false
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Inspect a legacy Homebrew bundle without executing or changing it.
pub fn bundle_status(prefix: &Prefix) {
    let directory = prefix.homebrew_bundle();
    if let Some(record) = bundle_record(&directory) {
        if !runnable_bundle(&directory) {
            println!(
                "legacy bundled Homebrew: invalid executable at {} (unused; remove with: abrew bundle remove)",
                directory.display()
            );
        } else {
            println!(
                "legacy bundled Homebrew: {} (unused; remove with: abrew bundle remove)",
                directory.join("bin/brew").display()
            );
        }
        print!("{record}");
    } else if directory.symlink_metadata().is_ok() {
        println!(
            "legacy bundled Homebrew: unowned path {} (left untouched)",
            directory.display()
        );
    } else {
        println!("legacy bundled Homebrew: none");
    }
}

/// Explicitly remove only a legacy bundle carrying abrew's embedded ownership receipt.
pub fn bundle_remove(prefix: &Prefix) -> Result<(), InstallError> {
    prefix.ensure()?;
    let dest = prefix.homebrew_bundle();
    match std::fs::symlink_metadata(&dest) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("abrew: cannot inspect bundle: {error}").into()),
        Ok(_) if bundle_record(&dest).is_none() => {
            return Err(InstallError::Failed(format!(
                "abrew: refusing to remove unowned bundle path {}",
                dest.display()
            )));
        }
        Ok(_) => {}
    }
    std::fs::remove_dir_all(&dest)
        .map_err(|error| format!("abrew: cannot remove bundle: {error}"))?;
    println!("abrew: legacy Homebrew bundle removed");
    Ok(())
}

/// What `bundle dump` writes, resolved from its type toggles
/// (brew `SubcommandContext` selection: a type dumps unless
/// disabled, and when any positive type switch is present only
/// the named types dump).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpSelection {
    pub formulae: bool,
    pub casks: bool,
    pub taps: bool,
    /// Any bundle-extension section would dump; not native.
    pub extensions_enabled: bool,
    pub describe: bool,
    pub no_restart: bool,
}

impl DumpSelection {
    /// All core sections with descriptions (test/default use).
    #[must_use]
    pub fn all() -> Self {
        Self {
            formulae: true,
            casks: true,
            taps: true,
            extensions_enabled: false,
            describe: true,
            no_restart: false,
        }
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| {
        let v = v.to_string_lossy();
        let t = v.trim().to_ascii_lowercase();
        !t.is_empty() && !["false", "no", "off", "nil", "0"].contains(&t.as_str())
    })
}

/// Resolve a dump selection from subcommand tokens (the
/// leading `dump` word may be present). `None` for a token
/// dump does not take natively.
#[must_use]
pub fn dump_selection(rest: &[String]) -> Option<DumpSelection> {
    let mut positive: Vec<&str> = Vec::new();
    let mut disabled: Vec<&str> = Vec::new();
    let mut no_describe = false;
    let mut no_restart = false;
    let mut seen_sub = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        if a == "dump" && !seen_sub {
            seen_sub = true;
        } else if a == "--file" {
            it.next()?;
        } else if a.starts_with("--file=") || matches!(a, "-f" | "--force") {
        } else if matches!(a, "--formula" | "--formulae" | "--brews") {
            positive.push("brew");
        } else if matches!(a, "--cask" | "--casks") {
            positive.push("cask");
        } else if matches!(a, "--tap" | "--taps") {
            positive.push("tap");
        } else if matches!(
            a,
            "--no-formula" | "--no-formulae" | "--no-brews" | "--no-dump-brew"
        ) {
            disabled.push("brew");
        } else if matches!(a, "--no-cask" | "--no-casks" | "--no-dump-cask") {
            disabled.push("cask");
        } else if matches!(a, "--no-tap" | "--no-taps" | "--no-dump-tap") {
            disabled.push("tap");
        } else if a == "--no-describe" {
            no_describe = true;
        } else if a == "--no-restart" {
            no_restart = true;
        } else if matches!(a, "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d") {
        } else {
            let ext = a.strip_prefix("--").and_then(|rest| {
                let (is_positive, key) = if let Some(k) = rest.strip_prefix("no-dump-") {
                    (false, k)
                } else if let Some(k) = rest.strip_prefix("no-") {
                    (false, k)
                } else {
                    (true, rest)
                };
                EXTENSION_KEYS
                    .iter()
                    .find(|e| **e == key)
                    .map(|e| (is_positive, *e))
            });
            match ext {
                Some((true, key)) => positive.push(key),
                Some((false, key)) => disabled.push(key),
                None => return None,
            }
        }
    }
    if env_truthy("HOMEBREW_BUNDLE_DUMP_NO_BREW") {
        disabled.push("brew");
    }
    if env_truthy("HOMEBREW_BUNDLE_DUMP_NO_CASK") {
        disabled.push("cask");
    }
    if env_truthy("HOMEBREW_BUNDLE_DUMP_NO_TAP") {
        disabled.push("tap");
    }
    for key in EXTENSION_KEYS {
        if env_truthy(&format!("HOMEBREW_BUNDLE_DUMP_NO_{}", key.to_uppercase())) {
            disabled.push(key);
        }
    }
    let no_type_args = positive.is_empty();
    let selected = |t: &str| !disabled.contains(&t) && (positive.contains(&t) || no_type_args);
    Some(DumpSelection {
        formulae: selected("brew"),
        casks: selected("cask"),
        taps: selected("tap"),
        extensions_enabled: EXTENSION_KEYS.iter().any(|k| selected(k)),
        describe: !no_describe && !env_truthy("HOMEBREW_BUNDLE_NO_DESCRIBE"),
        no_restart,
    })
}

/// brew's trust store (`trust.json`) entry sets, by kind.
#[derive(Default)]
struct TrustSets {
    taps: Vec<String>,
    formulae: Vec<String>,
    casks: Vec<String>,
    commands: Vec<String>,
}

impl TrustSets {
    fn read() -> Self {
        let store = super::trust::read_store();
        let take = |key: &str| -> Vec<String> {
            store
                .get(key)
                .and_then(aterm_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(aterm_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            taps: take("trustedtaps"),
            formulae: take("trustedformulae"),
            casks: take("trustedcasks"),
            commands: take("trustedcommands"),
        }
    }
}

/// Whether `reference` names `tap` for trust purposes
/// (the non-remote arm of brew `Tap#matches_reference?`).
fn tap_matches_reference(tap: &super::tap::Tap, reference: &str) -> bool {
    let remote = super::tap::remote_url(tap);
    let custom = remote
        .as_deref()
        .is_some_and(|r| r != super::tap::default_remote(&tap.user, &tap.repo));
    !custom && tap.name() == reference.to_lowercase()
}

fn dump_taps(
    prefix: &Prefix,
    trust: &TrustSets,
    dumped_formulae: &[String],
    dumped_casks: &[String],
) -> String {
    let token = std::env::var_os("HOMEBREW_GITHUB_API_TOKEN")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|v| !v.trim().is_empty());
    let mut lines: Vec<String> = Vec::new();
    for tap in super::tap::list(prefix) {
        let name = tap.name();
        let mut line = format!("tap \"{name}\"");
        if let Some(remote) = super::tap::remote_url(&tap)
            && remote != super::tap::default_remote(&tap.user, &tap.repo)
        {
            let remote = match &token {
                Some(t) => {
                    remote.replace(t.as_str(), "#{ENV.fetch(\"HOMEBREW_GITHUB_API_TOKEN\")}")
                }
                None => remote,
            };
            line.push_str(&format!(", \"{remote}\""));
        }
        if trust.taps.iter().any(|r| tap_matches_reference(&tap, r)) {
            line.push_str(", trusted: true");
        } else {
            let groups: [(&str, &[String], &[String]); 3] = [
                ("formulae", dumped_formulae, &trust.formulae),
                ("casks", dumped_casks, &trust.casks),
                ("commands", &[], &trust.commands),
            ];
            let mut options: Vec<String> = Vec::new();
            for (key, dumped, entries) in groups {
                let mut items: Vec<&str> = entries
                    .iter()
                    .filter_map(|entry| {
                        let (reference, item) = entry.rsplit_once('/')?;
                        if reference.is_empty() || item.is_empty() {
                            return None;
                        }
                        if reference != name && !tap_matches_reference(&tap, reference) {
                            return None;
                        }
                        if dumped.iter().any(|d| d == &format!("{name}/{item}")) {
                            return None;
                        }
                        Some(item)
                    })
                    .collect();
                items.sort_unstable();
                items.dedup();
                if !items.is_empty() {
                    let rendered: Vec<String> = items.iter().map(|i| format!("\"{i}\"")).collect();
                    options.push(format!("{key}: [{}]", rendered.join(", ")));
                }
            }
            if !options.is_empty() {
                line.push_str(&format!(", trusted: {{ {} }}", options.join(", ")));
            }
        }
        lines.push(line);
    }
    lines.sort();
    lines.dedup();
    lines.join("\n")
}

#[cfg(target_os = "macos")]
fn dump_service_started(short: &str) -> bool {
    let label = format!("homebrew.mxcl.{short}");
    super::fetch::run_capture("/bin/launchctl", &["list", &label])
        .is_ok_and(|out| out.status.success())
}

#[cfg(not(target_os = "macos"))]
fn dump_service_started(_short: &str) -> bool {
    false
}

/// Ruby `TSort` component order over `nodes` (insertion
/// order, children in listed order): post-order DFS, each
/// node after its children. Cyclic edges are skipped.
fn dump_tsort(nodes: &[String], children: &dyn Fn(&str) -> Vec<String>) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        Active,
        Done,
    }
    let mut marks: std::collections::HashMap<String, Mark> = std::collections::HashMap::new();
    let mut out: Vec<String> = Vec::new();
    fn visit(
        node: &str,
        children: &dyn Fn(&str) -> Vec<String>,
        marks: &mut std::collections::HashMap<String, Mark>,
        out: &mut Vec<String>,
    ) {
        match marks.get(node).copied().unwrap_or(Mark::Unseen) {
            Mark::Done | Mark::Active => return,
            Mark::Unseen => {}
        }
        marks.insert(node.to_owned(), Mark::Active);
        for child in children(node) {
            visit(&child, children, marks, out);
        }
        marks.insert(node.to_owned(), Mark::Done);
        out.push(node.to_owned());
    }
    for node in nodes {
        visit(node, children, &mut marks, &mut out);
    }
    out
}

fn dump_formulae(
    prefix: &Prefix,
    api: &super::api::Api,
    trust: &TrustSets,
    describe: bool,
    no_restart: bool,
) -> Vec<(String, String)> {
    struct Row {
        name: String,
        full: String,
        version: String,
        on_request: bool,
        deps: Vec<String>,
        used_options: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let installed = prefix.installed(Kind::Formula);
    // Step 1: core before tap, then full name.
    let mut keyed: Vec<(bool, String, &super::prefix::Installed)> = installed
        .iter()
        .map(|i| {
            let full = prefix.specified_name(i);
            (full.contains('/'), full, i)
        })
        .collect();
    keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for (_, full, i) in keyed {
        let short = i.name.clone();
        let version = prefix
            .linked_keg_version(&short)
            .unwrap_or_else(|| i.version.clone());
        let keg = prefix.keg(&short, &version);
        let (on_request, runtime_deps, used_options) = super::prefix::receipt_dump_fields(&keg);
        let deps = runtime_deps.unwrap_or_else(|| {
            api.formula(&short)
                .map(|f| f.dependencies)
                .unwrap_or_default()
        });
        rows.push(Row {
            name: short,
            full,
            version,
            on_request,
            deps,
            used_options,
        });
    }
    // Step 2: dependency topology (brew `Topo`).
    let by_name: std::collections::HashMap<&str, &Row> =
        rows.iter().map(|r| (r.name.as_str(), r)).collect();
    let by_full: std::collections::HashMap<&str, &Row> =
        rows.iter().map(|r| (r.full.as_str(), r)).collect();
    let mut nodes: Vec<String> = Vec::new();
    for r in &rows {
        nodes.push(r.name.clone());
        nodes.push(r.full.clone());
    }
    let ordered = dump_tsort(&nodes, &|node| {
        let Some(row) = by_full.get(node).or_else(|| by_name.get(node)) else {
            return Vec::new();
        };
        row.deps
            .iter()
            .filter_map(|d| {
                let dep_short = super::formula::Formula::short_name(d);
                by_full
                    .get(d.as_str())
                    .or_else(|| by_name.get(dep_short))
                    .map(|r| r.full.clone())
            })
            .collect()
    });
    let mut seen: Vec<&Row> = Vec::new();
    for node in &ordered {
        if let Some(row) = by_full
            .get(node.as_str())
            .or_else(|| by_name.get(node.as_str()))
            && !seen.iter().any(|r| r.full == row.full)
        {
            seen.push(row);
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for row in seen.into_iter().filter(|r| r.on_request) {
        let record = api
            .formula(&row.name)
            .ok()
            .or_else(|| super::tap::lookup(prefix, &row.full));
        let mut line = String::new();
        if describe
            && let Some(f) = &record
            && !f.desc.is_empty()
        {
            for s in f.desc.split('\n') {
                line.push_str(&format!("# {s}\n"));
            }
        }
        line.push_str(&format!("brew \"{}\"", row.full));
        let mut args = row.used_options.clone();
        if row.version.starts_with("HEAD") {
            args.push("HEAD".to_owned());
        }
        args.sort();
        args.dedup();
        if !args.is_empty() {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
            line.push_str(&format!(", args: [{}]", quoted.join(", ")));
        }
        if !no_restart
            && record.as_ref().is_some_and(|f| f.service.is_some())
            && dump_service_started(&row.name)
        {
            line.push_str(", restart_service: :changed");
        }
        if let Some(f) = &record {
            let linked = prefix.linked_keg_exists(&row.name);
            let link = match (linked, f.keg_only) {
                (true, true) => Some(true),
                (false, false) => Some(false),
                _ => None,
            };
            if let Some(link) = link {
                line.push_str(&format!(", link: {link}"));
            }
        }
        if trust.formulae.iter().any(|t| t == &row.full) {
            line.push_str(", trusted: true");
        }
        out.push((row.full.clone(), line));
    }
    out
}

fn dump_cask_config(prefix: &Prefix, token: &str) -> Option<String> {
    let path = prefix
        .root
        .join("Caskroom")
        .join(token)
        .join(".metadata/config.json");
    let bytes = crate::metadata_io::read_bounded_regular(&path, 64 * 1024).ok()?;
    let config = aterm_json::from_slice::<aterm_json::Value>(&bytes).ok()?;
    let explicit = config.get("explicit")?.as_object()?;
    if explicit.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME")?.to_string_lossy().into_owned();
    let mut parts: Vec<String> = Vec::new();
    for (key, value) in explicit {
        let (key, rendered) = if key == "languages" {
            let joined = value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(aterm_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            ("language", joined)
        } else {
            (key.as_str(), value.as_str().unwrap_or_default().to_owned())
        };
        let rendered = match rendered.strip_prefix(home.as_str()) {
            Some(rest) => format!("~{rest}"),
            None => rendered,
        };
        parts.push(format!("{key}: \"{rendered}\""));
    }
    Some(parts.join(", "))
}

fn dump_casks(
    prefix: &Prefix,
    api: &super::api::Api,
    trust: &TrustSets,
    describe: bool,
) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for i in prefix.installed(Kind::Cask) {
        let full = prefix.specified_name(&i);
        let mut line = String::new();
        if describe
            && let Ok(c) = api.cask(&i.name)
            && !c.desc.is_empty()
        {
            line.push_str(&format!("# {}\n", c.desc));
        }
        line.push_str(&format!("cask \"{full}\""));
        if let Some(config) = dump_cask_config(prefix, &i.name) {
            line.push_str(&format!(", args: {{ {config} }}"));
        }
        if trust.casks.iter().any(|t| t == &full) {
            line.push_str(", trusted: true");
        }
        rows.push((full, line));
    }
    rows.sort();
    rows
}

/// `brew bundle dump` content (`Bundle::Dumper`): tap, brew,
/// cask sections in registration order, newline-terminated.
#[must_use]
pub fn brewfile_dump(prefix: &Prefix, api: &super::api::Api, sel: &DumpSelection) -> String {
    let trust = TrustSets::read();
    let formulae = if sel.formulae {
        dump_formulae(prefix, api, &trust, sel.describe, sel.no_restart)
    } else {
        Vec::new()
    };
    let casks = if sel.casks {
        dump_casks(prefix, api, &trust, sel.describe)
    } else {
        Vec::new()
    };
    let mut sections: Vec<String> = Vec::new();
    if sel.taps {
        let dumped_formulae: Vec<String> = formulae.iter().map(|(n, _)| n.clone()).collect();
        let dumped_casks: Vec<String> = casks.iter().map(|(n, _)| n.clone()).collect();
        sections.push(dump_taps(prefix, &trust, &dumped_formulae, &dumped_casks));
    }
    if sel.formulae {
        sections.push(
            formulae
                .into_iter()
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if sel.casks {
        sections.push(
            casks
                .into_iter()
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let body = sections
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    format!("{body}\n")
}

/// Normalize an official core formula or cask operand in a Brewfile (`name`,
/// `homebrew/[homebrew-]core/name` for `brew`, or `homebrew/[homebrew-]cask/name`
/// for `cask`). Third-party tap packages require tap recipes and are not native.
fn parse_official_package_entry(key: &str, name: &str) -> Option<String> {
    if super::valid_component(name) {
        return Some(name.to_owned());
    }
    let (tap_part, pkg) = name.rsplit_once('/')?;
    if !super::valid_component(pkg) {
        return None;
    }
    let (user, repo) = super::tap::parse_tap_name(tap_part)?;
    match (key, user.as_str(), repo.as_str()) {
        ("brew", "homebrew", "core") | ("cask", "homebrew", "cask") => Some(pkg.to_owned()),
        _ => None,
    }
}

fn normalize_qualified_name(full: &str) -> Option<String> {
    let (tap_part, pkg) = full.rsplit_once('/')?;
    if !super::valid_component(pkg) {
        return None;
    }
    let (user, repo) = super::tap::parse_tap_name(tap_part)?;
    Some(format!("{user}/{repo}/{pkg}"))
}

/// Whether `entry` is already satisfied by `prefix`.
#[must_use]
pub fn entry_installed(prefix: &Prefix, entry: &BrewfileEntry) -> bool {
    match entry {
        BrewfileEntry::Tap(n) => super::tap::parse_tap_name(n)
            .map(|(u, r)| format!("{u}/{r}"))
            .is_some_and(|want| super::tap::list(prefix).iter().any(|t| t.name() == want)),
        BrewfileEntry::Brew(n) => package_installed(prefix, Kind::Formula, n, false),
        BrewfileEntry::Cask(n) => package_installed(prefix, Kind::Cask, n, true),
    }
}

fn package_installed(prefix: &Prefix, kind: Kind, requested: &str, canonical_cask: bool) -> bool {
    let (key, official_tap) = match kind {
        Kind::Formula => ("brew", "homebrew/core"),
        Kind::Cask => ("cask", "homebrew/cask"),
    };
    let Some(raw_short) = parse_official_package_entry(key, requested) else {
        return false;
    };
    let short = if canonical_cask {
        super::api::canonical_cask_token(&raw_short)
    } else {
        raw_short.as_str()
    };
    let Some(inst) = prefix.installed_one(kind, short) else {
        return false;
    };
    let want = format!("{official_tap}/{short}");
    inst.versions.iter().any(|version| {
        let one = super::prefix::Installed {
            name: short.to_owned(),
            version: version.clone(),
            kind,
            versions: Vec::new(),
        };
        normalize_qualified_name(&prefix.full_name(&one)).as_deref() == Some(want.as_str())
    })
}

/// One Brewfile line's instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrewfileEntry {
    Tap(String),
    Brew(String),
    Cask(String),
}

/// Parse a Brewfile's `tap`/`brew`/`cask "name"` lines.
#[must_use]
pub fn parse_brewfile(text: &str) -> (Vec<BrewfileEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let quoted = |key: &str| -> Option<String> {
            let rest = line.strip_prefix(key)?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let rest = rest.get(quote.len_utf8()..)?;
            let end = rest.find(quote)?;
            let name = rest.get(..end)?;
            let tail = rest.get(end + quote.len_utf8()..)?.trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return None;
            }
            if key == "tap" {
                let (user, repo) = super::tap::parse_tap_name(name)?;
                Some(format!("{user}/{repo}"))
            } else {
                parse_official_package_entry(key, name)
            }
        };
        if let Some(tap) = quoted("tap") {
            entries.push(BrewfileEntry::Tap(tap));
        } else if let Some(n) = quoted("brew") {
            entries.push(BrewfileEntry::Brew(n));
        } else if let Some(n) = quoted("cask") {
            entries.push(BrewfileEntry::Cask(n));
        } else {
            skipped.push(line.to_string());
        }
    }
    (entries, skipped)
}

/// Extension DSL keys brew's bundler registers (also their
/// `--<key>` list switches).
pub const EXTENSION_KEYS: &[&str] = &[
    "mas", "vscode", "go", "cargo", "uv", "flatpak", "winget", "krew", "npm",
];

/// One `bundle list` row with the display name brew prints
/// (the `Bundle::Dsl` sanitize rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    Tap(String),
    Brew(String),
    Cask(String),
    Ext(&'static str, String),
}

pub(super) fn sanitize_tap_display(name: &str) -> String {
    let lower = name.to_lowercase();
    match super::tap::parse_tap_name(&lower) {
        Some((user, repo)) => format!("{user}/{repo}"),
        None => lower,
    }
}

fn sanitize_cask_display(name: &str) -> String {
    let parts: Vec<&str> = name.splitn(3, '/').collect();
    let base = if parts.len() == 3 { parts[2] } else { name };
    base.to_lowercase()
}

/// A Brewfile row's directive key and raw quoted name.
fn list_row_parts(line: &str) -> Option<(&str, &str, &str)> {
    let key_end = line.find(|c: char| c.is_whitespace() || c == '(')?;
    let (key, rest) = line.split_at(key_end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = rest.get(quote.len_utf8()..)?;
    let end = inner.find(quote)?;
    let name = inner.get(..end)?;
    if name.is_empty() || name.contains('\\') {
        return None;
    }
    // A trailing Ruby modifier (`if`/`unless`) makes inclusion
    // conditional; refuse rather than misprint.
    let tail = inner.get(end + quote.len_utf8()..).unwrap_or_default();
    let padded = format!(" {tail} ");
    if padded.contains(" if ") || padded.contains(" unless ") {
        return None;
    }
    Some((key, name, tail))
}

fn parse_list_row(line: &str) -> Option<ListEntry> {
    let (key, name, _) = list_row_parts(line)?;
    match key {
        "tap" => Some(ListEntry::Tap(sanitize_tap_display(name))),
        "brew" => Some(ListEntry::Brew(sanitize_brew_entry_name(name))),
        "cask" => Some(ListEntry::Cask(sanitize_cask_display(name))),
        other => EXTENSION_KEYS
            .iter()
            .find(|k| **k == other)
            .map(|k| ListEntry::Ext(k, name.to_owned())),
    }
}

/// Parse Brewfile rows for `bundle list` in file order.
/// Option tails are ignored (brew prints just the name);
/// `None` when any row is a shape we cannot name exactly.
#[must_use]
pub fn parse_brewfile_list(text: &str) -> Option<Vec<ListEntry>> {
    let mut entries = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line == "cask_args"
            || line.starts_with("cask_args ")
            || line.starts_with("cask_args(")
            || line.starts_with("cask_args\t")
        {
            continue;
        }
        entries.push(parse_list_row(line)?);
    }
    Some(entries)
}

/// One Brewfile entry for `bundle cleanup`: the sanitized
/// entry name (brew `Dsl::Entry#name`) plus the raw quoted
/// name (cask entries keep their full name for tap keeping).
/// A Brewfile `trusted:` option value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedOpt {
    True,
    Items(Vec<(String, Vec<String>)>),
    /// Any other Ruby value: brew yields no trust targets.
    Other,
}

pub struct CleanupEntry {
    pub kind: &'static str,
    pub name: String,
    pub raw: String,
    /// The row's `trusted:` option, when present.
    pub trusted: Option<TrustedOpt>,
    /// A tap row's clone target, when declared.
    pub clone_target: Option<String>,
}

/// A quoted string at the start of `s`, returning its
/// contents and the remainder.
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let quote = s.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = s.get(quote.len_utf8()..)?;
    let end = inner.find(quote)?;
    let value = inner.get(..end)?;
    if value.contains('\\') {
        return None;
    }
    Some((value.to_owned(), inner.get(end + quote.len_utf8()..)?))
}

/// Find `key:` at a word boundary in an options tail.
fn find_option<'a>(tail: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}:");
    let mut search = tail;
    while let Some(at) = search.find(&needle) {
        let boundary = at == 0
            || !search[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if boundary {
            return Some(search[at + needle.len()..].trim_start());
        }
        search = &search[at + needle.len()..];
    }
    None
}

fn parse_trusted_items(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let mut items = Vec::new();
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some(sym) = part.strip_prefix(':') {
                if sym.is_empty()
                    || !sym
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'))
                {
                    return None;
                }
                items.push(sym.to_owned());
            } else if part.chars().all(|c| c.is_ascii_digit()) {
                items.push(part.to_owned());
            } else {
                let (text, rest) = take_quoted(part)?;
                if !rest.trim().is_empty() {
                    return None;
                }
                items.push(text);
            }
        }
        return Some(items);
    }
    if let Some(sym) = value.strip_prefix(':') {
        return Some(vec![sym.to_owned()]);
    }
    if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() {
        return Some(vec![value.to_owned()]);
    }
    let (text, rest) = take_quoted(value)?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(vec![text])
}

/// Parse a `trusted:` option from a Brewfile row tail.
/// Outer `None` = unparseable shape; inner `None` = absent.
fn parse_trusted_opt(tail: &str) -> Option<Option<TrustedOpt>> {
    let Some(value) = find_option(tail, "trusted") else {
        return Some(None);
    };
    let value = value.trim_start();
    if value == "true"
        || value
            .strip_prefix("true")
            .is_some_and(|rest| rest.trim_start().starts_with(',') || rest.trim().is_empty())
    {
        return Some(Some(TrustedOpt::True));
    }
    if let Some(rest) = value.strip_prefix('{') {
        // Balanced-brace slice (braces inside strings would
        // need a real lexer; refuse those shapes).
        let mut depth = 1i32;
        let mut end = None;
        for (i, c) in rest.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &rest[..end?];
        let mut pairs: Vec<(String, Vec<String>)> = Vec::new();
        for part in split_top_level_commas(inner) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (key, val) = part.split_once(':')?;
            let key = key.trim().trim_start_matches(':');
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            pairs.push((key.to_owned(), parse_trusted_items(val)?));
        }
        return Some(Some(TrustedOpt::Items(pairs)));
    }
    Some(Some(TrustedOpt::Other))
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut in_string: Option<char> = None;
    for (i, c) in s.char_indices() {
        match in_string {
            Some(q) => {
                if c == q {
                    in_string = None;
                }
            }
            None => match c {
                '"' | '\'' => in_string = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&s[start..]);
    parts
}

/// A tap row's clone target: the second positional string
/// or the `clone_target:` keyword value.
fn parse_clone_target(tail: &str) -> Option<String> {
    if let Some(value) = find_option(tail, "clone_target") {
        return take_quoted(value).map(|(text, _)| text);
    }
    let tail = tail.trim_start().strip_prefix(',')?.trim_start();
    take_quoted(tail).map(|(text, _)| text)
}

/// Parse Brewfile rows for `bundle cleanup`; `None` on any
/// row shape we cannot name exactly (as `bundle list`).
#[must_use]
pub fn parse_brewfile_cleanup(text: &str) -> Option<Vec<CleanupEntry>> {
    let mut entries = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line == "cask_args"
            || line.starts_with("cask_args ")
            || line.starts_with("cask_args(")
            || line.starts_with("cask_args\t")
        {
            continue;
        }
        let (key, name, tail) = list_row_parts(line)?;
        let trusted = parse_trusted_opt(tail)?;
        let clone_target = if key == "tap" {
            parse_clone_target(tail)
        } else {
            None
        };
        let entry = match key {
            "tap" => CleanupEntry {
                kind: "tap",
                name: sanitize_tap_display(name),
                raw: name.to_owned(),
                trusted: trusted.clone(),
                clone_target: clone_target.clone(),
            },
            "brew" => CleanupEntry {
                kind: "brew",
                name: sanitize_brew_entry_name(name),
                raw: name.to_owned(),
                trusted: trusted.clone(),
                clone_target: clone_target.clone(),
            },
            "cask" => CleanupEntry {
                kind: "cask",
                name: sanitize_cask_display(name),
                raw: name.to_owned(),
                trusted: trusted.clone(),
                clone_target: clone_target.clone(),
            },
            other => {
                let kind = EXTENSION_KEYS.iter().find(|k| **k == other)?;
                CleanupEntry {
                    kind,
                    name: name.to_owned(),
                    raw: name.to_owned(),
                    trusted: trusted.clone(),
                    clone_target: clone_target.clone(),
                }
            }
        };
        entries.push(entry);
    }
    Some(entries)
}

/// What `bundle cleanup` acts on, from its type toggles
/// (same selection algebra as dump with cleanup env names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupSelection {
    pub formulae: bool,
    pub casks: bool,
    pub taps: bool,
    pub extensions_enabled: bool,
}

/// Resolve a cleanup selection from subcommand tokens.
/// `None` for a token cleanup does not take natively.
#[must_use]
pub fn cleanup_selection(rest: &[String]) -> Option<CleanupSelection> {
    let mut positive: Vec<&str> = Vec::new();
    let mut disabled: Vec<&str> = Vec::new();
    let mut all = false;
    let mut seen_sub = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        if a == "cleanup" && !seen_sub {
            seen_sub = true;
        } else if a == "--file" {
            it.next()?;
        } else if a.starts_with("--file=") || matches!(a, "-f" | "--force" | "--zap") {
        } else if a == "--all" {
            all = true;
        } else if matches!(a, "--formula" | "--formulae" | "--brews") {
            positive.push("brew");
        } else if matches!(a, "--cask" | "--casks") {
            positive.push("cask");
        } else if matches!(a, "--tap" | "--taps") {
            positive.push("tap");
        } else if matches!(
            a,
            "--no-formula" | "--no-formulae" | "--no-brews" | "--no-cleanup-brew"
        ) {
            disabled.push("brew");
        } else if matches!(a, "--no-cask" | "--no-casks" | "--no-cleanup-cask") {
            disabled.push("cask");
        } else if matches!(a, "--no-tap" | "--no-taps" | "--no-cleanup-tap") {
            disabled.push("tap");
        } else if matches!(a, "--verbose" | "-v" | "--quiet" | "-q" | "--debug" | "-d") {
        } else {
            let ext = a.strip_prefix("--").and_then(|rest| {
                let (is_positive, key) = if let Some(k) = rest.strip_prefix("no-cleanup-") {
                    (false, k)
                } else if let Some(k) = rest.strip_prefix("no-") {
                    (false, k)
                } else {
                    (true, rest)
                };
                EXTENSION_KEYS
                    .iter()
                    .find(|e| **e == key)
                    .map(|e| (is_positive, *e))
            });
            match ext {
                Some((true, key)) => positive.push(key),
                Some((false, key)) => disabled.push(key),
                None => return None,
            }
        }
    }
    if env_truthy("HOMEBREW_BUNDLE_CLEANUP_NO_BREW") {
        disabled.push("brew");
    }
    if env_truthy("HOMEBREW_BUNDLE_CLEANUP_NO_CASK") {
        disabled.push("cask");
    }
    if env_truthy("HOMEBREW_BUNDLE_CLEANUP_NO_TAP") {
        disabled.push("tap");
    }
    for key in EXTENSION_KEYS {
        if env_truthy(&format!(
            "HOMEBREW_BUNDLE_CLEANUP_NO_{}",
            key.to_uppercase()
        )) {
            disabled.push(key);
        }
    }
    let no_type_args = positive.is_empty();
    let selected =
        |t: &str| !disabled.contains(&t) && (positive.contains(&t) || all || no_type_args);
    Some(CleanupSelection {
        formulae: selected("brew"),
        casks: selected("cask"),
        taps: selected("tap"),
        extensions_enabled: EXTENSION_KEYS.iter().any(|k| selected(k)),
    })
}

/// The three removal lists `bundle cleanup` would print.
pub struct CleanupPlan {
    pub casks: Vec<String>,
    pub formulae: Vec<String>,
    pub taps: Vec<String>,
}

struct CleanFormula {
    name: String,
    full: String,
    runtime_deps: Vec<String>,
    build_deps: Vec<String>,
    poured: bool,
    aliases: Vec<String>,
    oldnames: Vec<String>,
    keepme: bool,
}

fn tap_of_full(full: &str) -> Option<String> {
    let parts: Vec<&str> = full.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    let (user, repo) = super::tap::parse_tap_name(&format!("{}/{}", parts[0], parts[1]))?;
    Some(format!("{user}/{repo}"))
}

fn clean_formula_rows(prefix: &Prefix, api: &super::api::Api) -> Vec<CleanFormula> {
    let oldnames_by_name: std::collections::HashMap<String, Vec<String>> = api
        .index_rows_fresh(false)
        .map(|rows| rows.into_iter().map(|r| (r.name, r.oldnames)).collect())
        .unwrap_or_default();
    prefix
        .installed(Kind::Formula)
        .iter()
        .map(|i| {
            let full = prefix.specified_name(i);
            let version = prefix
                .linked_keg_version(&i.name)
                .unwrap_or_else(|| i.version.clone());
            let keg = prefix.keg(&i.name, &version);
            let record = api
                .formula(&i.name)
                .ok()
                .or_else(|| super::tap::lookup(prefix, &full));
            let (receipt_deps, poured) = super::prefix::receipt_cleanup_fields(&keg);
            let runtime_deps = receipt_deps.unwrap_or_else(|| {
                record
                    .as_ref()
                    .map(|f| f.dependencies.clone())
                    .unwrap_or_default()
            });
            let keepme = i.versions.iter().any(|v| {
                let path = prefix.keg(&i.name, v).join(".keepme");
                crate::metadata_io::read_bounded_regular_utf8(&path, 64 * 1024).is_ok_and(|text| {
                    text.lines()
                        .any(|line| std::path::Path::new(line.trim()).exists())
                })
            });
            CleanFormula {
                name: i.name.clone(),
                full,
                runtime_deps,
                build_deps: record
                    .as_ref()
                    .map(|f| f.build_dependencies.clone())
                    .unwrap_or_default(),
                poured,
                aliases: record.map(|f| f.aliases).unwrap_or_default(),
                oldnames: oldnames_by_name.get(&i.name).cloned().unwrap_or_default(),
                keepme,
            }
        })
        .collect()
}

/// `bundle cleanup`'s removal computation (brew
/// `CleanupSubcommand` kept-set algebra), without effects.
#[must_use]
pub fn cleanup_plan(
    prefix: &Prefix,
    api: &super::api::Api,
    entries: &[CleanupEntry],
    sel: &CleanupSelection,
) -> CleanupPlan {
    // Kept casks: entry names canonicalized through installed
    // casks' old tokens.
    let installed_casks = prefix.installed(Kind::Cask);
    let mut cask_oldnames: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for i in &installed_casks {
        let full = prefix.specified_name(i);
        if let Ok(c) = api.cask(&i.name) {
            for old in &c.old_tokens {
                cask_oldnames.insert(old.clone(), full.clone());
                if let Some(tap) = tap_of_full(&full) {
                    cask_oldnames.insert(format!("{tap}/{old}"), full.clone());
                }
            }
        }
    }
    let kept_casks: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "cask")
        .map(|e| {
            cask_oldnames
                .get(&e.name)
                .cloned()
                .unwrap_or_else(|| e.name.clone())
        })
        .collect();
    let casks = if sel.casks {
        let mut current: Vec<String> = installed_casks
            .iter()
            .map(|i| prefix.specified_name(i))
            .collect();
        current.sort();
        current
            .into_iter()
            .filter(|n| !kept_casks.contains(n))
            .collect()
    } else {
        Vec::new()
    };

    // Formula graph in brew's dump order (core first, then
    // dependency topology).
    let mut rows = clean_formula_rows(prefix, api);
    rows.sort_by(|a, b| (a.full.contains('/'), &a.full).cmp(&(b.full.contains('/'), &b.full)));
    let by_name: std::collections::HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(idx, r)| (r.name.as_str(), idx))
        .collect();
    let by_full: std::collections::HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(idx, r)| (r.full.as_str(), idx))
        .collect();
    let find_row = |name: &str| -> Option<usize> {
        by_full.get(name).copied().or_else(|| {
            by_name
                .get(super::formula::Formula::short_name(name))
                .copied()
        })
    };
    let mut nodes: Vec<String> = Vec::new();
    for r in &rows {
        nodes.push(r.name.clone());
        nodes.push(r.full.clone());
    }
    let ordered = dump_tsort(&nodes, &|node| {
        let Some(idx) = find_row(node) else {
            return Vec::new();
        };
        rows[idx]
            .runtime_deps
            .iter()
            .filter_map(|d| find_row(d).map(|di| rows[di].full.clone()))
            .collect()
    });
    let mut ordered_rows: Vec<usize> = Vec::new();
    for node in &ordered {
        if let Some(idx) = find_row(node)
            && !ordered_rows.contains(&idx)
        {
            ordered_rows.push(idx);
        }
    }

    // Alias/oldname canonicalization maps (brew's).
    let mut alias_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut oldname_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for r in &rows {
        for a in &r.aliases {
            alias_map.insert(a.clone(), r.full.clone());
            if let Some(tap) = tap_of_full(&r.full) {
                alias_map.insert(format!("{tap}/{a}"), r.full.clone());
            }
        }
        for o in &r.oldnames {
            oldname_map.insert(o.clone(), r.full.clone());
            if let Some(tap) = tap_of_full(&r.full) {
                oldname_map.insert(format!("{tap}/{o}"), r.full.clone());
            }
        }
    }

    // Kept formulae: brew entries + kept casks' formula deps,
    // canonicalized, plus their recursive dependencies.
    let mut kept: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "brew")
        .map(|e| e.name.clone())
        .collect();
    for kept_cask in &kept_casks {
        let token = super::formula::Formula::short_name(kept_cask);
        if let Ok(c) = api.cask(token) {
            kept.extend(c.depends_formula.iter().cloned());
        }
    }
    for name in &mut kept {
        if let Some(canon) = alias_map
            .get(name.as_str())
            .or_else(|| oldname_map.get(name.as_str()))
        {
            *name = canon.clone();
        }
    }
    fn recursive_deps(
        rows: &[CleanFormula],
        find: &dyn Fn(&str) -> Option<usize>,
        names: &[String],
        checked: &mut Vec<String>,
    ) -> Vec<String> {
        let mut dependencies: Vec<String> = Vec::new();
        for name in names {
            if checked.contains(name) {
                continue;
            }
            let Some(idx) = find(name) else { continue };
            let row = &rows[idx];
            let mut f_deps = row.runtime_deps.clone();
            if !row.poured {
                for d in &row.build_deps {
                    if !f_deps.contains(d) {
                        f_deps.push(d.clone());
                    }
                }
            }
            if f_deps.is_empty() {
                continue;
            }
            checked.push(name.clone());
            let nested = recursive_deps(rows, find, &f_deps, checked);
            dependencies.extend(f_deps);
            dependencies.extend(nested);
        }
        let mut uniq: Vec<String> = Vec::new();
        for d in dependencies {
            if !uniq.contains(&d) {
                uniq.push(d);
            }
        }
        uniq
    }
    let mut checked: Vec<String> = Vec::new();
    let deps = recursive_deps(&rows, &find_row, &kept.clone(), &mut checked);
    kept.extend(deps);

    let in_kept = |full: &str| -> bool {
        if kept.contains(&full.to_owned()) {
            return true;
        }
        let short = super::formula::Formula::short_name(full);
        if kept.iter().any(|k| k == short) {
            return true;
        }
        if let Some(old) = oldname_map.get(full).or_else(|| oldname_map.get(short))
            && kept.contains(old)
        {
            return true;
        }
        if let Some(resolved) = alias_map.get(full) {
            if kept.contains(resolved) {
                return true;
            }
            if kept
                .iter()
                .any(|k| k == super::formula::Formula::short_name(resolved))
            {
                return true;
            }
        }
        false
    };
    let formulae = if sel.formulae {
        ordered_rows
            .iter()
            .map(|&idx| &rows[idx])
            .filter(|r| !in_kept(&r.full) && !r.keepme)
            .map(|r| r.full.clone())
            .collect()
    } else {
        Vec::new()
    };

    let taps = if sel.taps {
        let mut kept_taps: Vec<String> = entries
            .iter()
            .filter(|e| e.kind == "tap")
            .map(|e| e.name.clone())
            .collect();
        for e in entries {
            let from = match e.kind {
                "brew" => tap_of_full(&e.name),
                "cask" => tap_of_full(&e.raw),
                _ => None,
            };
            if let Some(tap) = from {
                kept_taps.push(tap);
            }
        }
        for name in &kept {
            if let Some(idx) = find_row(name) {
                kept_taps.push(
                    tap_of_full(&rows[idx].full).unwrap_or_else(|| "homebrew/core".to_owned()),
                );
            }
        }
        super::tap::list(prefix)
            .iter()
            .map(super::tap::Tap::name)
            .filter(|t| t != "homebrew/core" && !kept_taps.contains(t))
            .collect()
    } else {
        Vec::new()
    };

    CleanupPlan {
        casks,
        formulae,
        taps,
    }
}

thread_local! {
    /// `--file=-` stdin content, read once: admission parses
    /// the Brewfile and the command reads it again, but the
    /// pipe can only be drained once.
    static STDIN_BREWFILE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

const MAX_BREWFILE_BYTES: usize = 4 * 1024 * 1024;

fn read_stdin_brewfile() -> Result<String, String> {
    use std::io::Read as _;
    STDIN_BREWFILE.with(|cell| {
        if let Some(text) = cell.borrow().as_ref() {
            return Ok(text.clone());
        }
        let mut text = String::new();
        std::io::stdin()
            .take((MAX_BREWFILE_BYTES as u64) + 1)
            .read_to_string(&mut text)
            .map_err(|e| format!("abrew: cannot read stdin: {e}"))?;
        if text.len() > MAX_BREWFILE_BYTES {
            return Err("abrew: cannot read stdin: file exceeds size limit".to_string());
        }
        *cell.borrow_mut() = Some(text.clone());
        Ok(text)
    })
}

/// The text of the Brewfile file at `path`, up to 4 MiB: a link there (a dotfiles checkout) is
/// followed, as brew follows it, and what it ends at must be a regular file.
pub(crate) fn read_brewfile_file(path: &Path) -> std::io::Result<String> {
    crate::metadata_io::read_bounded_regular_utf8_follow_final_links(path, MAX_BREWFILE_BYTES)
}

/// Read a Brewfile path (`./Brewfile` by default, as brew's is;
/// `-` reads stdin).
pub fn read_brewfile(path: Option<&Path>) -> Result<String, String> {
    if path == Some(Path::new("-")) {
        return read_stdin_brewfile();
    }
    let p = path.map_or_else(|| deps_brewfile_path(None), Path::to_path_buf);
    read_brewfile_file(&p).map_err(|e| {
        // brew's `Brewfile.read` maps any ENOENT here.
        if e.kind() == std::io::ErrorKind::NotFound {
            "Error: No Brewfile found".to_string()
        } else {
            format!("abrew: cannot read {}: {e}", p.display())
        }
    })
}

/// The Brewfile `deps --brewfile` reads (`Bundle::Brewfile.path`
/// with `global: false`): an explicit non-empty `--brewfile=`
/// value (`-` reads stdin), else `$HOMEBREW_BUNDLE_FILE`, else
/// `./Brewfile`.
#[must_use]
pub fn deps_brewfile_path(value: Option<&str>) -> std::path::PathBuf {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        return std::path::PathBuf::from(value);
    }
    if let Some(value) = std::env::var_os("HOMEBREW_BUNDLE_FILE")
        && value.to_string_lossy().chars().any(|c| !c.is_whitespace())
    {
        // Only the flag's `-` means stdin ([`read_brewfile`]); here it is a file of that name.
        return if value == "-" {
            std::path::PathBuf::from("./-")
        } else {
            std::path::PathBuf::from(value)
        };
    }
    std::path::PathBuf::from("Brewfile")
}

/// `Bundle::Dsl.sanitize_brew_name`: downcase, strip the legacy
/// `homebrew/homebrew/` qualification, and drop a tap repo's
/// `homebrew-` prefix.
fn sanitize_brew_entry_name(name: &str) -> String {
    let name = name.to_lowercase();
    let word = |s: &str, extra: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
    };
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() == 3 && word(parts[0], "-") && word(parts[1], "-") && word(parts[2], "+-.@") {
        if parts[0] == "homebrew" && parts[1] == "homebrew" {
            return parts[2].to_string();
        }
        let repo = parts[1].strip_prefix("homebrew-").unwrap_or(parts[1]);
        return format!("{}/{}/{}", parts[0], repo, parts[2]);
    }
    name
}

/// `Bundle::Dsl.sanitize_cask_name`: last path component, downcased.
fn sanitize_cask_entry_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_lowercase()
}

/// Parse a Brewfile the way `deps --brewfile` consumes it: only
/// `brew`/`cask` entries are inputs (brew's `filter_map` ignores
/// every other entry type, so `tap` and the bundle extension
/// directives are read past), entry options after the quoted name
/// are accepted because deps uses only the name, and names get
/// brew's Dsl sanitization. Anything else is returned as skipped:
/// brew evaluates the file as Ruby, so unrecognised lines are not
/// native.
#[must_use]
pub fn parse_brewfile_deps(text: &str) -> (Vec<(Kind, String)>, Vec<String>) {
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains("#{") {
            skipped.push(line.to_string());
            continue;
        }
        let word_end = line.find(char::is_whitespace).unwrap_or(line.len());
        let (word, rest) = (&line[..word_end], line[word_end..].trim_start());
        // A quoted first argument plus the tail after its quote;
        // an entry's tail is empty, a comment, or `, options`.
        let quoted: fn(&str) -> Option<(&str, &str)> = |rest| {
            let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let inner = rest.get(quote.len_utf8()..)?;
            let end = inner.find(quote)?;
            Some((
                inner.get(..end)?,
                inner.get(end + quote.len_utf8()..)?.trim(),
            ))
        };
        let plain_tail =
            |tail: &str| tail.is_empty() || tail.starts_with('#') || tail.starts_with(',');
        match word {
            "brew" | "cask" => match quoted(rest) {
                Some((name, tail)) if plain_tail(tail) => entries.push(if word == "brew" {
                    (Kind::Formula, sanitize_brew_entry_name(name))
                } else {
                    (Kind::Cask, sanitize_cask_entry_name(name))
                }),
                _ => skipped.push(line.to_string()),
            },
            "tap" | "cargo" | "flatpak" | "go" | "krew" | "mas" | "npm" | "uv" | "vscode"
            | "winget" => match quoted(rest) {
                Some((_, tail)) if plain_tail(tail) => {}
                _ => skipped.push(line.to_string()),
            },
            // `cask_args` takes keyword arguments, never inputs.
            "cask_args" => {}
            _ => skipped.push(line.to_string()),
        }
    }
    (entries, skipped)
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "bundle_legacy_tests.rs"]
mod legacy_tests;

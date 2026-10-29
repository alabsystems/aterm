// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use std::cmp::Ordering;
use std::process::ExitCode;

use super::api::Api;
use super::formula::{Formula, version_cmp};
use super::install::{self, InstallOpts};
use super::prefix::{Installed, Kind, Prefix};
use super::{bundle, cask, fetch, from_install, has_flag, not_native, operands, stub, tap};

/// Return true when no local survey found `name` and the API confirmed it does not exist.
pub(super) fn package_definitively_unknown(
    prefix: &Prefix,
    api: &Api,
    name: &str,
    formula: bool,
    cask: bool,
) -> bool {
    if formula {
        if prefix.installed_one(Kind::Formula, name).is_some()
            || tap::lookup(prefix, name).is_some()
        {
            return false;
        }
        match api.formula(name) {
            Ok(_) => return false,
            Err(e) if e.starts_with("abrew: no such formula:") => {}
            Err(_) => return false,
        }
    }
    if cask {
        if prefix
            .installed_one(Kind::Cask, super::api::canonical_cask_token(name))
            .is_some()
        {
            return false;
        }
        match api.cask(name) {
            Ok(_) => return false,
            Err(e) if e.starts_with("abrew: no such cask:") => {}
            Err(_) => return false,
        }
    }
    formula || cask
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn suggestion(api: &Api, name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let mut best: Option<(usize, String)> = None;
    for casks in [false, true] {
        let Ok(rows) = api.index(casks) else {
            continue;
        };
        for (candidate, _) in rows {
            if candidate == name {
                continue;
            }
            let d = edit_distance(&lower, &candidate.to_lowercase());
            if d <= 2
                && best
                    .as_ref()
                    .is_none_or(|(bd, bn)| d < *bd || (d == *bd && candidate < *bn))
            {
                best = Some((d, candidate));
            }
        }
    }
    best.map(|(_, n)| n)
}

/// Brew `Commands.suggestion_message` over the command dictionary.
pub(super) fn command_suggestion(cmd: &str, dictionary: &[String]) -> String {
    let corrections = spell_correct(dictionary, cmd);
    if corrections.is_empty() {
        String::new()
    } else {
        format!("\nDid you mean {}?", to_sentence_or(&corrections))
    }
}

/// Brew `Utils::Text.to_sentence(.., conjunction: "or")`.
fn to_sentence_or(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [.., last] => format!("{} or {last}", items[..items.len() - 1].join(", ")),
    }
}

/// Brew `FormulaOrCaskUnavailableError#did_you_mean`: empty when
/// the spell checker finds nothing over the formula names.
fn did_you_mean(api: &Api, name: &str) -> String {
    let Ok(rows) = api.index(false) else {
        return String::new();
    };
    let dictionary: Vec<String> = rows.into_iter().map(|(n, _)| n).collect();
    let corrections = spell_correct(&dictionary, name);
    if corrections.is_empty() {
        String::new()
    } else {
        format!("Did you mean {}?", to_sentence_or(&corrections))
    }
}

/// Brew `FormulaUnavailableError#to_s`, without the `Error: `
/// prefix (callers add it, or `Warning: `).
pub(super) fn formula_unavailable(api: &Api, name: &str) -> String {
    let did = did_you_mean(api, name);
    if did.is_empty() {
        format!("No available formula with the name \"{name}\".")
    } else {
        format!("No available formula with the name \"{name}\". {did}")
    }
}

/// Brew `Cask::CaskUnavailableError#to_s` shape.
pub(super) fn cask_unavailable(name: &str) -> String {
    format!("Cask '{name}' is unavailable: No Cask with this name exists.")
}

fn no_such_keg(prefix: &Prefix, short: &str) -> String {
    format!("No such keg: {}", prefix.cellar().join(short).display())
}

/// Brew's two-step failure for a name absent from the prefix:
/// unknown to every index is the unavailable error; a known
/// package takes the caller's not-installed sentence.
fn absent_error(api: &Api, prefix: &Prefix, name: &str, known_sentence: String) -> String {
    if package_definitively_unknown(prefix, api, name, true, true) {
        format!("Error: {}", formula_unavailable(api, name))
    } else {
        format!("Error: {known_sentence}")
    }
}

/// Brew `Utils::Browser.open`: `$HOMEBREW_BROWSER`, else
/// `$BROWSER`, else the OS opener. Best-effort like brew's
/// `safe_system` call sites that only open pages.
fn open_browser(urls: &[String]) {
    let urls: Vec<&str> = urls
        .iter()
        .map(String::as_str)
        .filter(|u| !u.is_empty())
        .collect();
    if urls.is_empty() {
        return;
    }
    let browser = ["HOMEBREW_BROWSER", "BROWSER"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "/usr/bin/open".to_string());
    let _ = super::fetch::run_inherit(&browser, &urls);
}

/// Ruby `ERB::Util.url_encode`: percent-encode every byte outside
/// `[A-Za-z0-9_.\-~]` (a space is `%20`, never `+`).
fn erb_url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn no_such(api: &Api, name: &str, kind: Option<&str>) -> String {
    let mut msg = match kind {
        Some(k) => format!("abrew: no such {k}: {name}"),
        None => format!("abrew: no formula or cask named \"{name}\""),
    };
    if let Some(s) = suggestion(api, name) {
        msg += &format!("\nDid you mean \"{s}\"?");
    }
    msg
}

fn failed(msg: &str) -> ExitCode {
    eprintln!("{}", super::tty::error_line(msg));
    ExitCode::from(1)
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("abrew: {msg}");
    ExitCode::from(2)
}

pub(super) fn cmd_on(prefix: &Prefix) -> ExitCode {
    if let Err(e) = prefix.ensure() {
        return failed(&e);
    }
    match std::fs::remove_file(prefix.disabled_path()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return failed(&format!("abrew: cannot remove off marker: {error}")),
    }
    if let Some(layout) = crate::store::resolve_from(crate::config::cached())
        && let Err(e) = stub::lay(&layout)
    {
        eprintln!("abrew: stub not laid: {e}");
    }
    if crate::config::cached_abrew().enabled() {
        println!("abrew: on — `brew` in aterm sessions uses atpkg's native compatibility");
    } else {
        println!("abrew: on for this prefix, but [abrew] enabled=false still disables native brew");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_off(prefix: &Prefix) -> ExitCode {
    if let Err(e) = prefix.ensure() {
        return failed(&e);
    }
    if let Err(e) = super::prefix::write_metadata(&prefix.disabled_path(), b"off\n") {
        return failed(&format!("abrew: {e}"));
    }
    if let Some(layout) = crate::store::resolve_from(crate::config::cached())
        && let Err(error) = stub::lay(&layout)
    {
        return failed(&format!(
            "abrew: native brew is off, but its managed wrapper could not be refreshed: {error}"
        ));
    }
    println!("abrew: off — native brew commands are disabled until `abrew on`");
    ExitCode::SUCCESS
}

pub(super) fn cmd_status(prefix: &Prefix) -> ExitCode {
    println!("prefix: {}", prefix.root.display());
    println!(
        "reroute: {}",
        if !crate::config::cached_abrew().enabled() {
            "off ([abrew] enabled=false)"
        } else if prefix.is_disabled() {
            "off"
        } else {
            "on"
        }
    );
    println!("implementation: atpkg native (no Homebrew fallback)");
    bundle::bundle_status(prefix);
    println!(
        "installed: {} formulae, {} casks",
        prefix.installed(Kind::Formula).len(),
        prefix.installed(Kind::Cask).len()
    );
    ExitCode::SUCCESS
}

/// Extract the inner pattern from brew's `/regex/` search syntax.
pub(super) fn search_regex_pattern(joined: &str) -> Option<&str> {
    if joined.contains('\n') {
        return None;
    }
    joined.strip_prefix('/')?.strip_suffix('/')
}

/// Compile a `/regex/` search pattern under bounded regex limits.
pub(super) fn compile_search_regex(
    pattern: &str,
) -> Result<aterm_regex::Regex, aterm_regex::Error> {
    aterm_regex::RegexBuilder::new(pattern)
        .size_limit(128 * 1024)
        .step_limit(1 << 22)
        .build()
}

enum SearchQuery {
    Text(String),
    Regex(aterm_regex::Regex),
}

fn simplify_search_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@' || *c == '+')
        .collect()
}

impl SearchQuery {
    fn text(raw: &str) -> Self {
        SearchQuery::Text(simplify_search_text(raw))
    }

    fn hits_any(&self, fields: &[&str]) -> Result<bool, aterm_regex::StepLimitExceeded> {
        match self {
            SearchQuery::Text(q) => Ok(fields
                .iter()
                .any(|f| simplify_search_text(f).contains(q.as_str()))),
            SearchQuery::Regex(re) => {
                for f in fields {
                    if re.try_is_match(f)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }
}

/// Ruby's DidYouMean Jaro distance, ported literally (bit flags
/// become bool vectors; names are far below any width limit).
fn jaro_distance(s1: &[char], s2: &[char]) -> f64 {
    let (s1, s2) = if s1.len() > s2.len() {
        (s2, s1)
    } else {
        (s1, s2)
    };
    let (len1, len2) = (s1.len(), s2.len());
    let range = if len2 > 3 { len2 / 2 - 1 } else { 0 };
    let mut flags1 = vec![false; len1];
    let mut flags2 = vec![false; len2];
    let mut m = 0f64;
    for i in 0..len1 {
        let last = i + range;
        let mut j = i.saturating_sub(range);
        while j <= last && j < len2 {
            if !flags2[j] && s1[i] == s2[j] {
                flags2[j] = true;
                flags1[i] = true;
                m += 1.0;
                break;
            }
            j += 1;
        }
    }
    let mut t = 0f64;
    let mut k = 0usize;
    for i in 0..len1 {
        if flags1[i] {
            let mut j = k;
            let mut index = k;
            while j < len2 {
                index = j;
                if flags2[j] {
                    k = j + 1;
                    break;
                }
                j += 1;
            }
            if index < len2 && s1[i] != s2[index] {
                t += 1.0;
            }
        }
    }
    let t = (t / 2.0).floor();
    if m == 0.0 {
        0.0
    } else {
        (m / len1 as f64 + m / len2 as f64 + (m - t) / m) / 3.0
    }
}

fn jaro_winkler_distance(s1: &[char], s2: &[char]) -> f64 {
    let jaro = jaro_distance(s1, s2);
    if jaro > 0.7 {
        let mut prefix = 0usize;
        while prefix < 4 && prefix < s1.len() && prefix < s2.len() && s1[prefix] == s2[prefix] {
            prefix += 1;
        }
        jaro + (prefix as f64 * 0.1 * (1.0 - jaro))
    } else {
        jaro
    }
}

/// The did_you_mean gem's Levenshtein (the compact in-place variant).
fn levenshtein_distance(s1: &[char], s2: &[char]) -> usize {
    let (n, m) = (s1.len(), s2.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d: Vec<usize> = (0..=m).collect();
    let mut x = 0usize;
    for (idx, c1) in s1.iter().enumerate() {
        let mut i = idx + 1;
        for j in 0..m {
            let cost = usize::from(*c1 != s2[j]);
            x = (d[j + 1] + 1).min(i + 1).min(d[j] + cost);
            d[j] = i;
            i = x;
        }
        d[m] = x;
    }
    x
}

/// DidYouMean::SpellChecker#correct over `dictionary`, in the gem's
/// result order (Jaro-Winkler gate, distance-sorted, then the
/// mistype/misspell Levenshtein cuts). This is brew's fuzzy search.
pub(super) fn spell_correct(dictionary: &[String], input: &str) -> Vec<String> {
    fn normalize(s: &str) -> Vec<char> {
        s.to_lowercase().chars().filter(|c| *c != '@').collect()
    }
    let ninput = normalize(input);
    let threshold = if ninput.len() > 3 { 0.834 } else { 0.77 };
    let mut words: Vec<&String> = dictionary
        .iter()
        .filter(|w| jaro_winkler_distance(&normalize(w), &ninput) >= threshold)
        .filter(|w| w.as_str() != input)
        .collect();
    // The gem sorts ascending then reverses. Sets and
    // distance-descending order match brew; inside an exact
    // Jaro-Winkler tie group brew's order is CRuby's unstable
    // sort (an artifact of its C qsort), which we do not chase —
    // a stable sort keeps dictionary order there.
    words.sort_by(|a, b| {
        let da = jaro_winkler_distance(&a.chars().collect::<Vec<_>>(), &ninput);
        let db = jaro_winkler_distance(&b.chars().collect::<Vec<_>>(), &ninput);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mistype = (ninput.len() as f64 * 0.25).ceil() as usize;
    let corrections: Vec<String> = words
        .iter()
        .filter(|w| levenshtein_distance(&normalize(w), &ninput) <= mistype)
        .map(|w| (*w).clone())
        .collect();
    if !corrections.is_empty() {
        return corrections;
    }
    words
        .iter()
        .filter(|w| {
            let nw = normalize(w);
            levenshtein_distance(&nw, &ninput) < ninput.len().min(nw.len())
        })
        .take(1)
        .map(|w| (*w).clone())
        .collect()
}

fn search_inspect(query: &str) -> String {
    // Ruby String#inspect for the shapes a search term takes.
    let mut out = String::from("\"");
    for c in query.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Brew's pretty install status as visible text (the marks are
/// terminal-only; abrew prints them without brew's ANSI colours,
/// like every other abrew headline).
fn pretty_search_name(
    name: &str,
    row: Option<&super::api::SearchRow>,
    installed: bool,
    force_bold: bool,
) -> String {
    // Brew decorates search names through pretty_install_status:
    // bold follows installed-ness in name search, and is forced
    // in description listings.
    let (deprecated, disabled) = row.map_or((false, false), |r| (r.deprecated, r.disabled));
    super::tty::pretty_install_status(
        name,
        installed,
        false,
        deprecated,
        disabled,
        false,
        true,
        false,
        installed || force_bold,
    )
}

/// Brew's `MissingFormula.disallowed_reason` table (plus the macOS
/// `xcode` entry). Matched against the lowercased query.
fn disallowed_reason(name: &str) -> Option<String> {
    let text = match name {
        "gem" | "rubygem" | "rubygems" => {
            "macOS provides gem as part of Ruby. To install a newer version:\n  brew install ruby\n"
        }
        "pip" => "pip is part of the python formula:\n  brew install python\n",
        "pil" => "Instead of PIL, consider pillow:\n  brew install pillow\n",
        "macruby" => {
            "MacRuby has been discontinued. Consider RubyMotion:\n  brew install --cask rubymotion\n"
        }
        "gsutil" => "gsutil is available through pip:\n  pip3 install gsutil\n",
        "gfortran" => "GNU Fortran is part of the GCC formula:\n  brew install gcc\n",
        "play" => {
            "Play 2.3 replaces the play command with activator:\n  brew install typesafe-activator\n\nYou can read more about this change at:\n  https://www.playframework.com/documentation/2.3.x/Migration23\n  https://www.playframework.com/documentation/2.3.x/Highlights23\n"
        }
        "haskell-platform" => {
            "The components of the Haskell Platform are available separately.\n\nGlasgow Haskell Compiler:\n  brew install ghc\n\nCabal build system:\n  brew install cabal-install\n\nHaskell Stack tool:\n  brew install haskell-stack\n"
        }
        "mysqldump-secure" => {
            "The creator of mysqldump-secure tried to game our popularity metrics.\n"
        }
        "ngrok" => {
            "Upstream sunsetted 1.x in March 2016 and 2.x is not open-source.\n\nIf you wish to use the 2.x release you can install it with:\n  brew install --cask ngrok\n"
        }
        "cargo" => "cargo is part of the rust formula:\n  brew install rust\n",
        "cargo-completion" => {
            "cargo-completion is part of the rust formula:\n  brew install rust\n"
        }
        "uconv" => "uconv is part of the icu4c formula:\n  brew install icu4c\n",
        "postgresql" | "postgres" => {
            "postgresql breaks existing databases on upgrade without human intervention.\n\nSee a more specific version to install with:\n  brew formulae | grep postgresql@\n"
        }
        "xcode" => "Xcode can be installed from the App Store.\n",
        _ if name.contains("lzma") => "lzma is now part of the xz formula:\n  brew install xz\n",
        _ => return None,
    };
    Some(text.to_string())
}

fn tap_migration_map(tap: &tap::Tap) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let file = tap.path.join("tap_migrations.json");
    if let Ok(bytes) = crate::metadata_io::read_bounded_regular(&file, 1024 * 1024)
        && let Ok(aterm_json::Value::Object(obj)) = aterm_json::from_slice(&bytes)
    {
        for (name, target) in obj {
            if let aterm_json::Value::String(target) = target {
                out.insert(name, target);
            }
        }
    }
    out
}

/// Brew's `MissingFormula.tap_migration_reason` over installed
/// taps, then the two core taps (clone file, else the cached raw
/// file via [`Api::core_tap_migrations`]).
fn tap_migration_reason(prefix: &Prefix, api: &Api, name: &str) -> Option<String> {
    let installed = tap::list(prefix);
    let mut sources: Vec<(String, std::collections::BTreeMap<String, String>)> = installed
        .iter()
        .map(|t| (t.name(), tap_migration_map(t)))
        .collect();
    for (casks, tap_name) in [(false, "homebrew/core"), (true, "homebrew/cask")] {
        if !sources.iter().any(|(n, _)| n == tap_name) {
            sources.push((tap_name.to_string(), api.core_tap_migrations(casks)));
        }
    }
    for (old_tap, migrations) in &sources {
        let Some(new_tap) = migrations.get(name) else {
            continue;
        };
        let same_tap = new_tap == name;
        let parts: Vec<&str> = new_tap.split('/').collect();
        let migrated_tap_name = if parts.len() >= 2 {
            Some(format!("{}/{}", parts[0], parts[1]))
        } else {
            None
        };
        let same_tap_new_name = !same_tap && migrated_tap_name.is_none() && !new_tap.contains('/');
        let new_tap_name: Option<String> = if let Some(t) = &migrated_tap_name {
            Some(t.clone())
        } else if new_tap.contains('/') {
            Some(new_tap.clone())
        } else if same_tap_new_name {
            Some(old_tap.clone())
        } else {
            None
        };
        let new_tap_new_name: Option<String> = if migrated_tap_name.is_some() {
            parts.get(2).map(|s| s.to_string())
        } else if same_tap_new_name {
            Some(new_tap.clone())
        } else {
            None
        };
        let mut message = if same_tap {
            "It was migrated from a formula to a cask.\n".to_string()
        } else {
            format!("It was migrated from {old_tap} to {new_tap}.\n")
        };
        let install_cmd = if new_tap_name
            .as_deref()
            .is_some_and(|t| t.starts_with("homebrew/cask"))
            || same_tap
        {
            "install --cask"
        } else {
            "install"
        };
        let new_name = new_tap_new_name.unwrap_or_else(|| name.to_string());
        if same_tap || same_tap_new_name || new_tap_name.as_deref() == Some("homebrew/core") {
            message.push_str("You can install it by running:\n");
        } else if let Some(t) = &new_tap_name {
            message.push_str(&format!(
                "You can access it again by running:\n  brew tap {t}\nAnd then you can install it by running:\n"
            ));
        }
        message.push_str(&format!("  brew {install_cmd} {new_name}\n"));
        return Some(message);
    }
    None
}

/// Brew's silent `deleted_reason`: only when homebrew/core is
/// actually cloned in this prefix (API-mode brew finds nothing
/// either — `Tap.from_path` has no tap on disk).
fn deleted_reason(prefix: &Prefix, name: &str) -> Option<String> {
    let tap = tap::list(prefix)
        .into_iter()
        .find(|t| t.user == "homebrew" && t.repo == "core")?;
    let shard = if name.starts_with("lib") {
        "lib".to_string()
    } else {
        name.chars().next()?.to_string()
    };
    let relative = format!("Formula/{shard}/{name}.rb");
    if tap.path.join(&relative).is_file() {
        return None;
    }
    let diff = tap_git(
        &tap,
        &[
            "diff",
            "--diff-filter=D",
            "--name-only",
            "@{1 month ago}",
            "--",
            &relative,
        ],
    )?;
    if diff.is_empty() {
        return None;
    }
    let log = tap_git(
        &tap,
        &[
            "log",
            "--since=1 month ago",
            "--diff-filter=D",
            "--name-only",
            "--max-count=1",
            "--format=%H%n%h%n%B",
            "--",
            &relative,
        ],
    )?;
    let mut lines = log.lines();
    let _hash = lines.next()?;
    let short_hash = lines.next()?;
    let rest: Vec<&str> = lines.collect();
    let (message_lines, relative_found) = rest.split_at(rest.len().saturating_sub(1));
    let relative_found = relative_found.first().copied().unwrap_or(relative.as_str());
    let mut commit_message = message_lines
        .iter()
        .filter(|l| !l.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n  ");
    let issues = "https://github.com/Homebrew/homebrew-core/issues";
    if let Some(idx) = commit_message.rfind(" (#")
        && commit_message.ends_with(')')
    {
        let num: String = commit_message[idx + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !num.is_empty() {
            commit_message = format!("{} ({issues}/{num})", &commit_message[..idx]);
        }
    }
    Some(format!(
        "{name} was deleted from homebrew/core in commit {short_hash}:\n  {commit_message}\n\nTo show the formula before removal, run:\n  git -C \"$(brew --repo homebrew/core)\" show {short_hash}^:{relative_found}\n\nIf you still use this formula, consider creating your own tap:\n  https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap\n"
    ))
}

/// Silent-mode MissingFormula reason: cask_reason is silent-nil in
/// brew, leaving disallowed / migration / deleted.
fn missing_formula_reason(prefix: &Prefix, api: &Api, query: &str) -> Option<String> {
    disallowed_reason(&query.to_lowercase())
        .or_else(|| tap_migration_reason(prefix, api, query))
        .or_else(|| deleted_reason(prefix, query))
}

enum SearchMode {
    Text(String),
    Regex(aterm_regex::Regex),
}

fn search_field_hits(mode: &SearchMode, fields: &[&str]) -> Result<bool, ExitCode> {
    match mode {
        SearchMode::Text(q) => {
            let q = simplify_search_text(q);
            Ok(fields
                .iter()
                .any(|f| simplify_search_text(f).contains(q.as_str())))
        }
        SearchMode::Regex(re) => {
            for f in fields {
                match re.try_is_match(f) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(_) => {
                        return Err(failed(
                            "abrew: native regex search ran out of its work budget; simplify the pattern",
                        ));
                    }
                }
            }
            Ok(false)
        }
    }
}

/// (formula hits, cask hits, cask canonical names), pretty-printed.
type NameHits = (Vec<String>, Vec<String>, Vec<String>);

/// Brew's name search (substring plus fuzzy), as pretty display
/// strings: (formula hits, cask hits, cask canonical names).
#[allow(clippy::too_many_arguments)]
fn collect_name_hits(
    prefix: &Prefix,
    api: &Api,
    mode: &SearchMode,
    joined: &str,
    formula_wanted: bool,
    cask_wanted: bool,
    installed_f: &std::collections::BTreeSet<String>,
    installed_c: &std::collections::BTreeSet<String>,
) -> Result<NameHits, ExitCode> {
    // A tap-qualified name resolves exactly, in its own section only
    // (string queries only — a /regex/ also splits into three parts).
    let tap_parts: Vec<&str> = if matches!(mode, SearchMode::Text(_)) {
        joined.split('/').collect()
    } else {
        Vec::new()
    };
    let mut formula_hits: Vec<String> = Vec::new();
    let mut cask_hits: Vec<String> = Vec::new();
    let mut cask_hit_names: Vec<String> = Vec::new();
    let mut formula_rows: Vec<super::api::SearchRow> = Vec::new();
    let mut cask_rows: Vec<super::api::SearchRow> = Vec::new();
    if formula_wanted {
        match api.search_index(false) {
            Ok(rows) => formula_rows = rows,
            Err(e) => return Err(failed(&e)),
        }
    }
    if cask_wanted {
        match api.search_index(true) {
            Ok(rows) => cask_rows = rows,
            Err(e) => return Err(failed(&e)),
        }
    }
    // Brew's search also covers tapped formulae and casks (the
    // official taps are the API indexes already loaded).
    for tap in tap::list(prefix) {
        let official = tap.user.eq_ignore_ascii_case("homebrew")
            && matches!(tap.repo.as_str(), "core" | "cask");
        if official {
            continue;
        }
        if formula_wanted {
            for stem in tap_formula_names(&tap) {
                if formula_rows.iter().any(|r| r.name == stem) {
                    continue;
                }
                let (desc, deprecated, disabled) = tap::formula_file(&tap, &stem)
                    .and_then(|file| std::fs::read_to_string(file).ok())
                    .map(|text| {
                        let f = tap::parse_rb_as_data(&text, &stem);
                        (f.desc, f.deprecate, f.disable)
                    })
                    .unwrap_or_default();
                formula_rows.push(super::api::SearchRow {
                    name: stem,
                    aliases: Vec::new(),
                    display_names: Vec::new(),
                    desc,
                    deprecated,
                    disabled,
                });
            }
        }
        if cask_wanted {
            for stem in tap_cask_tokens(&tap) {
                if cask_rows.iter().any(|r| r.name == stem) {
                    continue;
                }
                cask_rows.push(super::api::SearchRow {
                    name: stem,
                    aliases: Vec::new(),
                    display_names: Vec::new(),
                    desc: String::new(),
                    deprecated: false,
                    disabled: false,
                });
            }
        }
    }
    let by_name_f: std::collections::BTreeMap<&str, &super::api::SearchRow> =
        formula_rows.iter().map(|r| (r.name.as_str(), r)).collect();
    if tap_parts.len() == 3 {
        let short = tap_parts[2];
        let is_cask_tap = tap_parts[1] == "cask" || tap_parts[0] != "homebrew";
        if !is_cask_tap && formula_wanted && by_name_f.contains_key(short) {
            formula_hits.push(short.to_string());
        } else if cask_wanted && cask_rows.iter().any(|r| r.name == short) {
            cask_hits.push(short.to_string());
        }
    } else {
        if formula_wanted {
            // Candidates: canonical names then aliases, displayed as found.
            let mut candidates: Vec<(String, &super::api::SearchRow, bool)> = Vec::new();
            for row in &formula_rows {
                candidates.push((row.name.clone(), row, false));
            }
            for row in &formula_rows {
                for alias in &row.aliases {
                    candidates.push((alias.clone(), row, true));
                }
            }
            let mut results: Vec<(String, &super::api::SearchRow, bool)> = Vec::new();
            for cand in &candidates {
                match search_field_hits(mode, &[cand.0.as_str()]) {
                    Ok(true) => results.push(cand.clone()),
                    Ok(false) => {}
                    Err(code) => return Err(code),
                }
            }
            results.sort_by(|a, b| {
                let (ab, av) = a.0.split_once('@').unwrap_or((a.0.as_str(), "0"));
                let (bb, bv) = b.0.split_once('@').unwrap_or((b.0.as_str(), "0"));
                ab.cmp(bb)
                    .then_with(|| super::formula::version_cmp(av, bv))
                    .then_with(|| a.0.cmp(&b.0))
            });
            if let SearchMode::Text(q) = &mode {
                // Brew's fuzzy dictionary is names + full names —
                // canonical names only, never aliases.
                let dictionary: Vec<String> = formula_rows.iter().map(|r| r.name.clone()).collect();
                for correction in spell_correct(&dictionary, q) {
                    if let Some(row) = by_name_f.get(correction.as_str())
                        && !results.iter().any(|r| r.0 == row.name)
                    {
                        results.push((row.name.clone(), row, false));
                    }
                }
            }
            let found: std::collections::BTreeSet<String> =
                results.iter().map(|r| r.0.clone()).collect();
            for (name, row, is_alias) in results {
                if is_alias && found.contains(row.name.as_str()) {
                    continue;
                }
                // Brew's valid_platform? filter never fires for
                // API-loaded formulae (the OS-requirement flags are
                // only recorded while evaluating a formula class),
                // so no platform filtering happens here.
                let installed = installed_f.contains(&row.name);
                formula_hits.push(pretty_search_name(&name, Some(row), installed, false));
            }
        }
        if cask_wanted {
            let mut results: Vec<&super::api::SearchRow> = Vec::new();
            for row in &cask_rows {
                match search_field_hits(mode, &[row.name.as_str()]) {
                    Ok(true) => results.push(row),
                    Ok(false) => {}
                    Err(code) => return Err(code),
                }
            }
            if let SearchMode::Text(q) = &mode {
                let dictionary: Vec<String> = cask_rows.iter().map(|r| r.name.clone()).collect();
                for correction in spell_correct(&dictionary, q) {
                    if let Some(row) = cask_rows.iter().find(|r| r.name == correction)
                        && !results.iter().any(|r| r.name == row.name)
                    {
                        results.push(row);
                    }
                }
            }
            results.sort_by(|a, b| a.name.cmp(&b.name));
            for row in results {
                cask_hit_names.push(row.name.clone());
                cask_hits.push(pretty_search_name(
                    &row.name,
                    Some(row),
                    installed_c.contains(&row.name),
                    false,
                ));
            }
        }
    }
    Ok((formula_hits, cask_hits, cask_hit_names))
}

pub(super) fn cmd_search(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let joined = operands(rest).join(" ");
    if joined.is_empty() {
        return usage("search needs a term");
    }
    let desc = has_flag(rest, "--desc", "");
    let formula_flag = has_flag(rest, "--formula", "") || has_flag(rest, "--formulae", "");
    let cask_flag = has_flag(rest, "--cask", "") || has_flag(rest, "--casks", "");
    // Brew's name search treats both kind switches as --formula.
    let formula_wanted = !cask_flag || formula_flag;
    let cask_wanted = !formula_flag;
    let mode = match search_regex_pattern(&joined) {
        Some(pattern) => match compile_search_regex(pattern) {
            Ok(re) => SearchMode::Regex(re),
            Err(_) => return failed(&format!("Error: {joined} is not a valid regex.")),
        },
        None => SearchMode::Text(joined.clone()),
    };
    let installed_f: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Formula)
        .into_iter()
        .map(|i| i.name)
        .collect();
    let installed_c: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Cask)
        .into_iter()
        .map(|i| i.name)
        .collect();
    let tty = std::io::stdout().is_terminal();
    if desc {
        // Description search shows a kind's section unless the
        // other kind was the only one requested, and separates
        // the sections with a blank line only when neither kind
        // switch was given.
        let mut first_section = true;
        for casks in [false, true] {
            if (casks && formula_flag && !cask_flag) || (!casks && cask_flag && !formula_flag) {
                continue;
            }
            if !first_section && !formula_flag && !cask_flag {
                println!();
            }
            first_section = false;
            println!(
                "{}",
                super::tty::ohai(if casks { "Casks" } else { "Formulae" })
            );
            let rows = match api.search_index(casks) {
                Ok(rows) => rows,
                Err(e) => return failed(&e),
            };
            let mut lines: Vec<(String, &super::api::SearchRow)> = Vec::new();
            for row in &rows {
                let hit = if casks {
                    let mut fields: Vec<&str> =
                        row.display_names.iter().map(String::as_str).collect();
                    fields.push(row.desc.as_str());
                    search_field_hits(&mode, &fields)
                } else {
                    search_field_hits(&mode, &[row.desc.as_str()])
                };
                match hit {
                    Ok(true) => lines.push((row.name.clone(), row)),
                    Ok(false) => {}
                    Err(code) => return code,
                }
            }
            lines.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, row) in lines {
                let shown = pretty_search_name(
                    &name,
                    Some(row),
                    installed_f.contains(&name) || installed_c.contains(&name),
                    true,
                );
                if casks {
                    let desc_text = if row.desc.is_empty() {
                        "[no description]".to_string()
                    } else {
                        row.desc.clone()
                    };
                    if row.display_names.is_empty() {
                        println!("{shown}: {desc_text}");
                    } else {
                        println!("{shown}: ({}) {desc_text}", row.display_names.join(", "));
                    }
                } else if !row.desc.is_empty() {
                    println!("{shown}: {}", row.desc);
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    let (formula_hits, cask_hits, cask_hit_names) = match collect_name_hits(
        prefix,
        api,
        &mode,
        &joined,
        formula_wanted,
        cask_wanted,
        &installed_f,
        &installed_c,
    ) {
        Ok(hits) => hits,
        Err(code) => return code,
    };
    if !formula_hits.is_empty() {
        if tty {
            println!("{}", super::tty::ohai("Formulae"));
        }
        print_columns(&formula_hits);
    }
    if !formula_hits.is_empty() && !cask_hits.is_empty() {
        println!();
    }
    if !cask_hits.is_empty() {
        if tty {
            println!("{}", super::tty::ohai("Casks"));
        }
        print_columns(&cask_hits);
    }
    let any_hits = !formula_hits.is_empty() || !cask_hits.is_empty();
    if tty {
        // Brew's MissingFormula footnote (print_missing_formula_help):
        // text queries only, and not when the query is itself a
        // cask hit.
        if matches!(mode, SearchMode::Text(_))
            && !cask_hit_names.iter().any(|n| n == &joined)
            && let Some(reason) = missing_formula_reason(prefix, api, &joined)
        {
            if any_hits {
                println!();
                println!("If you meant {} specifically:", search_inspect(&joined));
            }
            print!("{reason}");
        }
    }
    if !any_hits {
        eprintln!(
            "Error: No formulae or casks found for {}.",
            search_inspect(&joined)
        );
        return ExitCode::from(1);
    }
    if tty {
        print_regex_help(&joined);
    }
    ExitCode::SUCCESS
}

/// Brew's `print_regex_help`: a text query carrying regex
/// metacharacters (dot excluded — brew's list has none) earns the
/// /slashes/ hint on a tty.
fn print_regex_help(query: &str) {
    const METACHARACTERS: &[&str] = &[
        "\\", "|", "(", ")", "[", "]", "{", "}", "^", "$", "*", "+", "?",
    ];
    if query.starts_with('/') || !METACHARACTERS.iter().any(|char| query.contains(char)) {
        return;
    }
    super::tty::opoo!(
        "Warning: Did you mean to perform a regular expression search?\nSurround your query with /slashes/ to search locally by regex."
    );
}

pub(super) fn keg_abv(dir: &std::path::Path) -> String {
    let mut count = 0usize;
    let mut bytes = 0u64;
    let mut seen: std::collections::BTreeSet<(u64, u64)> = std::collections::BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            // Brew counts each directory entry's own size too.
            bytes = bytes.saturating_add(meta.len());
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            if p.file_name().is_none_or(|n| n != ".DS_Store") {
                count += 1;
            }
            if hardlink_key(&meta).is_none_or(|key| seen.insert(key)) {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    format!(
        "{count} file{}, {}",
        if count == 1 { "" } else { "s" },
        disk_usage_readable(bytes)
    )
}

fn formula_source(prefix: &Prefix, f: &Formula) -> (String, Option<String>) {
    let parts: Vec<&str> = f.full_name.split('/').collect();
    if parts.len() == 3 {
        let (user, repo) = (parts[0], parts[1]);
        let official = user == "homebrew" && matches!(repo, "core" | "cask");
        if !official {
            if let Some(t) = tap::list(prefix)
                .into_iter()
                .find(|t| t.user == user && t.repo == repo)
                && let Some(file) = tap::formula_file(&t, &f.name)
            {
                return (file.display().to_string(), Some(t.name()));
            }
            return (
                format!(
                    "https://github.com/{user}/homebrew-{repo}/blob/HEAD/Formula/{}.rb",
                    f.name
                ),
                Some(format!("{user}/{repo}")),
            );
        }
    }
    (
        format!(
            "https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/{}/{}.rb",
            core_shard(&f.name),
            f.name
        ),
        None,
    )
}

/// An epoch second as brew's `%Y-%m-%d at %H:%M:%S` in local time.
fn format_epoch_local(secs: i64) -> String {
    #[cfg(unix)]
    let offset = aterm_uds::zone::local_offset_at(secs).unwrap_or(0);
    #[cfg(not(unix))]
    let offset = 0;
    let local = secs + offset;
    let days = local.div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
    let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} at {:02}:{:02}:{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Today as a civil `(year, month, day)` in local time.
fn today_civil() -> (i64, i64, i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    #[cfg(unix)]
    let offset = aterm_uds::zone::local_offset_at(now).unwrap_or(0);
    #[cfg(not(unix))]
    let offset = 0;
    aterm_types::rfc3339::civil_from_days((now + offset).div_euclid(86_400))
}

fn parse_iso_date(text: &str) -> Option<(i64, i64, i64)> {
    let mut parts = text.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// Ruby `Date#>>12`: twelve months later, day clamped to the
/// target month's length.
fn add_twelve_months((year, month, day): (i64, i64, i64)) -> (i64, i64, i64) {
    let year = year + 1;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_len = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (year, month, day.min(month_len))
}

fn formula_deprecate_reason(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "does_not_build" => "does not build",
        "no_license" => "has no license",
        "repo_archived" => "has an archived upstream repository",
        "repo_removed" => "has a removed upstream repository",
        "unmaintained" => "is not maintained upstream",
        "unreachable" => "is no longer reliably reachable upstream",
        "unsupported" => "is not supported upstream",
        "deprecated_upstream" => "is deprecated upstream",
        "versioned_formula" => "is a versioned formula",
        "checksum_mismatch" => {
            "was built with an initially released source file that had a different checksum \
             than the current one. Upstream's repository might have been compromised. \
             We can re-package this once upstream has confirmed that they retagged their release"
        }
        _ => return None,
    })
}

fn cask_deprecate_reason(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "discontinued" => "is discontinued upstream",
        "moved_to_mas" => "is now exclusively distributed on the Mac App Store",
        "no_longer_available" => "is no longer available upstream",
        "no_longer_meets_criteria" => "no longer meets the criteria for acceptable casks",
        "unmaintained" => "is not maintained upstream",
        "fails_gatekeeper_check" => "does not pass the macOS Gatekeeper check",
        "unreachable" => "is no longer reliably reachable upstream",
        _ => return None,
    })
}

/// Brew's `DeprecateDisable.message`, first letter upcased as
/// the info printers do.
fn deprecate_disable_message(
    cask: bool,
    deprecated: bool,
    disabled: bool,
    dd: &super::formula::DeprecateDisable,
) -> Option<String> {
    let kind = if deprecated {
        "deprecated"
    } else if disabled {
        "disabled"
    } else {
        return None;
    };
    let raw_reason = if deprecated {
        dd.deprecation_reason.as_deref()
    } else {
        dd.disable_reason.as_deref()
    };
    let reason = raw_reason.map(|r| {
        let mapped = if cask {
            cask_deprecate_reason(r)
        } else {
            formula_deprecate_reason(r)
        };
        mapped.map_or_else(|| r.to_string(), str::to_string)
    });
    let mut message = match reason.as_deref().filter(|r| !r.is_empty()) {
        Some(reason) => format!("{kind} because it {reason}!"),
        None => format!("{kind}!"),
    };
    let disable_date = dd
        .disable_date
        .as_deref()
        .and_then(parse_iso_date)
        .or_else(|| {
            dd.deprecation_date
                .as_deref()
                .and_then(parse_iso_date)
                .map(add_twelve_months)
        });
    if let Some(date) = disable_date {
        let shown = format!("{:04}-{:02}-{:02}", date.0, date.1, date.2);
        if date < today_civil() {
            message.push_str(&format!(" It was disabled on {shown}."));
        } else {
            message.push_str(&format!(" It will be disabled on {shown}."));
        }
    }
    let (replacement_formula, replacement_cask) = if disabled {
        (
            dd.disable_replacement_formula.as_deref(),
            dd.disable_replacement_cask.as_deref(),
        )
    } else {
        (
            dd.deprecation_replacement_formula.as_deref(),
            dd.deprecation_replacement_cask.as_deref(),
        )
    };
    let replacement = match (replacement_formula, replacement_cask) {
        (Some(f), Some(c)) if f == c => Some(f.to_string()),
        (Some(f), _) => Some(format!("--formula {f}")),
        (None, Some(c)) => Some(format!("--cask {c}")),
        (None, None) => None,
    };
    if let Some(replacement) = replacement.filter(|r| !r.is_empty()) {
        message.push_str(&format!("\nReplacement:\n  brew install {replacement}\n"));
    }
    let mut chars = message.chars();
    let first = chars.next()?.to_uppercase().collect::<String>();
    Some(first + chars.as_str())
}

/// Substitute the API caveats placeholders (`$HOMEBREW_PREFIX`,
/// `$HOMEBREW_CELLAR`, `$APPDIR`) with this prefix's paths, as
/// brew's API loader substitutes its own.
fn expand_caveats(prefix: &Prefix, caveats: &str) -> String {
    caveats
        .replace("$HOMEBREW_PREFIX", &prefix.root.display().to_string())
        .replace("$HOMEBREW_CELLAR", &prefix.cellar().display().to_string())
        .replace("$APPDIR", &prefix.applications().display().to_string())
}

/// The `==> Analytics` summary brew appends to plain `info` for
/// core formulae and casks: per-category totals by period (the
/// `--analytics` tables are the full form). Absent when the API
/// object carries no analytics or analytics are disabled, as in
/// brew (`HOMEBREW_NO_ANALYTICS` / `HOMEBREW_NO_GITHUB_API`).
/// Info's analytics tail: full tables when verbose, the
/// per-period summary otherwise. Guards mirror brew's
/// `formula_output`/`cask_output` env checks.
fn print_analytics_for(verbose: bool, doc: Option<&aterm_json::Value>) {
    if !verbose {
        print_analytics_summary(doc);
        return;
    }
    if ["HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_GITHUB_API"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
    {
        return;
    }
    let Some(doc) = doc else { return };
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(80);
    print_analytics_tables(doc, None, None, width);
}

fn print_analytics_summary(doc: Option<&aterm_json::Value>) {
    if ["HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_GITHUB_API"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
    {
        return;
    }
    let lines = analytics_summary_lines(doc);
    if lines.is_empty() {
        return;
    }
    println!("{}", super::tty::ohai("Analytics"));
    for line in lines {
        println!("{line}");
    }
}

fn analytics_summary_lines(doc: Option<&aterm_json::Value>) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(analytics) = doc
        .and_then(|d| d.get("analytics"))
        .and_then(|a| a.as_object())
        .filter(|a| !a.is_empty())
    else {
        return lines;
    };
    let mut categories: Vec<&String> = ["install", "install_on_request", "build_error"]
        .iter()
        .filter_map(|c| analytics.get_key_value(*c).map(|(k, _)| k))
        .collect();
    categories.extend(
        analytics
            .keys()
            .filter(|k| !["install", "install_on_request", "build_error"].contains(&k.as_str())),
    );
    for category in categories {
        let Some(by_days) = analytics.get(category).and_then(|v| v.as_object()) else {
            continue;
        };
        let mut day_keys: Vec<(&String, u64)> = by_days
            .keys()
            .map(|k| (k, k.trim_end_matches('d').parse::<u64>().unwrap_or(0)))
            .collect();
        day_keys.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        let summaries: Vec<String> = day_keys
            .iter()
            .map(|(day_key, days)| {
                let total = by_days
                    .get(*day_key)
                    .and_then(|v| v.as_object())
                    .map(|results| {
                        results
                            .values()
                            .map(|c| c.as_u64().unwrap_or(0))
                            .sum::<u64>()
                    })
                    .unwrap_or(0);
                format!("{} ({days} days)", format_count(total))
            })
            .collect();
        lines.push(format!(
            "{}: {}",
            category.replace('_', "-"),
            summaries.join(", ")
        ));
    }
    lines
}

fn print_formula_info(prefix: &Prefix, api: &Api, f: &Formula, verbose: bool) {
    let inst = prefix.installed_one(Kind::Formula, &f.name);
    let outdated = inst
        .as_ref()
        .is_some_and(|i| version_cmp(&i.version, &f.version) == Ordering::Less);
    // Brew's "(bottled)" and build-dependency display ask whether a
    // bottle pours on THIS machine (stable.bottled? for the local
    // tag) and may pour here (pour_bottle?), not whether any bottle
    // exists for some other platform.
    let machine = super::machine::Machine::this();
    let pour_ok = (!f.pour_requires_clt || machine.has_clt())
        && !matches!(&f.pour_needs, Some(n) if n.starts_with("pour_bottle_only_if")
            || n.starts_with("a pour condition"));
    let bottled = f.bottle_for(&machine.bottle_tags()).is_some() && pour_ok;
    // An outdated display names the full pkg_version (with revision).
    let stable_version = if outdated {
        super::install::keg_version(f)
    } else {
        f.version.clone()
    };
    let stable = if bottled {
        format!("stable {stable_version} (bottled)")
    } else {
        format!("stable {stable_version}")
    };
    let mut spec = if outdated {
        let linked = inst
            .as_ref()
            .and_then(|i| linked_keg_version(prefix, i))
            .unwrap_or_else(|| inst.as_ref().map(|i| i.version.clone()).unwrap_or_default());
        format!("{linked} → {stable}")
    } else {
        stable
    };
    if f.head {
        spec.push_str(", HEAD");
    }
    let status_name = super::tty::pretty_install_status(
        &f.full_name,
        inst.is_some(),
        outdated,
        f.deprecate,
        f.disable,
        false,
        !f.disable,
        false,
        true,
    );
    println!(
        "{}: {spec}{}",
        super::tty::oh1(&status_name),
        if f.keg_only { " [keg-only]" } else { "" }
    );
    if !f.desc.is_empty() {
        println!("{}", f.desc);
    }
    if !f.homepage.is_empty() {
        println!("{}", super::tty::url(&f.homepage));
    }
    if !f.aliases.is_empty() {
        println!("Aliases: {}", f.aliases.join(", "));
    }
    if !f.oldnames.is_empty() {
        println!("Old Names: {}", f.oldnames.join(", "));
    }
    if let Some(message) =
        deprecate_disable_message(false, f.deprecate, f.disable, &f.deprecate_disable)
    {
        println!("{message}");
    }
    if !f.conflicts.is_empty() {
        let mut names = f.conflicts.clone();
        names.sort();
        println!("Conflicts with:");
        for name in names {
            println!("  {name}");
        }
    }
    match &inst {
        Some(i) => {
            let keg = prefix.keg(&f.name, &i.version);
            let on_request = super::prefix::receipt_word(&keg, "installed_on_request").as_deref()
                == Some("true");
            println!(
                "Installed ({})",
                if on_request {
                    "on request"
                } else {
                    "as dependency"
                }
            );
        }
        None => {
            println!("Not installed");
            {
                // Brew fetches the bottle tab only when verbose but
                // prints sizes whenever a manifest is already
                // cached; sizes come from its OCI annotations.
                let tags = super::machine::Machine::this().bottle_tags();
                if verbose {
                    let _ = super::install::fetch_bottle_manifest(prefix, f);
                }
                if let Some(bottle) = f.bottle_for(&tags)
                    && let Ok(text) = std::fs::read_to_string(prefix.cache().join(format!(
                        "{}-{}.bottle_manifest.json",
                        f.name,
                        super::install::keg_version(f)
                    )))
                    && let Ok(manifest) = aterm_json::from_str::<aterm_json::Value>(&text)
                    && let Some(annotations) = manifest
                        .get("manifests")
                        .and_then(|m| m.as_array())
                        .and_then(|entries| {
                            entries.iter().find_map(|entry| {
                                let annotations = entry.get("annotations")?;
                                let refname = annotations
                                    .get("org.opencontainers.image.ref.name")?
                                    .as_str()?;
                                refname
                                    .ends_with(&format!(".{}", bottle.tag))
                                    .then_some(annotations)
                            })
                        })
                {
                    if let Some(size) = annotations
                        .get("sh.brew.bottle.size")
                        .and_then(|v| v.as_str())
                        .and_then(|v| v.parse::<u64>().ok())
                    {
                        println!("Bottle Size: {}", disk_usage_readable(size));
                    }
                    if let Some(size) = annotations
                        .get("sh.brew.bottle.installed_size")
                        .and_then(|v| v.as_str())
                        .and_then(|v| v.parse::<u64>().ok())
                    {
                        println!("Installed Size: {}", disk_usage_readable(size));
                    }
                }
            }
        }
    }
    let (from, tap_name) = formula_source(prefix, f);
    let core_formula = tap_name.is_none();
    println!(
        "From: {}",
        if from.starts_with("http") {
            super::tty::url(&from)
        } else {
            from.clone()
        }
    );
    if let Some(tap_name) = tap_name {
        println!("Tap: {tap_name}");
    }
    if !f.license.is_empty() {
        println!("License: {}", f.license);
    }
    print_metadata_lines(
        prefix,
        &f.name,
        false,
        inst.is_some(),
        inst.is_none() && !bottled,
    );
    let section_rows = installed_section_rows(prefix, api, f, inst.as_ref(), verbose);
    if !section_rows.is_empty() {
        println!(
            "{}",
            super::tty::ohai(&format!(
                "Installed {}Versions",
                if verbose { "Kegs and " } else { "" }
            ))
        );
        let decorated: Vec<String> = section_rows
            .iter()
            .map(|r| {
                super::tty::pretty_install_status(
                    &r.full_name,
                    true,
                    r.outdated,
                    false,
                    false,
                    false,
                    true,
                    false,
                    true,
                )
            })
            .collect();
        let nw = decorated
            .iter()
            .map(|d| super::tty::visible_len(d))
            .max()
            .unwrap_or(0);
        let vw = section_rows
            .iter()
            .map(|r| r.shown.chars().count())
            .max()
            .unwrap_or(0);
        let sw = section_rows
            .iter()
            .map(|r| r.size.chars().count() + 2)
            .max()
            .unwrap_or(0);
        for (row, name) in section_rows.iter().zip(&decorated) {
            let size = format!("({})", row.size);
            let name = format!("{name}{}", " ".repeat(nw - super::tty::visible_len(name)));
            if row.linked {
                println!(
                    "{} {:<vw$} {:<sw$} [Linked]",
                    name,
                    row.shown,
                    size,
                    vw = vw,
                    sw = sw
                );
            } else {
                println!("{} {:<vw$} {}", name, row.shown, size, vw = vw);
            }
            if verbose {
                println!("  Cellar: {}", row.keg.display());
                let poured =
                    super::prefix::receipt_word(&row.keg, "source").as_deref() == Some("bottle");
                let mut tab = if poured {
                    "Poured from bottle".to_string()
                } else {
                    "Built from source".to_string()
                };
                if row.core {
                    tab.push_str(" using the formulae.brew.sh API");
                }
                if let Some(secs) = super::prefix::receipt_word(&row.keg, "time")
                    .and_then(|t| t.parse::<i64>().ok())
                {
                    tab.push_str(&format!(" on {}", format_epoch_local(secs)));
                }
                println!("  {tab}");
            }
        }
    }
    // Brew shows build dependencies only when a keg was built from
    // source or an uninstalled formula would install from source.
    let built_from_source = inst.is_some() && {
        let rack = prefix.cellar().join(&f.name);
        std::fs::read_dir(&rack)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                super::prefix::receipt_word(&e.path(), "source").is_some_and(|w| w != "bottle")
            })
    };
    let mut dep_lines = Vec::new();
    if (built_from_source || (inst.is_none() && !bottled)) && !f.build_dependencies.is_empty() {
        dep_lines.push(format!(
            "Build ({}): {}",
            f.build_dependencies.len(),
            decorate_dep_names(prefix, api, &f.build_dependencies, inst.is_some())
        ));
    }
    if !f.dependencies.is_empty() {
        dep_lines.push(format!(
            "Required ({}): {}",
            f.dependencies.len(),
            decorate_dep_names(prefix, api, &f.dependencies, inst.is_some())
        ));
    }
    if !f.recommended_dependencies.is_empty() {
        dep_lines.push(format!(
            "Recommended ({}): {}",
            f.recommended_dependencies.len(),
            decorate_dep_names(prefix, api, &f.recommended_dependencies, false)
        ));
    }
    if !f.optional_dependencies.is_empty() {
        dep_lines.push(format!(
            "Optional ({}): {}",
            f.optional_dependencies.len(),
            decorate_dep_names(prefix, api, &f.optional_dependencies, false)
        ));
    }
    let closure = if inst.is_some() {
        flat_dependencies(f, false, |n| api.formula(n).ok())
    } else {
        std::collections::BTreeSet::new()
    };
    // Dependents are a terminal-only statistic in brew.
    let dependents = if super::tty::stdout_tty() && inst.is_some() {
        installed_dependent_names(prefix, api, &f.name)
    } else {
        Vec::new()
    };
    if !dep_lines.is_empty() || !closure.is_empty() || !dependents.is_empty() {
        println!("{}", super::tty::ohai("Dependencies"));
        for line in dep_lines {
            println!("{line}");
        }
        if !closure.is_empty() {
            let installed_count = closure
                .iter()
                .filter(|d| prefix.installed_one(Kind::Formula, d).is_some())
                .count();
            let counts = if installed_count == closure.len() {
                format!("all installed {}", super::tty::success("✔"))
            } else {
                format!(
                    "{installed_count} installed {}, {} missing {}",
                    super::tty::success("✔"),
                    closure.len() - installed_count,
                    super::tty::error("✘")
                )
            };
            println!("Recursive Runtime ({}): {counts}", closure.len());
        }
        if !dependents.is_empty() {
            if verbose {
                println!(
                    "Dependents ({}): {}",
                    dependents.len(),
                    dependents.join(", ")
                );
            } else {
                println!("Dependents: {}", dependents.len());
            }
        }
    }
    if !f.options.is_empty() || f.head {
        println!("{}", super::tty::ohai("Options"));
        let mut options: Vec<&(String, String)> = f.options.iter().collect();
        options.sort_by(|a, b| a.0.cmp(&b.0));
        for (flag, description) in options {
            println!("{flag}\n\t{description}");
        }
        if f.head {
            println!("--HEAD\n\tInstall HEAD version");
        }
    }
    if verbose {
        let mut binaries: Vec<String> = match &inst {
            Some(i) => {
                let keg = prefix.keg(
                    &f.name,
                    linked_keg_version(prefix, i)
                        .as_deref()
                        .unwrap_or(&i.version),
                );
                let mut out = Vec::new();
                for sub in ["bin", "sbin"] {
                    if let Ok(entries) = std::fs::read_dir(keg.join(sub)) {
                        for e in entries.flatten() {
                            let is_exec = e.metadata().is_ok_and(|m| {
                                if !m.is_file() {
                                    return false;
                                }
                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt as _;
                                    m.permissions().mode() & 0o111 != 0
                                }
                                #[cfg(not(unix))]
                                {
                                    true
                                }
                            });
                            if is_exec {
                                out.push(e.file_name().to_string_lossy().into_owned());
                            }
                        }
                    }
                }
                out
            }
            // Brew reads the fetched bottle tab's exec files; with
            // no bottle for this machine there is no list. The API
            // executables stand in for the tab's.
            None => {
                let tags = super::machine::Machine::this().bottle_tags();
                if f.bottle_for(&tags).is_some() {
                    f.executables.clone()
                } else {
                    Vec::new()
                }
            }
        };
        binaries.sort();
        binaries.dedup();
        if !binaries.is_empty() {
            println!("{}", super::tty::ohai("Binaries"));
            print_columns(&binaries);
        }
    }
    let info_keg = inst.as_ref().map(|i| {
        prefix.keg(
            &f.name,
            linked_keg_version(prefix, i)
                .as_deref()
                .unwrap_or(&i.version),
        )
    });
    let caveats = formula_caveats_text(prefix, f, info_keg.as_deref());
    if !caveats.is_empty() {
        println!("{}", super::tty::ohai("Caveats"));
        print!("{caveats}");
    }
    if core_formula {
        print_analytics_for(verbose, api.formula_value(&f.name).ok().as_ref());
    }
}

/// One row of brew's Installed Versions section.
struct InstalledRow {
    full_name: String,
    shown: String,
    keg: std::path::PathBuf,
    size: String,
    linked: bool,
    outdated: bool,
    core: bool,
}

/// Brew `installed_section_lines`: rows for the formula, its
/// unversioned parent and its versioned siblings — whichever have
/// kegs — newest keg first per formula (plus any linked keg when
/// not verbose), formulae ordered by their newest keg. Related
/// formulae no longer in the API render without an outdated arrow.
fn installed_section_rows(
    prefix: &Prefix,
    api: &Api,
    f: &Formula,
    inst: Option<&Installed>,
    verbose: bool,
) -> Vec<InstalledRow> {
    let base = f.name.split('@').next().unwrap_or(&f.name);
    let mut related: Vec<(String, Option<Formula>, Installed)> = Vec::new();
    if let Some(i) = inst {
        related.push((f.full_name.clone(), None, i.clone()));
    }
    for other in prefix.installed(Kind::Formula) {
        if other.name == f.name || other.name.split('@').next() != Some(base) {
            continue;
        }
        let loaded = api.formula(&other.name).ok();
        let full = loaded
            .as_ref()
            .map_or_else(|| other.name.clone(), |g| g.full_name.clone());
        related.push((full, loaded, other));
    }
    // Brew orders formulae (and kegs within them) by
    // [version_scheme, version]: the scheme is a tab fact recorded
    // at install time, with the loaded formula's as fallback.
    let keg_scheme = |inst: &Installed, v: &str, fallback: u64| -> u64 {
        super::prefix::receipt_word(&prefix.keg(&inst.name, v), "version_scheme")
            .and_then(|w| w.parse::<u64>().ok())
            .unwrap_or(fallback)
    };
    let fallback_scheme = |loaded: &Option<Formula>, inst: &Installed| -> u64 {
        loaded.as_ref().map_or_else(
            || {
                if inst.name == f.name {
                    f.version_scheme
                } else {
                    0
                }
            },
            |g| g.version_scheme,
        )
    };
    related.sort_by(|a, b| {
        let key = |r: &(String, Option<Formula>, Installed)| {
            (
                keg_scheme(&r.2, &r.2.version, fallback_scheme(&r.1, &r.2)),
                r.2.version.clone(),
            )
        };
        let (sa, va) = key(a);
        let (sb, vb) = key(b);
        sb.cmp(&sa).then_with(|| version_cmp(&vb, &va))
    });
    let mut rows = Vec::new();
    for (full_name, loaded, other_inst) in &related {
        let formula = loaded
            .as_ref()
            .or_else(|| (other_inst.name == f.name).then_some(f));
        let linked_version = linked_keg_version(prefix, other_inst);
        let outdated_i =
            formula.is_some_and(|g| version_cmp(&other_inst.version, &g.version) == Ordering::Less);
        let core = formula.is_none_or(|g| formula_source(prefix, g).1.is_none());
        let mut ordered_versions: Vec<&String> = other_inst.versions.iter().collect();
        ordered_versions.sort_by(|a, b| {
            let fallback = formula.map_or(0, |g| g.version_scheme);
            let sa = keg_scheme(other_inst, a, fallback);
            let sb = keg_scheme(other_inst, b, fallback);
            sb.cmp(&sa).then_with(|| version_cmp(b, a))
        });
        for (idx, v) in ordered_versions.into_iter().enumerate() {
            let linked = linked_version.as_deref() == Some(v.as_str());
            if !verbose && idx != 0 && !linked {
                continue;
            }
            let shown = match formula {
                Some(g) if outdated_i && idx == 0 && super::install::keg_version(g) != *v => {
                    format!("{v} \u{2192} {}", super::install::keg_version(g))
                }
                _ => v.clone(),
            };
            let keg = prefix.keg(&other_inst.name, v);
            rows.push(InstalledRow {
                full_name: full_name.clone(),
                shown,
                size: keg_abv(&keg),
                keg,
                linked,
                outdated: outdated_i,
                core,
            });
        }
    }
    rows
}

/// Brew's tty-only info metadata lines: a pinned note (with the
/// pin symlink's mtime) and, for an uninstalled formula that
/// would build, `Installs from source: yes`.
fn pinned_metadata_line(prefix: &Prefix, name: &str, cask: bool) -> Option<String> {
    let (version, pin) = if cask {
        (
            prefix.cask_pinned_version(name)?,
            prefix.root.join("var/homebrew/pinned_casks").join(name),
        )
    } else {
        (
            prefix.pinned_version(name)?,
            prefix.root.join("Pinned").join(name),
        )
    };
    let mut line = format!("Pinned: {version}");
    if let Some(secs) = std::fs::symlink_metadata(&pin)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
    {
        line.push_str(&format!(" on {}", format_epoch_local(secs)));
    }
    Some(line)
}

fn print_metadata_lines(
    prefix: &Prefix,
    name: &str,
    cask: bool,
    installed: bool,
    installs_from_source: bool,
) {
    if !super::tty::stdout_tty() {
        return;
    }
    if let Some(line) = pinned_metadata_line(prefix, name, cask) {
        println!("{line}");
    }
    if !cask && !installed && installs_from_source {
        println!("Installs from source: yes");
    }
}

/// Brew's `installed_dependent_names`: installed formulae whose
/// recorded runtime closure contains `name`. Abrew receipts carry
/// no tab closure, so each installed formula's declared graph
/// closure stands in for its tab, as elsewhere.
fn installed_dependent_names(prefix: &Prefix, api: &Api, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for other in prefix.installed(Kind::Formula) {
        if other.name == name {
            continue;
        }
        let Ok(g) = api.formula(&other.name) else {
            continue;
        };
        let closure = flat_dependencies(&g, false, |n| api.formula(n).ok());
        if closure.contains(name) {
            out.push(other.name);
        }
    }
    out.sort();
    out
}

/// Brew's `decorate_dependencies`: each name carries its install
/// status on a terminal (plain text off one, so only a terminal
/// pays the per-dep lookups).
fn decorate_dep_names(
    prefix: &Prefix,
    api: &Api,
    names: &[String],
    mark_uninstalled: bool,
) -> String {
    if !super::tty::stdout_tty() {
        return names.join(", ");
    }
    names
        .iter()
        .map(|n| {
            let short = Formula::short_name(n);
            let inst = prefix.installed_one(Kind::Formula, short);
            let outdated = inst.as_ref().is_some_and(|i| {
                api.formula(short)
                    .is_ok_and(|g| version_cmp(&i.version, &g.version) == Ordering::Less)
            });
            super::tty::pretty_install_status(
                n,
                inst.is_some(),
                outdated,
                false,
                false,
                false,
                true,
                mark_uninstalled,
                true,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Brew's assembled formula caveats: the formula's own text, the
/// keg-only notice and the service instructions, joined by blank
/// lines. Empty when there is nothing to say. (Brew also appends
/// shadowed-PATH notes, which depend on the caller's live PATH and
/// are left out rather than guessed.)
/// Brew `Caveats#completions_and_elisp`: the generated
/// "installed to" lines for shell completions/functions and
/// Emacs Lisp files found in the poured keg. Only the invoking
/// shell's line is considered when it names a supported shell.
pub(super) fn completions_and_elisp(
    prefix: &Prefix,
    f: &Formula,
    keg: &std::path::Path,
) -> Vec<String> {
    fn on_path(program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
    }
    // Brew asks $SHELL first, the parent process second.
    let preferred = std::env::var("SHELL").ok().and_then(|s| {
        let base = s
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_start_matches('-')
            .to_string();
        matches!(
            base.as_str(),
            "bash" | "zsh" | "fish" | "pwsh" | "pwsh-preview" | "powershell"
        )
        .then_some(base)
    });
    let invoking = preferred
        .or_else(crate::caller_shell::invoking_shell)
        .unwrap_or_default();
    let base = invoking
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_start_matches('-');
    let shells: &[&str] = match base {
        "bash" => &["bash"],
        "zsh" => &["zsh"],
        "fish" => &["fish"],
        "pwsh" | "pwsh-preview" | "powershell" => &["pwsh"],
        _ => &["bash", "zsh", "fish", "pwsh"],
    };
    completions_and_elisp_for(prefix, f, keg, shells, &on_path)
}

fn completions_and_elisp_for(
    prefix: &Prefix,
    f: &Formula,
    keg: &std::path::Path,
    shells: &[&str],
    on_path: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    fn dir_has(dir: &std::path::Path, pred: impl Fn(&str) -> bool) -> bool {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| pred(&e.file_name().to_string_lossy()))
            })
            .unwrap_or(false)
    }
    let root = if f.keg_only {
        prefix.opt(&f.name).display().to_string()
    } else {
        prefix.root.display().to_string()
    };
    let mut out: Vec<String> = Vec::new();
    for shell in shells {
        if !on_path(shell) {
            continue;
        }
        let (completion, functions) = match *shell {
            "bash" => (dir_has(&keg.join("etc/bash_completion.d"), |_| true), false),
            "fish" => (
                dir_has(&keg.join("share/fish/vendor_completions.d"), |_| true),
                dir_has(&keg.join("share/fish/vendor_functions.d"), |_| true),
            ),
            "zsh" => (
                dir_has(&keg.join("share/zsh/site-functions"), |n| {
                    n.starts_with('_')
                }),
                dir_has(&keg.join("share/zsh/site-functions"), |n| {
                    !n.starts_with('_')
                }),
            ),
            _ => (
                dir_has(&keg.join("share/pwsh/completions"), |_| true),
                false,
            ),
        };
        if !completion && !functions {
            continue;
        }
        let mut installed: Vec<&str> = Vec::new();
        if completion {
            installed.push("completions");
        }
        if functions {
            installed.push("functions");
        }
        let joined = installed.join(" and ");
        let text = match *shell {
            "bash" => {
                format!("Bash completion has been installed to:\n  {root}/etc/bash_completion.d\n")
            }
            "fish" => {
                let mut text = format!("fish {joined} have been installed to:");
                if completion {
                    text.push_str(&format!("\n  {root}/share/fish/vendor_completions.d"));
                }
                if functions {
                    text.push_str(&format!("\n  {root}/share/fish/vendor_functions.d"));
                }
                text.push('\n');
                text
            }
            "zsh" => {
                format!("zsh {joined} have been installed to:\n  {root}/share/zsh/site-functions\n")
            }
            _ => format!(
                "PowerShell completion has been installed to:\n  {root}/share/pwsh/completions\n"
            ),
        };
        out.push(text);
    }
    if !f.keg_only {
        let elisp = keg.join("share/emacs/site-lisp").join(&f.name);
        if dir_has(&elisp, |n| n.ends_with(".el") || n.ends_with(".elc")) {
            out.push(format!(
                "Emacs Lisp files have been installed to:\n  {}/share/emacs/site-lisp/{}\n",
                prefix.root.display(),
                f.name
            ));
        }
    }
    out
}

pub(super) fn formula_caveats_text(
    prefix: &Prefix,
    f: &Formula,
    keg: Option<&std::path::Path>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !f.caveats.trim().is_empty() {
        parts.push(format!("{}\n", expand_caveats(prefix, f.caveats.trim())));
    }
    if let Some(text) = keg_only_text(prefix, f, keg) {
        parts.push(text);
    }
    if let Some(text) = service_caveats(prefix, f) {
        parts.push(text);
    }
    parts.join("\n")
}

/// Brew `KegOnlyReason#to_s` as caveats chomps it: the explanation
/// when present, else the stock text for a symbol reason, else the
/// free-text reason itself.
fn keg_only_reason_text(reason: &str, explanation: &str) -> String {
    if !explanation.is_empty() {
        return explanation.trim_end().to_string();
    }
    match reason {
        ":versioned_formula" => "this is an alternate version of another formula".to_string(),
        ":provided_by_macos" => "macOS already provides this software and installing another version in\nparallel can cause all kinds of trouble".to_string(),
        ":shadowed_by_macos" => "macOS provides similar software and installing this software in\nparallel can cause all kinds of trouble".to_string(),
        other => other.strip_prefix(':').unwrap_or(other).to_string(),
    }
}

/// Brew `Caveats#keg_only_text`: the not-symlinked notice, plus —
/// for an installed keg — PATH, compiler, pkgconf and cmake
/// guidance naming the opt paths in the caller's shell syntax.
fn keg_only_text(prefix: &Prefix, f: &Formula, keg: Option<&std::path::Path>) -> Option<String> {
    if !f.keg_only {
        return None;
    }
    if let Some(keg) = keg {
        let linked = ["bin", "sbin"].iter().any(|sub| {
            std::fs::read_dir(prefix.root.join(sub))
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| std::fs::canonicalize(e.path()).is_ok_and(|t| t.starts_with(keg)))
        });
        if linked {
            return None;
        }
    }
    let (reason, explanation) = match &f.keg_only_reason {
        Some((r, e)) => (r.as_str(), e.as_str()),
        None => ("", ""),
    };
    let mut s = format!(
        "{} is keg-only, which means it was not symlinked into {},\nbecause {}.\n",
        f.name,
        prefix.root.display(),
        keg_only_reason_text(reason, explanation)
    );
    if let Some(keg) = keg {
        let shell = preferred_shell_name();
        let opt = prefix.opt(&f.name);
        let (has_bin, has_sbin) = (keg.join("bin").is_dir(), keg.join("sbin").is_dir());
        if has_bin || has_sbin {
            s.push_str(&format!(
                "\nIf you need to have {} first in your PATH, run:\n",
                f.name
            ));
            for (has, sub) in [(has_bin, "bin"), (has_sbin, "sbin")] {
                if has {
                    let line =
                        prepend_path_in_profile(shell.as_deref(), &opt.join(sub).to_string_lossy())
                            .unwrap_or_default();
                    s.push_str(&format!("  {line}\n"));
                }
            }
        }
        let (has_lib, has_include) = (keg.join("lib").is_dir(), keg.join("include").is_dir());
        if has_lib || has_include {
            s.push_str(&format!(
                "\nFor compilers to find {} you may need to set:\n",
                f.name
            ));
            if has_lib {
                let line = export_value(
                    shell.as_deref(),
                    "LDFLAGS",
                    &format!("-L{}", opt.join("lib").display()),
                )
                .unwrap_or_default();
                s.push_str(&format!("  {line}\n"));
            }
            if has_include {
                let line = export_value(
                    shell.as_deref(),
                    "CPPFLAGS",
                    &format!("-I{}", opt.join("include").display()),
                )
                .unwrap_or_default();
                s.push_str(&format!("  {line}\n"));
            }
            if on_path("pkgconf")
                && (keg.join("lib/pkgconfig").is_dir() || keg.join("share/pkgconfig").is_dir())
            {
                s.push_str(&format!(
                    "\nFor pkgconf to find {} you may need to set:\n",
                    f.name
                ));
                if keg.join("lib/pkgconfig").is_dir() {
                    let line = export_value(
                        shell.as_deref(),
                        "PKG_CONFIG_PATH",
                        &format!("{}", opt.join("lib/pkgconfig").display()),
                    )
                    .unwrap_or_default();
                    s.push_str(&format!("  {line}\n"));
                }
                if keg.join("share/pkgconfig").is_dir() {
                    let line = export_value(
                        shell.as_deref(),
                        "PKG_CONFIG_PATH",
                        &format!("{}", opt.join("share/pkgconfig").display()),
                    )
                    .unwrap_or_default();
                    s.push_str(&format!("  {line}\n"));
                }
            }
            if on_path("cmake")
                && (keg.join("lib/cmake").is_dir() || keg.join("share/cmake").is_dir())
            {
                let line = export_value(
                    shell.as_deref(),
                    "CMAKE_PREFIX_PATH",
                    &opt.to_string_lossy(),
                )
                .unwrap_or_default();
                s.push_str(&format!(
                    "\nFor cmake to find {} you may need to set:\n  {line}\n",
                    f.name
                ));
            }
        }
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

/// Whether `name` is an executable on the caller's PATH.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|dir| {
            let candidate = dir.join(name);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                candidate
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                candidate.is_file()
            }
        })
    })
}

/// Brew `Caveats#service_caveats` on macOS: how to start (or
/// restart) the formula's launchd service, and the manual command.
/// The running check reads launchctl only when the service plist
/// is actually installed.
#[cfg(target_os = "macos")]
fn service_caveats(prefix: &Prefix, f: &Formula) -> Option<String> {
    let service = f.service.as_ref()?;
    let label = format!("homebrew.mxcl.{}", f.name);
    let plist = if service.require_root {
        std::path::PathBuf::from(format!("/Library/LaunchDaemons/{label}.plist"))
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    };
    let running = plist.exists()
        && super::fetch::run_capture("/bin/launchctl", &["list", &label])
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("\"PID\""));
    let sudo = if service.require_root { "sudo " } else { "" };
    let mut lines: Vec<String> = Vec::new();
    if running {
        lines.push(format!("To restart {} after an upgrade:", f.full_name));
        lines.push(format!("  {sudo}brew services restart {}", f.full_name));
    } else if service.require_root {
        lines.push(format!(
            "To start {} now and restart at startup:",
            f.full_name
        ));
        lines.push(format!("  sudo brew services start {}", f.full_name));
    } else {
        lines.push(format!(
            "To start {} now and restart at login:",
            f.full_name
        ));
        lines.push(format!("  brew services start {}", f.full_name));
    }
    lines.push("Or, if you don't want/need a background service you can just run:".to_string());
    lines.push(format!(
        "  {}",
        service_manual_command(&prefix.root.to_string_lossy(), service)
    ));
    Some(format!("{}\n", lines.join("\n")))
}

/// Linux has its own systemd wording in brew; not yet rendered, so
/// service caveats are omitted there rather than approximated.
#[cfg(not(target_os = "macos"))]
fn service_caveats(_prefix: &Prefix, _f: &Formula) -> Option<String> {
    None
}

/// Brew `Service#manual_command`: environment assignments (PATH
/// excluded) followed by the shell-quoted run argv, with API
/// `$HOMEBREW_PREFIX` placeholders resolved to abrew's prefix.
#[cfg(any(target_os = "macos", test))]
fn service_manual_command(prefix_root: &str, service: &super::formula::ServiceInfo) -> String {
    let mut parts: Vec<String> = service
        .environment_variables
        .iter()
        .filter(|(key, _)| key != "PATH")
        .map(|(key, value)| {
            format!(
                "{key}=\"{}\"",
                value.replace("$HOMEBREW_PREFIX", prefix_root)
            )
        })
        .collect();
    parts.extend(
        service
            .run
            .iter()
            .map(|arg| sh_quote(&arg.replace("$HOMEBREW_PREFIX", prefix_root))),
    );
    parts.join(" ")
}

/// Brew `Utils::Shell.sh_quote`: Ruby `Shellwords.escape` (safe
/// characters pass, everything else is backslashed, newline becomes
/// `'<newline>'`) with the backslash before `~` removed so tilde
/// expansion still happens.
fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | ',' | ':' | '/' | '@' | '~' => {
                out.push(c);
            }
            '\n' => out.push_str("'\n'"),
            _ => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    out
}

/// Brew `Utils::Shell.csh_quote`.
fn csh_quote(s: &str) -> String {
    sh_quote(s).replace("'\n'", "'\\\n'")
}

/// Brew `Utils::Shell.pwsh_quote` (also rc's quoting).
fn pwsh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Brew `Utils::Shell.preferred`: the shell named by $SHELL,
/// falling back to the invoking shell. `None` mirrors brew's nil
/// (an unrecognized shell name).
fn preferred_shell_name() -> Option<String> {
    let from = std::env::var("SHELL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(crate::caller_shell::invoking_shell)?;
    let base = from.rsplit('/').next().unwrap_or(&from);
    let base = base.strip_prefix('-').unwrap_or(base);
    matches!(
        base,
        "bash" | "ksh" | "mksh" | "sh" | "zsh" | "fish" | "pwsh" | "rc" | "csh" | "tcsh"
    )
    .then(|| base.to_string())
}

/// Brew `Utils::Shell.profile`.
fn shell_profile(shell: Option<&str>) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match shell {
        Some("bash") => {
            let candidate = format!("{home}/.bash_profile");
            if std::path::Path::new(&candidate).exists() {
                return candidate;
            }
        }
        Some("pwsh") => {
            let candidate = format!("{home}/.config/powershell/Microsoft.PowerShell_profile.ps1");
            if std::path::Path::new(&candidate).exists() {
                return candidate;
            }
        }
        Some("rc") => {
            let candidate = format!("{home}/.rcrc");
            if std::path::Path::new(&candidate).exists() {
                return candidate;
            }
        }
        Some("zsh") => {
            if let Ok(zdotdir) = std::env::var("HOMEBREW_ZDOTDIR")
                && !zdotdir.is_empty()
            {
                return format!("{zdotdir}/.zshrc");
            }
        }
        _ => {}
    }
    match shell {
        Some("bash" | "sh") => "~/.profile",
        Some("csh") => "~/.cshrc",
        Some("tcsh") => "~/.tcshrc",
        Some("fish") => "~/.config/fish/config.fish",
        Some("ksh" | "mksh") => "~/.kshrc",
        Some("pwsh") => "~/.config/powershell/Microsoft.PowerShell_profile.ps1",
        Some("rc") => "~/.rcrc",
        Some("zsh") => "~/.zshrc",
        _ => "~/.profile",
    }
    .to_string()
}

/// Brew `Utils::Shell.profile_redirect_target`.
fn profile_redirect_target(shell: Option<&str>) -> String {
    let target = shell_profile(shell);
    match shell {
        Some("rc" | "pwsh") => {
            if target.starts_with("~/") {
                target
            } else {
                pwsh_quote(&target)
            }
        }
        Some("csh" | "tcsh") => csh_quote(&target),
        _ => sh_quote(&target),
    }
}

/// Brew `Utils::Shell.export_value` for the preferred shell.
fn export_value(shell: Option<&str>, key: &str, value: &str) -> Option<String> {
    match shell {
        Some("bash" | "ksh" | "mksh" | "sh" | "zsh") => {
            Some(format!("export {key}={}", sh_quote(value)))
        }
        Some("fish") => Some(format!("set -gx {key} {}", sh_quote(value))),
        Some("pwsh") => Some(format!("$env:{key} = {}", pwsh_quote(value))),
        Some("rc") => Some(format!("{key}=({})", pwsh_quote(value))),
        Some("csh" | "tcsh") => Some(format!("setenv {key} {};", csh_quote(value))),
        _ => None,
    }
}

/// Brew `Utils::Shell.prepend_path_in_profile`.
fn prepend_path_in_profile(shell: Option<&str>, path: &str) -> Option<String> {
    match shell {
        Some("bash" | "ksh" | "mksh" | "sh" | "zsh") | None => Some(format!(
            "printf '%s\\n' {} >> {}",
            sh_quote(&format!("export PATH={}:$PATH", sh_quote(path))),
            profile_redirect_target(shell)
        )),
        Some("pwsh") => Some(format!(
            "{} >> {}",
            pwsh_quote(&format!(
                "$env:PATH = {} + \":$env:PATH\"",
                pwsh_quote(path)
            )),
            profile_redirect_target(shell)
        )),
        Some("rc") => Some(format!(
            "echo {} >> {}",
            pwsh_quote(&format!("path=({} $path)", pwsh_quote(path))),
            profile_redirect_target(shell)
        )),
        Some("csh" | "tcsh") => Some(format!(
            "printf '%s\\n' {} >> {}",
            csh_quote(&format!("setenv PATH {}:$PATH", csh_quote(path))),
            profile_redirect_target(shell)
        )),
        Some("fish") => Some(format!("fish_add_path {}", sh_quote(path))),
        _ => None,
    }
}

fn keg_time(dir: &std::path::Path) -> Option<u64> {
    let meta = std::fs::metadata(dir.join("INSTALL_RECEIPT.json"))
        .or_else(|_| std::fs::metadata(dir))
        .ok()?;
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn enrich_formula_json(prefix: &Prefix, api: &Api, value: &mut aterm_json::Value) {
    let aterm_json::Value::Object(map) = value else {
        return;
    };
    // Brew's `to_hash` — what `info --json` prints — never carries
    // these raw-API fields (analytics is its own opt-in report), and
    // its bottle `files` holds only the bottle this machine would
    // pour: brew's per-tag API file ships just that tag (or `all`).
    for key in ["analytics", "executables", "generated_date", "variations"] {
        map.remove(key);
    }
    // An API-loaded formula's own `requirements` serialize empty in
    // brew (macos/arch/xcode are all consumed at struct generation),
    // and its patch hashes carry no `type` — the API loader never
    // sets one. Both verified against the oracle (go, wget).
    map.insert(
        "requirements".to_string(),
        aterm_json::Value::Array(Vec::new()),
    );
    if let Some(patches) = map
        .get_mut("patches")
        .and_then(aterm_json::Value::as_array_mut)
    {
        for patch in patches {
            if let Some(obj) = patch.as_object_mut() {
                obj.remove("type");
            }
        }
    }
    if let Some(files) = map
        .get_mut("bottle")
        .and_then(|b| b.as_object_mut())
        .and_then(|b| b.get_mut("stable"))
        .and_then(|s| s.as_object_mut())
        .and_then(|s| s.get_mut("files"))
        .and_then(|f| f.as_object_mut())
    {
        let keep = super::machine::Machine::this()
            .bottle_tags()
            .into_iter()
            .find(|t| files.contains_key(t))
            .or_else(|| files.contains_key("all").then(|| "all".to_string()));
        if let Some(keep) = keep {
            files.retain(|tag, _| *tag == keep);
        }
    }
    let Some(name) = map
        .get("name")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let parsed = api.formula(&name).ok();
    let runtime: Vec<aterm_json::Value> = match &parsed {
        Some(f) => {
            let closure = flat_dependencies(f, false, |n| api.formula(n).ok());
            closure
                .iter()
                .filter_map(|dep| {
                    let inst = prefix.installed_one(Kind::Formula, dep)?;
                    let mut row = aterm_json::Map::new();
                    row.insert(
                        "full_name".to_string(),
                        aterm_json::Value::String(prefix.specified_name(&inst)),
                    );
                    row.insert(
                        "pkg_version".to_string(),
                        aterm_json::Value::String(inst.version.clone()),
                    );
                    row.insert(
                        "declared_directly".to_string(),
                        aterm_json::Value::Bool(f.dependencies.iter().any(|d| d == dep)),
                    );
                    Some(aterm_json::Value::Object(row))
                })
                .collect()
        }
        None => Vec::new(),
    };
    let mut installed_rows = Vec::new();
    let mut linked = aterm_json::Value::Null;
    let mut outdated = false;
    if let Some(inst) = prefix.installed_one(Kind::Formula, &name) {
        for version in &inst.versions {
            let keg = prefix.keg(&name, version);
            let sbom = keg.join("sbom.spdx.json");
            let mut row = aterm_json::Map::new();
            row.insert(
                "sbom".to_string(),
                if sbom.is_file() {
                    aterm_json::Value::String(sbom.to_string_lossy().into_owned())
                } else {
                    aterm_json::Value::Null
                },
            );
            row.insert(
                "version".to_string(),
                aterm_json::Value::String(version.clone()),
            );
            row.insert(
                "used_options".to_string(),
                aterm_json::Value::Array(Vec::new()),
            );
            row.insert("built_as_bottle".to_string(), aterm_json::Value::Bool(true));
            row.insert(
                "poured_from_bottle".to_string(),
                aterm_json::Value::Bool(true),
            );
            row.insert(
                "time".to_string(),
                keg_time(&keg).map_or(aterm_json::Value::Null, aterm_json::Value::from),
            );
            row.insert(
                "runtime_dependencies".to_string(),
                aterm_json::Value::Array(runtime.clone()),
            );
            row.insert(
                "installed_on_request".to_string(),
                aterm_json::Value::Bool(
                    super::prefix::receipt_word(&keg, "installed_on_request").as_deref()
                        == Some("true"),
                ),
            );
            installed_rows.push(aterm_json::Value::Object(row));
        }
        if let Some(v) = linked_keg_version(prefix, &inst) {
            linked = aterm_json::Value::String(v);
        }
        if let Some(f) = &parsed {
            outdated = version_cmp(&inst.version, &f.version) == Ordering::Less;
        }
    }
    map.insert(
        "installed".to_string(),
        aterm_json::Value::Array(installed_rows),
    );
    map.insert("linked_keg".to_string(), linked);
    map.insert(
        "pinned".to_string(),
        aterm_json::Value::Bool(prefix.is_pinned(&name)),
    );
    map.insert("outdated".to_string(), aterm_json::Value::Bool(outdated));
}

/// Brew's `BundleVersion` for an installed cask: `CFBundleShortVersionString`
/// and `CFBundleVersion` from the staged app's `Info.plist`, the short
/// form with any trailing ` (<version>)` removed. Binary plists go
/// through `plutil` exactly as brew's reader does.
fn cask_bundle_versions(dir: &std::path::Path) -> (Option<String>, Option<String>) {
    fn plist_string(xml: &str, key: &str) -> Option<String> {
        let tag = format!("<key>{key}</key>");
        let after = xml.find(&tag)? + tag.len();
        let value = xml[after..].trim_start().strip_prefix("<string>")?;
        let end = value.find("</string>")?;
        let v = value[..end].trim();
        (!v.is_empty() && v.len() <= 64).then(|| v.to_string())
    }
    let mut apps: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().is_some_and(|x| x == "app")
                        && p.join("Contents/Info.plist").is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    apps.sort();
    let Some(app) = apps.first() else {
        return (None, None);
    };
    let plist = app.join("Contents/Info.plist");
    let Ok(bytes) = std::fs::read(&plist) else {
        return (None, None);
    };
    let xml = if bytes.starts_with(b"bplist00") {
        let Ok(out) = super::fetch::run_capture(
            aterm_uds::os_path!("plutil"),
            &[
                "-convert",
                "xml1",
                "-o",
                "-",
                plist.to_str().unwrap_or_default(),
            ],
        ) else {
            return (None, None);
        };
        if !out.status.success() {
            return (None, None);
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let mut short = plist_string(&xml, "CFBundleShortVersionString");
    let long = plist_string(&xml, "CFBundleVersion");
    if let (Some(s), Some(v)) = (&short, &long)
        && let Some(stripped) = s
            .strip_suffix(&format!("({v})"))
            .map(|p| p.trim_end().to_string())
    {
        short = Some(stripped);
    }
    (short, long)
}

/// Brew's `deep_remove_placeholders` (api/cask_struct.rb): every
/// string in the document gets the four generation placeholders
/// substituted, in brew's order.
fn deep_remove_placeholders(
    value: &mut aterm_json::Value,
    home: &str,
    prefix: &str,
    cellar: &str,
    appdir: &str,
) {
    match value {
        aterm_json::Value::Object(map) => {
            for child in map.values_mut() {
                deep_remove_placeholders(child, home, prefix, cellar, appdir);
            }
        }
        aterm_json::Value::Array(items) => {
            for child in items {
                deep_remove_placeholders(child, home, prefix, cellar, appdir);
            }
        }
        aterm_json::Value::String(s) if s.contains('$') => {
            *s = s
                .replace("/$HOME", home)
                .replace("$HOMEBREW_PREFIX", prefix)
                .replace("$HOMEBREW_CELLAR", cellar)
                .replace("$APPDIR", appdir);
        }
        _ => {}
    }
}

fn enrich_cask_json(prefix: &Prefix, value: &mut aterm_json::Value) {
    let home = std::env::var("HOME").unwrap_or_default();
    deep_remove_placeholders(
        value,
        &home,
        &prefix.root.to_string_lossy(),
        &prefix.cellar().to_string_lossy(),
        &prefix.applications().to_string_lossy(),
    );
    let aterm_json::Value::Object(map) = value else {
        return;
    };
    // As on the formula side, brew's cask `to_h` never carries these
    // raw-API fields.
    for key in [
        "analytics",
        "generated_date",
        "supported_platforms",
        "variations",
    ] {
        map.remove(key);
    }
    // Brew's API struct collapses language variations into the one
    // it selected; `to_hash` reports the key as null.
    map.insert("language_variations".to_string(), aterm_json::Value::Null);
    let Some(token) = map
        .get("token")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let latest = map
        .get("version")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string);
    let auto_updates = map
        .get("auto_updates")
        .and_then(aterm_json::Value::as_bool)
        .unwrap_or(false);
    let mut bundle_versions = (None, None);
    let (installed, installed_time, outdated) = match prefix.installed_one(Kind::Cask, &token) {
        Some(inst) => {
            let dir = prefix.cask_dir(&token, &inst.version);
            bundle_versions = cask_bundle_versions(&dir);
            let outdated = !auto_updates
                && latest
                    .as_deref()
                    .is_some_and(|l| version_cmp(&inst.version, l) == Ordering::Less);
            (
                aterm_json::Value::String(inst.version.clone()),
                keg_time(&dir).map_or(aterm_json::Value::Null, aterm_json::Value::from),
                outdated,
            )
        }
        None => (aterm_json::Value::Null, aterm_json::Value::Null, false),
    };
    map.insert(
        "bundle_short_version".to_string(),
        bundle_versions
            .0
            .map_or(aterm_json::Value::Null, aterm_json::Value::String),
    );
    map.insert(
        "bundle_version".to_string(),
        bundle_versions
            .1
            .map_or(aterm_json::Value::Null, aterm_json::Value::String),
    );
    map.insert("installed".to_string(), installed);
    map.insert("installed_time".to_string(), installed_time);
    let pinned_version = prefix.cask_pinned_version(&token);
    map.insert(
        "pinned".to_string(),
        aterm_json::Value::Bool(pinned_version.is_some()),
    );
    map.insert(
        "pinned_version".to_string(),
        pinned_version.map_or(aterm_json::Value::Null, aterm_json::Value::String),
    );
    map.insert("outdated".to_string(), aterm_json::Value::Bool(outdated));
}

fn info_json(
    prefix: &Prefix,
    api: &Api,
    rest: &[String],
    names: &[String],
) -> Result<String, String> {
    let v2 = has_flag(rest, "--json=v2", "");
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let installed = has_flag(rest, "--installed", "");
    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    let mut fname: Vec<String> = names.to_vec();
    let mut cname: Vec<String> = names.to_vec();
    if installed {
        fname = prefix
            .installed(Kind::Formula)
            .into_iter()
            .map(|i| i.name)
            .collect();
        cname = prefix
            .installed(Kind::Cask)
            .into_iter()
            .map(|i| i.name)
            .collect();
    }
    let mut resolved = std::collections::BTreeSet::new();
    if !want_cask {
        for n in &fname {
            match api.formula_value(n) {
                Ok(v) => {
                    resolved.insert(Formula::short_name(n).to_string());
                    formulae.push(v);
                }
                Err(_) if want_formula || installed => {
                    return Err(format!("abrew: no formula {n}"));
                }
                Err(_) => {}
            }
        }
    }
    if !want_formula {
        for n in &cname {
            let short = Formula::short_name(n);
            if !installed
                && !want_cask
                && formulae
                    .iter()
                    .any(|v| v.get("name").and_then(aterm_json::Value::as_str) == Some(short))
            {
                continue;
            }
            match api.cask_value(n) {
                Ok(v) => {
                    resolved.insert(short.to_string());
                    casks.push(v);
                }
                Err(e) => {
                    if want_cask || installed {
                        return Err(e);
                    }
                }
            }
        }
    }
    if !installed {
        for n in names {
            if !resolved.contains(Formula::short_name(n)) {
                return Err(no_such(api, n, None));
            }
        }
    }
    for v in &mut formulae {
        enrich_formula_json(prefix, api, v);
    }
    for v in &mut casks {
        enrich_cask_json(prefix, v);
    }
    let value = if v2 {
        let mut map = aterm_json::Map::new();
        map.insert("formulae".to_string(), aterm_json::Value::Array(formulae));
        map.insert("casks".to_string(), aterm_json::Value::Array(casks));
        aterm_json::Value::Object(map)
    } else {
        aterm_json::Value::Array(formulae.into_iter().chain(casks).collect())
    };
    aterm_json::to_string(&value).map_err(|e| format!("abrew: {e}"))
}

/// Read one unambiguous `--flag value` or `--flag=value` option.
pub(super) fn flag_value(rest: &[String], flag: &str) -> Result<Option<String>, ()> {
    let mut found = None;
    let mut it = rest.iter();
    while let Some(s) = it.next() {
        let value = if s == flag {
            it.next().map(String::as_str)
        } else if let Some(v) = s.strip_prefix(&format!("{flag}=")) {
            Some(v)
        } else {
            continue;
        };
        let value = value
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or(())?;
        if found.replace(value.to_string()).is_some() {
            return Err(());
        }
    }
    Ok(found)
}

/// Return positional operands with valued flags' arguments removed.
pub(super) fn operands_without_flag_values(rest: &[String], valued: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = rest.iter();
    while let Some(s) = it.next() {
        if valued.contains(&s.as_str()) {
            let _ = it.next();
        } else if !s.starts_with('-') {
            out.push(s.clone());
        }
    }
    out
}

fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn analytics_table(category: &str, days: &str, rows: &[(String, u64)], width: usize) -> String {
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let total_count = format_count(total);
    let index_width = [5, rows.len().to_string().len()]
        .into_iter()
        .max()
        .unwrap_or(5);
    let count_width = [5, total_count.len()].into_iter().max().unwrap_or(5);
    let percent_width = 7usize;
    let name_width = width
        .saturating_sub(index_width + count_width + percent_width + 10)
        .max(1);
    let trunc = |s: &str| -> String { s.chars().take(name_width).collect() };
    let mut out = String::new();
    out += &format!(
        "{}\n",
        super::tty::ohai(&format!("{category} ({days} days)"))
    );
    out += &format!(
        "{:>index_width$} | {:<name_width$} | {:>count_width$} |  {:>percent_width$}\n",
        "Index",
        trunc("Name (with options)"),
        "Count",
        "Percent",
    );
    out += &format!(
        "{}:|-{}-|-{}:|-{}:\n",
        "-".repeat(index_width),
        "-".repeat(name_width),
        "-".repeat(count_width),
        "-".repeat(percent_width),
    );
    let max_index = rows.len().to_string().len();
    for (i, (name, count)) in rows.iter().enumerate() {
        let percent = if total == 0 {
            0.0
        } else {
            (*count as f64 * 100.0) / total as f64
        };
        out += &format!(
            "{:<index_width$} | {:<name_width$} | {:>count_width$} | {:>percent_width$}%\n",
            format!("{:0max_index$}", i + 1),
            trunc(name),
            format_count(*count),
            format!("{percent:.2}"),
        );
    }
    if rows.len() > 1 {
        out += &format!(
            "{:<index_width$} | {:<name_width$} | {:>count_width$} | {:>percent_width$}%\n",
            "Total", "", total_count, "100.00",
        );
    }
    out
}

/// The full per-category analytics tables of one API object,
/// shared by `info --analytics` and verbose `info`.
fn print_analytics_tables(
    doc: &aterm_json::Value,
    days_filter: Option<&str>,
    category_filter: Option<&str>,
    width: usize,
) {
    let Some(analytics) = doc.get("analytics").and_then(|a| a.as_object()) else {
        return;
    };
    if analytics.is_empty() {
        return;
    }
    println!("{}", super::tty::ohai("Analytics"));
    let mut categories: Vec<&String> = ["install", "install_on_request", "build_error"]
        .iter()
        .filter_map(|c| analytics.get_key_value(*c).map(|(k, _)| k))
        .collect();
    categories.extend(
        analytics
            .keys()
            .filter(|k| !["install", "install_on_request", "build_error"].contains(&k.as_str())),
    );
    for category in categories {
        let shown_category = category.replace('_', "-");
        if category_filter.is_some_and(|f| f != shown_category) {
            continue;
        }
        let Some(by_days) = analytics.get(category).and_then(|v| v.as_object()) else {
            continue;
        };
        let mut day_keys: Vec<(&String, u64)> = by_days
            .keys()
            .map(|k| {
                (
                    k,
                    k.trim_end_matches('d').parse::<u64>().unwrap_or(u64::MAX),
                )
            })
            .collect();
        day_keys.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        for (day_key, days) in day_keys {
            if days_filter.is_some_and(|f| f.parse::<u64>().ok() != Some(days)) {
                continue;
            }
            let Some(results) = by_days.get(day_key).and_then(|v| v.as_object()) else {
                continue;
            };
            let mut rows: Vec<(String, u64)> = results
                .iter()
                .map(|(n, c)| (n.clone(), c.as_u64().unwrap_or(0)))
                .collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            print!(
                "{}",
                analytics_table(&shown_category, &days.to_string(), &rows, width)
            );
        }
    }
}

fn info_analytics(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands_without_flag_values(rest, &["--days", "--category"]);
    if names.is_empty() {
        return not_native(prefix, args, "info --analytics (global form)");
    }
    let (Ok(days_filter), Ok(category_filter)) =
        (flag_value(rest, "--days"), flag_value(rest, "--category"))
    else {
        return not_native(prefix, args, "info --analytics (invalid options)");
    };
    let mut docs: Vec<Option<aterm_json::Value>> = Vec::new();
    for name in &names {
        let short = Formula::short_name(name);
        let doc = if has_flag(rest, "--cask", "") {
            api.cask_value(short).ok()
        } else if has_flag(rest, "--formula", "") {
            match api.formula_value(name) {
                Ok(v) => Some(v),
                Err(_) if tap::lookup(prefix, name).is_some() => None,
                Err(_) => return not_native(prefix, args, "info --analytics (unresolved name)"),
            }
        } else {
            match api.formula_value(name) {
                Ok(v) => Some(v),
                Err(_) => match api.cask_value(short) {
                    Ok(v) => Some(v),
                    Err(_) if tap::lookup(prefix, name).is_some() => None,
                    Err(_) => {
                        return not_native(prefix, args, "info --analytics (unresolved name)");
                    }
                },
            }
        };
        if has_flag(rest, "--cask", "") && doc.is_none() {
            return not_native(prefix, args, "info --analytics (unresolved name)");
        }
        docs.push(doc);
    }
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(80);
    for (i, doc) in docs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let Some(doc) = doc else { continue };
        print_analytics_tables(
            doc,
            days_filter.as_deref(),
            category_filter.as_deref(),
            width,
        );
    }
    ExitCode::SUCCESS
}

/// `info --github`: open each named formula/cask's source on
/// GitHub (the same URL the `From:` line prints).
fn info_github(prefix: &Prefix, api: &Api, rest: &[String], names: &[String]) -> ExitCode {
    if names.is_empty() {
        return super::usage_failed(
            "info",
            "Error: Invalid usage: this command requires a formula or cask argument",
        );
    }
    let mut urls = Vec::new();
    for name in names {
        let short = Formula::short_name(name);
        let resolved = if has_flag(rest, "--cask", "") {
            api.cask(short)
                .map(|c| {
                    format!(
                        "https://github.com/Homebrew/homebrew-cask/blob/HEAD/Casks/{}/{}.rb",
                        cask_shard(&c.token),
                        c.token
                    )
                })
                .map_err(|_| format!("Error: {}", cask_unavailable(short)))
        } else if let Ok(f) = api.formula(name) {
            Ok(formula_source(prefix, &f).0)
        } else if let Some(f) = tap::lookup(prefix, name) {
            Ok(formula_source(prefix, &f).0)
        } else if !has_flag(rest, "--formula", "")
            && let Ok(c) = api.cask(short)
        {
            Ok(format!(
                "https://github.com/Homebrew/homebrew-cask/blob/HEAD/Casks/{}/{}.rb",
                cask_shard(&c.token),
                c.token
            ))
        } else {
            Err(format!("Error: {}", formula_unavailable(api, name)))
        };
        match resolved {
            Ok(url) => urls.push(url),
            Err(message) => return failed(&message),
        }
    }
    open_browser(&urls);
    ExitCode::SUCCESS
}

/// Brew's `info --installed` summary listing: every installed
/// formula (sorted) then every cask, three lines each.
fn info_installed(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    fn reason_suffix(dir: &std::path::Path) -> &'static str {
        match super::prefix::receipt_word(dir, "installed_on_request").as_deref() {
            Some("true") => " (on request)",
            Some("false") => " (dependency)",
            _ => "",
        }
    }
    let mut first = true;
    let mut emit = |lines: [String; 3]| {
        if !first {
            println!();
        }
        first = false;
        for line in lines {
            println!("{line}");
        }
    };
    if !has_flag(rest, "--cask", "") {
        let mut installed = prefix.installed(Kind::Formula);
        installed.sort_by(|a, b| a.name.cmp(&b.name));
        for inst in installed {
            let resolved = api
                .formula(&inst.name)
                .ok()
                .or_else(|| tap::lookup(prefix, &inst.name));
            let receipt_full =
                super::prefix::receipt_word(&prefix.keg(&inst.name, &inst.version), "full_name");
            let (full_name, desc, from) = match &resolved {
                Some(f) => {
                    let from = formula_source(prefix, f)
                        .1
                        .unwrap_or_else(|| "homebrew/core".to_string());
                    (f.full_name.clone(), f.desc.clone(), from)
                }
                None => {
                    let full = receipt_full.unwrap_or_else(|| inst.name.clone());
                    let parts: Vec<&str> = full.split('/').collect();
                    let from = if parts.len() == 3 {
                        format!("{}/{}", parts[0], parts[1])
                    } else {
                        "homebrew/core".to_string()
                    };
                    (full, String::new(), from)
                }
            };
            // Brew's info_summary_title: the installed-status
            // name and description inside one oh1 headline.
            let status = super::tty::pretty_install_status(
                &full_name, true, false, false, false, false, true, false, true,
            );
            let title = if desc.is_empty() {
                super::tty::oh1(&status)
            } else {
                super::tty::oh1(&format!("{status}: {desc}"))
            };
            let mut versions = inst.versions.clone();
            versions.sort_by(|a, b| version_cmp(a, b));
            versions.dedup();
            let version_text = if versions.is_empty() {
                "-".to_string()
            } else {
                versions.join(", ")
            };
            emit([
                title,
                format!("Formula from {from}"),
                format!(
                    "Installed: {version_text}{}",
                    reason_suffix(&prefix.keg(&inst.name, &inst.version))
                ),
            ]);
        }
    }
    if !has_flag(rest, "--formula", "") {
        let mut installed = prefix.installed(Kind::Cask);
        installed.sort_by(|a, b| a.name.cmp(&b.name));
        for inst in installed {
            let token = super::api::canonical_cask_token(&inst.name).to_string();
            let (title_name, desc_part) = match api.cask(&token) {
                Ok(c) => {
                    let desc = if c.desc.is_empty() {
                        String::new()
                    } else if c.name.is_empty() {
                        c.desc.clone()
                    } else {
                        format!("({}) {}", c.name, c.desc)
                    };
                    (token.clone(), desc)
                }
                Err(_) => (token.clone(), String::new()),
            };
            let status = super::tty::pretty_install_status(
                &title_name,
                true,
                false,
                false,
                false,
                false,
                true,
                false,
                true,
            );
            let title = if desc_part.is_empty() {
                super::tty::oh1(&status)
            } else {
                super::tty::oh1(&format!("{status}: {desc_part}"))
            };
            emit([
                title,
                "Cask from homebrew/cask".to_string(),
                format!(
                    "Installed: {}{}",
                    inst.version,
                    reason_suffix(&prefix.cask_dir(&token, &inst.version))
                ),
            ]);
        }
    }
    ExitCode::SUCCESS
}

/// Brew's `info --quiet` three-line formula summary.
fn formula_quiet_lines(prefix: &Prefix, f: &Formula) -> [String; 3] {
    let inst = prefix.installed_one(Kind::Formula, &f.name);
    let status = super::tty::pretty_install_status(
        &f.full_name,
        inst.is_some(),
        false,
        false,
        false,
        false,
        true,
        false,
        true,
    );
    let title = if f.desc.is_empty() {
        super::tty::oh1(&status)
    } else {
        super::tty::oh1(&format!("{status}: {}", f.desc))
    };
    let (from_url, tap_name) = formula_source(prefix, f);
    match &inst {
        None => [
            title,
            format!("Formula from {from_url}"),
            "Not installed".to_string(),
        ],
        Some(i) => {
            let mut versions = i.versions.clone();
            versions.sort_by(|a, b| version_cmp(a, b));
            versions.dedup();
            let version_text = if versions.is_empty() {
                "-".to_string()
            } else {
                versions.join(", ")
            };
            let reason = match super::prefix::receipt_word(
                &prefix.keg(&i.name, &i.version),
                "installed_on_request",
            )
            .as_deref()
            {
                Some("true") => " (on request)",
                Some("false") => " (dependency)",
                _ => "",
            };
            [
                title,
                format!(
                    "Formula from {}",
                    tap_name.unwrap_or_else(|| "homebrew/core".to_string())
                ),
                format!("Installed: {version_text}{reason}"),
            ]
        }
    }
}

/// Brew's `info --quiet` three-line cask summary.
fn cask_quiet_lines(prefix: &Prefix, c: &super::formula::Cask) -> [String; 3] {
    let inst = prefix.installed_one(Kind::Cask, &c.token);
    let status = super::tty::pretty_install_status(
        &c.token,
        inst.is_some(),
        false,
        false,
        false,
        false,
        true,
        false,
        true,
    );
    let desc_part = if c.desc.is_empty() {
        String::new()
    } else if c.name.is_empty() {
        c.desc.clone()
    } else {
        format!("({}) {}", c.name, c.desc)
    };
    let title = if desc_part.is_empty() {
        super::tty::oh1(&status)
    } else {
        super::tty::oh1(&format!("{status}: {desc_part}"))
    };
    match &inst {
        None => [
            title,
            format!(
                "Cask from {}",
                super::tty::url(&format!(
                    "https://github.com/Homebrew/homebrew-cask/blob/HEAD/Casks/{}/{}.rb",
                    cask_shard(&c.token),
                    c.token
                ))
            ),
            "Not installed".to_string(),
        ],
        Some(i) => {
            let reason = match super::prefix::receipt_word(
                &prefix.cask_dir(&i.name, &i.version),
                "installed_on_request",
            )
            .as_deref()
            {
                Some("true") => " (on request)",
                Some("false") => " (dependency)",
                _ => "",
            };
            [
                title,
                "Cask from homebrew/cask".to_string(),
                format!("Installed: {}{reason}", i.version),
            ]
        }
    }
}

pub(super) fn cmd_info(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let verbose = has_flag(rest, "--verbose", "-v");
    let quiet = has_flag(rest, "--quiet", "-q");
    if has_flag(rest, "--analytics", "") {
        return info_analytics(prefix, api, args, rest);
    }
    let names = operands(rest);
    if rest
        .iter()
        .any(|a| matches!(a.as_str(), "--json" | "--json=v1" | "--json=v2"))
    {
        if names.is_empty() && !has_flag(rest, "--installed", "") {
            return usage("info --json needs a name or --installed");
        }
        return match info_json(prefix, api, rest, &names) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => failed(&e),
        };
    }
    if has_flag(rest, "--installed", "") {
        return info_installed(prefix, api, rest);
    }
    if has_flag(rest, "--github", "") {
        return info_github(prefix, api, rest, &names);
    }
    let _ = args;
    if names.is_empty() {
        println!(
            "{} formulae, {} casks installed under {}",
            prefix.installed(Kind::Formula).len(),
            prefix.installed(Kind::Cask).len(),
            prefix.root.display()
        );
        return ExitCode::SUCCESS;
    }
    let mut failures = 0;
    let mut printed = false;
    for (idx, name) in names.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        if has_flag(rest, "--cask", "") {
            match api.cask(name) {
                Ok(c) => {
                    if quiet {
                        for line in cask_quiet_lines(prefix, &c) {
                            println!("{line}");
                        }
                    } else {
                        print_cask_info(prefix, api, &c, verbose);
                    }
                    printed = true;
                }
                Err(_) => {
                    super::tty::onoe!("Error: {}", cask_unavailable(Formula::short_name(name)));
                    failures += 1;
                }
            }
            continue;
        }
        match api.formula(name) {
            Ok(f) => {
                if quiet {
                    for line in formula_quiet_lines(prefix, &f) {
                        println!("{line}");
                    }
                } else {
                    print_formula_info(prefix, api, &f, verbose);
                }
                printed = true;
            }
            Err(_) => match tap::lookup(prefix, name) {
                Some(f) => {
                    if quiet {
                        for line in formula_quiet_lines(prefix, &f) {
                            println!("{line}");
                        }
                    } else {
                        print_formula_info(prefix, api, &f, verbose);
                    }
                    printed = true;
                }
                None => {
                    if has_flag(rest, "--formula", "") {
                        super::tty::onoe!("Error: {}", formula_unavailable(api, name));
                        failures += 1;
                        continue;
                    }
                    if name.contains('/') {
                        if !printed {
                            return not_native(prefix, args, "info (unresolved name)");
                        }
                        super::tty::onoe!("Error: {}", formula_unavailable(api, name));
                        failures += 1;
                        continue;
                    }
                    match api.cask(name) {
                        Ok(c) => {
                            if quiet {
                                for line in cask_quiet_lines(prefix, &c) {
                                    println!("{line}");
                                }
                            } else {
                                print_cask_info(prefix, api, &c, verbose);
                            }
                            printed = true;
                        }
                        Err(_) => {
                            super::tty::onoe!("Error: {}", formula_unavailable(api, name));
                            failures += 1;
                        }
                    }
                }
            },
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn artifact_line(a: &super::formula::Artifact) -> Option<String> {
    use super::formula::Artifact;
    // Brew's CommandWrapper source is the generated script at
    // `.homebrew-command-wrappers/<name>`, always with an
    // explicit target.
    if let Artifact::Wrapper(name, _, _, dest) = a {
        return Some(format!(
            ".homebrew-command-wrappers/{name} -> {} (Command Wrapper)",
            dest.as_deref().unwrap_or(name)
        ));
    }
    let (source, target, label): (&str, Option<&str>, &str) = match a {
        Artifact::App(s, t) => (s, t.as_deref(), "App"),
        Artifact::Binary(s, t) => (s, t.as_deref(), "Binary"),
        Artifact::Font(s, t) => (s, t.as_deref(), "Font"),
        Artifact::Pkg(s) => (s, None, "Pkg"),
        Artifact::Manpage(s, t) => (s, t.as_deref(), "Manpage"),
        Artifact::Suite(s, t) => (s, t.as_deref(), "App Suite"),
        Artifact::KeyboardLayout(s, t) => (s, t.as_deref(), "Keyboard Layout"),
        Artifact::ScreenSaver(s, t) => (s, t.as_deref(), "Screen Saver"),
        Artifact::PrefPane(s, t) => (s, t.as_deref(), "Preference Pane"),
        Artifact::Service(s, t) => (s, t.as_deref(), "Service"),
        Artifact::Generic(s, t) => (s, t.as_deref(), "Generic Artifact"),
        Artifact::Wrapper(name, _, _, t) => (name, t.as_deref(), "Command Wrapper"),
        Artifact::Library(s, t, dir) => (s, t.as_deref(), library_label(dir)),
        Artifact::Other(_) => return None,
    };
    let basename = source.rsplit('/').next().unwrap_or(source);
    let shown = match target {
        Some(t) if matches!(a, Artifact::Generic(..)) || t != basename => {
            format!("{source} -> {t}")
        }
        _ => source.to_string(),
    };
    Some(format!("{shown} ({label})"))
}

fn library_label(dir: &str) -> &'static str {
    match dir {
        "Library/QuickLook" => "Quick Look Plugin",
        "Library/ColorPickers" => "Colorpicker",
        "Library/Dictionaries" => "Dictionary",
        "Library/Input Methods" => "Input Method",
        "Library/Internet Plug-Ins" => "Internet Plugin",
        "Library/Spotlight" => "Spotlight metadata importer",
        "Library/Audio/Plug-Ins/Components" => "Audio Unit Plugin",
        "Library/Audio/Plug-Ins/VST" => "VST Plugin",
        "Library/Audio/Plug-Ins/VST3" => "VST3 Plugin",
        "share/bash-completion/completions" => "Bash Completion",
        "share/zsh/site-functions" => "Zsh Completion",
        "share/fish/vendor_completions.d" => "Fish Completion",
        _ => "Artifact",
    }
}

fn print_cask_info(prefix: &Prefix, api: &Api, c: &super::formula::Cask, verbose: bool) {
    let inst = prefix.installed_one(Kind::Cask, &c.token);
    let outdated = !c.auto_updates
        && inst
            .as_ref()
            .is_some_and(|i| version_cmp(&i.version, &c.version) == Ordering::Less);
    let version = if outdated {
        format!(
            "{} → {}",
            inst.as_ref().map(|i| i.version.clone()).unwrap_or_default(),
            c.version
        )
    } else {
        c.version.clone()
    };
    let status_token = super::tty::pretty_install_status(
        &c.token,
        inst.is_some(),
        outdated,
        c.deprecated,
        c.disabled,
        false,
        cfg!(target_os = "macos") && !c.disabled,
        false,
        true,
    );
    println!(
        "{}{}: {version}{}",
        super::tty::oh1(&status_token),
        if c.name.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.name)
        },
        if c.auto_updates {
            " (auto_updates)"
        } else {
            ""
        }
    );
    if !c.desc.is_empty() {
        println!("{}", c.desc);
    }
    if !c.homepage.is_empty() {
        println!("{}", super::tty::url(&c.homepage));
    }
    if let Some(message) =
        deprecate_disable_message(true, c.deprecated, c.disabled, &c.deprecate_disable)
    {
        println!("{message}");
    }
    match &inst {
        Some(i) => {
            let dir = prefix.cask_dir(&c.token, &i.version);
            let on_request = super::prefix::receipt_word(&dir, "installed_on_request").as_deref()
                == Some("true");
            println!(
                "Installed ({})",
                if on_request {
                    "on request"
                } else {
                    "as dependency"
                }
            );
            println!(
                "{} ({})",
                dir.display(),
                disk_usage_readable(dir_disk_usage(&dir))
            );
            let when = super::prefix::receipt_word(&dir, "time")
                .and_then(|t| t.parse::<i64>().ok())
                .map(|secs| format!(" on {}", format_epoch_local(secs)))
                .unwrap_or_default();
            println!("  Installed using the formulae.brew.sh API{when}");
        }
        None => println!("Not installed"),
    }
    print_metadata_lines(prefix, &c.token, true, inst.is_some(), false);
    println!(
        "From: {}",
        super::tty::url(&format!(
            "https://github.com/Homebrew/homebrew-cask/blob/HEAD/Casks/{}/{}.rb",
            cask_shard(&c.token),
            c.token
        ))
    );
    if !c.depends_formula.is_empty() || !c.depends_cask.is_empty() {
        let mut all: Vec<String> = c.depends_formula.clone();
        all.extend(c.depends_cask.iter().map(|t| format!("{t} (cask)")));
        println!("{}", super::tty::ohai("Dependencies"));
        println!("Required ({}): {}", all.len(), all.join(", "));
        // Brew's recursive set: direct deps plus, for installed
        // formula deps only, their tab runtime closure; cask
        // deps recurse into their own deps. Abrew receipts
        // record no tab closure, so an installed dep's declared
        // graph closure stands in for its tab.
        fn collect_cask_recursive(
            prefix: &Prefix,
            api: &Api,
            cask: &super::formula::Cask,
            visited: &mut std::collections::BTreeSet<String>,
            out: &mut Vec<(String, bool)>,
        ) {
            for dep in &cask.depends_formula {
                let installed = prefix.installed_one(Kind::Formula, dep).is_some();
                out.push((dep.clone(), installed));
                if installed && let Ok(df) = api.formula(dep) {
                    for sub in flat_dependencies(&df, false, |n| api.formula(n).ok()) {
                        out.push((
                            sub.clone(),
                            prefix.installed_one(Kind::Formula, &sub).is_some(),
                        ));
                    }
                }
            }
            for dep in &cask.depends_cask {
                if !visited.insert(dep.clone()) {
                    continue;
                }
                out.push((dep.clone(), prefix.installed_one(Kind::Cask, dep).is_some()));
                if let Ok(dep_cask) = api.cask(dep) {
                    collect_cask_recursive(prefix, api, &dep_cask, visited, out);
                }
            }
        }
        let mut recursive: Vec<(String, bool)> = Vec::new();
        let mut visited: std::collections::BTreeSet<String> = [c.token.clone()].into();
        collect_cask_recursive(prefix, api, c, &mut visited, &mut recursive);
        recursive.sort();
        recursive.dedup();
        if !recursive.is_empty() {
            let installed_count = recursive.iter().filter(|(_, ok)| *ok).count();
            let counts = if installed_count == recursive.len() {
                format!("all installed {}", super::tty::success("✔"))
            } else {
                format!(
                    "{installed_count} installed {}, {} missing {}",
                    super::tty::success("✔"),
                    recursive.len() - installed_count,
                    super::tty::error("✘")
                )
            };
            println!("Recursive Runtime ({}): {counts}", recursive.len());
        }
    }
    let raw_doc = api.cask_value(&c.token).ok();
    let mut required: Vec<String> = raw_doc
        .as_ref()
        .map(super::formula::cask_info_requirement_displays)
        .unwrap_or_default();
    if required.is_empty() {
        if let Some(arch) = &c.arch {
            required.push(format!("{arch} architecture"));
        }
        if let Some(min) = &c.macos_min {
            required.push(format!("macOS >= {min}"));
        }
        if let Some(exact) = &c.macos_exact {
            required.push(format!("macOS == {exact}"));
        }
    }
    if !required.is_empty() {
        println!("{}", super::tty::ohai("Requirements"));
        let decorated: Vec<String> = required
            .iter()
            .map(|d| {
                let satisfied = requirement_satisfied(prefix, d);
                super::tty::pretty_install_status(
                    d, satisfied, false, false, false, false, true, true, satisfied,
                )
            })
            .collect();
        println!("Required: {}", decorated.join(", "));
    }
    if !c.languages.is_empty() {
        println!("{}", super::tty::ohai("Languages"));
        println!("{}", c.languages.join(", "));
    }
    let artifacts: Vec<String> = c.artifacts.iter().filter_map(artifact_line).collect();
    if !artifacts.is_empty() {
        println!("{}", super::tty::ohai("Artifacts"));
        for line in artifacts {
            println!("{line}");
        }
    }
    if !c.caveats.is_empty() {
        // Brew's heredoc leaves a blank line after the caveats.
        let text = expand_caveats(prefix, &c.caveats);
        println!("{}", super::tty::ohai("Caveats"));
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        println!();
    }
    print_analytics_for(verbose, raw_doc.as_ref());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DescField {
    Name,
    Description,
    Either,
}

fn desc_matches(
    query: &SearchQuery,
    field: DescField,
    cask: bool,
    name: &str,
    display: &str,
    desc: &str,
) -> Result<bool, aterm_regex::StepLimitExceeded> {
    let fields: &[&str] = match (field, cask) {
        (DescField::Name, _) => &[name],
        (DescField::Description, false) => &[desc],
        (DescField::Description, true) => &[display, desc],
        (DescField::Either, false) => &[name, desc],
        (DescField::Either, true) => &[name, display, desc],
    };
    query.hits_any(fields)
}

fn desc_search(prefix: &Prefix, api: &Api, rest: &[String], field: DescField) -> ExitCode {
    let joined = operands(rest).join(" ");
    if joined.is_empty() {
        return usage("desc search needs a term");
    }
    let query = match search_regex_pattern(&joined) {
        Some(pattern) => match compile_search_regex(pattern) {
            Ok(re) => SearchQuery::Regex(re),
            Err(_) => return failed(&format!("abrew: {joined} is not a valid regex.")),
        },
        None => SearchQuery::text(super::api::canonical_cask_token(&joined)),
    };
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    // Brew's description search shows both sections when both
    // switches are given, but the separating blank line only
    // when neither was.
    let both = !want_cask && !want_formula;
    for casks in [false, true] {
        if (casks && want_formula && !want_cask) || (!casks && want_cask && !want_formula) {
            continue;
        }
        let rows = match api.desc_index(casks) {
            Ok(rows) => rows,
            Err(e) => return failed(&e),
        };
        let mut hits: Vec<(String, String, String)> = Vec::new();
        for (name, display, desc) in rows {
            if desc.is_empty() {
                continue;
            }
            match desc_matches(&query, field, casks, &name, &display, &desc) {
                Ok(true) => hits.push((name, display, desc)),
                Ok(false) => {}
                Err(_) => {
                    return failed(
                        "abrew: native regex search ran out of its work budget; simplify the pattern",
                    );
                }
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        if casks {
            if both {
                println!();
            }
            println!("{}", super::tty::ohai("Casks"));
        } else {
            println!("{}", super::tty::ohai("Formulae"));
        }
        for (name, display, desc) in hits {
            let shown = desc_display_name(prefix, api, &name, casks);
            if casks && !display.is_empty() {
                println!("{shown}: ({display}) {desc}");
            } else {
                println!("{shown}: {desc}");
            }
        }
    }
    ExitCode::SUCCESS
}

/// A description-listing name with brew's terminal decoration
/// (plain off a terminal, where no status lookup happens).
fn desc_display_name(prefix: &Prefix, api: &Api, name: &str, cask: bool) -> String {
    if !super::tty::stdout_tty() {
        return name.to_string();
    }
    let (installed, deprecated, disabled) = if cask {
        let inst = prefix.installed_one(Kind::Cask, name).is_some();
        match api.cask(name) {
            Ok(c) => (inst, c.deprecated, c.disabled),
            Err(_) => (inst, false, false),
        }
    } else {
        let inst = prefix.installed_one(Kind::Formula, name).is_some();
        match api.formula(name) {
            Ok(f) => (inst, f.deprecate, f.disable),
            Err(_) => (inst, false, false),
        }
    };
    super::tty::pretty_install_status(
        name, installed, false, deprecated, disabled, false, true, false, true,
    )
}

pub(super) fn cmd_desc(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let field = if has_flag(rest, "--search", "-s") {
        Some(DescField::Either)
    } else if has_flag(rest, "--name", "-n") {
        Some(DescField::Name)
    } else if has_flag(rest, "--description", "-d") {
        Some(DescField::Description)
    } else {
        None
    };
    if let Some(field) = field {
        return desc_search(prefix, api, rest, field);
    }
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    // Brew's only_formula_or_cask: both switches mean either kind.
    let (want_cask, want_formula) = if want_cask && want_formula {
        (false, false)
    } else {
        (want_cask, want_formula)
    };
    // Brew collects named descriptions into a hash keyed by full
    // name and prints it sorted (Descriptions#print), so operand
    // order and duplicates collapse.
    let mut lines: Vec<(String, String)> = Vec::new();
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !want_cask {
            if let Ok(f) = api.formula(&name) {
                let shown = super::tty::pretty_install_status(
                    &f.name,
                    prefix.installed_one(Kind::Formula, &f.name).is_some(),
                    false,
                    f.deprecate,
                    f.disable,
                    false,
                    true,
                    false,
                    true,
                );
                lines.push((f.name.clone(), format!("{shown}: {}", f.desc)));
                continue;
            }
            if want_formula {
                return failed(&format!("Error: {}", formula_unavailable(api, &name)));
            }
        }
        if !want_formula {
            if let Ok(c) = api.cask(short) {
                let shown = super::tty::pretty_install_status(
                    &c.token,
                    prefix.installed_one(Kind::Cask, &c.token).is_some(),
                    false,
                    c.deprecated,
                    c.disabled,
                    false,
                    true,
                    false,
                    true,
                );
                lines.push((
                    c.token.clone(),
                    if c.name.is_empty() {
                        format!("{shown}: {}", c.desc)
                    } else {
                        format!("{shown}: ({}) {}", c.name, c.desc)
                    },
                ));
                continue;
            }
            if want_cask {
                return failed(&format!("Error: {}", cask_unavailable(short)));
            }
        }
        return failed(&format!("Error: {}", formula_unavailable(api, &name)));
    }
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    lines.dedup_by(|a, b| a.0 == b.0);
    for (_, line) in lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_home(api: &Api, rest: &[String]) -> ExitCode {
    // Admission delegates the --formula/--cask conflict, as for
    // every other command; both cannot reach here together.
    let want_formula = has_flag(rest, "--formula", "") || has_flag(rest, "--formulae", "");
    let want_cask = has_flag(rest, "--cask", "") || has_flag(rest, "--casks", "");
    let names = operands(rest);
    if names.is_empty() {
        open_browser(&["https://brew.sh".to_string()]);
        return ExitCode::SUCCESS;
    }
    let mut urls = Vec::new();
    for name in &names {
        let short = Formula::short_name(name);
        let resolved = if want_cask {
            api.cask(short)
                .map(|c| (format!("Cask {}", c.token), c.homepage))
                .map_err(|_| format!("Error: {}", cask_unavailable(short)))
        } else if want_formula {
            api.formula(name)
                .map(|f| (format!("Formula {}", f.name), f.homepage))
                .map_err(|_| format!("Error: {}", formula_unavailable(api, name)))
        } else if let Ok(f) = api.formula(name) {
            Ok((format!("Formula {}", f.name), f.homepage))
        } else if let Ok(c) = api.cask(short) {
            Ok((format!("Cask {}", c.token), c.homepage))
        } else {
            Err(format!("Error: {}", formula_unavailable(api, name)))
        };
        match resolved {
            Ok((label, homepage)) => {
                println!("Opening homepage for {label}");
                urls.push(homepage);
            }
            Err(message) => return failed(&message),
        }
    }
    open_browser(&urls);
    ExitCode::SUCCESS
}

fn core_shard(short: &str) -> String {
    if short.starts_with("lib") {
        "lib".to_string()
    } else {
        short.chars().next().unwrap_or('x').to_string()
    }
}

fn cask_shard(token: &str) -> String {
    if let Some(rest) = token.strip_prefix("font-") {
        format!("font/font-{}", rest.chars().next().unwrap_or('x'))
    } else {
        token.chars().next().unwrap_or('x').to_string()
    }
}

pub(super) fn cmd_cat(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let mut failures = 0;
    for name in operands(rest) {
        if cat_one(prefix, api, rest, &name) != ExitCode::SUCCESS {
            failures += 1;
        }
    }
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cat_one(prefix: &Prefix, api: &Api, rest: &[String], name: &str) -> ExitCode {
    let name = name.to_string();
    if has_flag(rest, "--cask", "") {
        let short = super::api::canonical_cask_token(Formula::short_name(&name));
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3
            && let (Some(u), Some(r)) = (parts.first(), parts.get(1))
            && !(*u == "homebrew" && *r == "cask")
        {
            let root = prefix.taps_dir().join(u).join(format!("homebrew-{r}"));
            for cand in [
                root.join("Casks").join(format!("{short}.rb")),
                root.join("Casks")
                    .join(cask_shard(short))
                    .join(format!("{short}.rb")),
            ] {
                if let Ok(text) = std::fs::read_to_string(&cand) {
                    print!("{text}");
                    return ExitCode::SUCCESS;
                }
            }
            return failed(&format!("abrew: no cask file for {name} in the local tap"));
        }
        if api.cask(short).is_err() {
            return failed(&format!(
                "Error: {short}'s source doesn't exist on disk.\nThe name may be wrong, or the tap hasn't been tapped. Instead try:\n  brew info --github --cask {short}"
            ));
        }
        let url = format!(
            "https://raw.githubusercontent.com/Homebrew/homebrew-cask/HEAD/Casks/{}/{short}.rb",
            cask_shard(short)
        );
        return match fetch::curl_get(&url, &[]) {
            Ok(bytes) => {
                print!("{}", String::from_utf8_lossy(&bytes));
                ExitCode::SUCCESS
            }
            Err(e) => failed(&e),
        };
    }
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() == 3
        && let (Some(u), Some(r), Some(n)) = (parts.first(), parts.get(1), parts.get(2))
    {
        let t = tap::Tap {
            user: (*u).to_string(),
            repo: (*r).to_string(),
            path: prefix.taps_dir().join(u).join(format!("homebrew-{r}")),
        };
        if let Some(file) = tap::formula_file(&t, n) {
            return match std::fs::read_to_string(&file) {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => failed(&format!("abrew: {e}")),
            };
        }
    }
    let short = Formula::short_name(&name);
    if api.formula(&name).is_err() && tap::lookup(prefix, &name).is_none() {
        return failed(&format!(
            "Error: {}/{name}'s source doesn't exist on disk.\nThe name may be wrong, or the tap hasn't been tapped. Instead try:\n  brew info --github {name}",
            prefix.root.display()
        ));
    }
    let url = format!(
        "https://raw.githubusercontent.com/Homebrew/homebrew-core/HEAD/Formula/{}/{short}.rb",
        core_shard(short)
    );
    match fetch::curl_get(&url, &[]) {
        Ok(bytes) => {
            // Brew's API packages carry the formula source with
            // its bottle block removed (bottle data travels in
            // the JSON instead); show the same text.
            print!("{}", strip_bottle_block(&String::from_utf8_lossy(&bytes)));
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

/// Remove a formula's top-level `bottle do ... end` block (and
/// one blank line after it), as brew's API source presents it.
/// A malformed or absent block leaves the text untouched.
fn strip_bottle_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == "bottle do") else {
        return text.to_string();
    };
    let indent = &lines[start][..lines[start].len() - lines[start].trim_start().len()];
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim_end() == format!("{indent}end"))
        .map(|offset| start + 1 + offset);
    let Some(end) = end else {
        return text.to_string();
    };
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    kept.extend_from_slice(&lines[..start]);
    let rest = &lines[end + 1..];
    let rest = if rest.first().is_some_and(|line| line.is_empty()) {
        &rest[1..]
    } else {
        rest
    };
    kept.extend_from_slice(rest);
    let mut out = kept.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// `brew --prefix/--cellar/--caskroom <name>`: validate package existence and print its path.
pub(super) fn cmd_prefix_query(prefix: &Prefix, api: &Api, cmd: &str, name: &str) -> ExitCode {
    let short = Formula::short_name(name);
    if cmd == "--caskroom" {
        return match api.cask(short) {
            Ok(c) => {
                println!("{}", prefix.caskroom().join(&c.token).display());
                ExitCode::SUCCESS
            }
            Err(_) => failed(&format!("Error: {}", cask_unavailable(short))),
        };
    }
    let resolved = api.formula(name).ok().or_else(|| tap::lookup(prefix, name));
    match resolved {
        Some(_) => {
            if cmd == "--prefix" {
                println!("{}", prefix.opt(short).display());
            } else {
                println!("{}", prefix.cellar().join(short).display());
            }
            ExitCode::SUCCESS
        }
        None => failed(&format!("Error: {}", formula_unavailable(api, name))),
    }
}

/// `brew --cache <name>…`: print the cached download path for each resolved package.
pub(super) fn cmd_cache_query(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let mut paths = Vec::new();
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !want_cask {
            let resolved = api
                .formula(&name)
                .ok()
                .or_else(|| tap::lookup(prefix, &name));
            match resolved {
                Some(f) => {
                    if f.bottle_for(&super::machine::Machine::this().bottle_tags())
                        .is_none()
                    {
                        return from_install(
                            prefix,
                            args,
                            Err(install::InstallError::Unsupported(format!(
                                "no bottle for {} on this platform",
                                f.name
                            ))),
                        );
                    }
                    paths.push(prefix.cache().join(format!(
                        "{}-{}.bottle.tar.gz",
                        f.name,
                        install::keg_version(&f)
                    )));
                    continue;
                }
                None if want_formula => {
                    return failed(&format!("Error: {}", formula_unavailable(api, &name)));
                }
                None => {}
            }
        }
        match api.cask(short) {
            Ok(c) => {
                let c = match cask::resolve(&c, super::machine::Machine::this()) {
                    Ok(c) => c.into_owned(),
                    Err(e) => return from_install(prefix, args, Err(e)),
                };
                paths.push(prefix.cache().join(cask::download_name(&c)));
            }
            Err(e) => {
                return failed(&if want_cask {
                    format!("Error: {}", cask_unavailable(short))
                } else if e.starts_with("abrew: no such ") {
                    format!("Error: {}", formula_unavailable(api, &name))
                } else {
                    e
                });
            }
        }
    }
    for p in paths {
        println!("{}", p.display());
    }
    ExitCode::SUCCESS
}

pub(super) fn list_index(api: &Api, casks: bool) -> ExitCode {
    match api.index(casks) {
        Ok(rows) => {
            for (name, _) in rows {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

pub(super) fn cmd_config(prefix: &Prefix) -> ExitCode {
    println!("ABREW_VERSION: {} (aterm)", env!("CARGO_PKG_VERSION"));
    println!("ABREW_PREFIX: {}", prefix.root.display());
    println!("ABREW_CELLAR: {}", prefix.cellar().display());
    println!("ABREW_CASKROOM: {}", prefix.caskroom().display());
    println!("CPU: {}", std::env::consts::ARCH);
    println!("OS: {}", std::env::consts::OS);
    println!("IMPLEMENTATION: atpkg native (no Homebrew fallback)");
    ExitCode::SUCCESS
}

fn shellenv(prefix: &Prefix) -> String {
    let root = prefix.root.to_string_lossy();
    let quote = crate::platform::sh_quote_str;
    format!(
        "export ABREW_PREFIX={};\nexport PATH={}\"${{PATH+:$PATH}}\";\nexport MANPATH={}\"${{MANPATH+:$MANPATH}}:\";\nexport INFOPATH={}\"${{INFOPATH:-}}\";\n",
        quote(&root),
        quote(&format!("{root}/bin:{root}/sbin")),
        quote(&format!("{root}/share/man")),
        quote(&format!("{root}/share/info:"))
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellKind {
    Posix,
    Fish,
    Csh,
    Pwsh,
}

fn shell_kind(name: &str) -> ShellKind {
    let base = name.rsplit('/').next().unwrap_or(name);
    let base = base.strip_prefix('-').unwrap_or(base);
    match base {
        "fish" => ShellKind::Fish,
        "csh" | "tcsh" => ShellKind::Csh,
        "pwsh" | "pwsh-preview" | "powershell" => ShellKind::Pwsh,
        _ => ShellKind::Posix,
    }
}

pub(super) fn cmd_shellenv(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let shell = operands(rest)
        .into_iter()
        .next()
        .or_else(crate::caller_shell::invoking_shell)
        .unwrap_or_default();
    let root = prefix.root.to_string_lossy();
    let text = match shell_kind(&shell) {
        ShellKind::Posix => shellenv(prefix),
        ShellKind::Fish => {
            let q = |s: &str| {
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
            };
            format!(
                "set -gx ABREW_PREFIX \"{}\";\nset -gx PATH \"{}/bin\" \"{}/sbin\" $PATH;\nset -gx MANPATH \"{}/share/man\" $MANPATH;\nset -gx INFOPATH \"{}/share/info\" $INFOPATH;\n",
                q(&root),
                q(&root),
                q(&root),
                q(&root),
                q(&root)
            )
        }
        ShellKind::Csh => {
            let quote = crate::platform::sh_quote_str;
            let man = format!("{root}/share/man");
            let info = format!("{root}/share/info");
            format!(
                "setenv ABREW_PREFIX {};\nsetenv PATH {}:\"$PATH\";\nif ($?MANPATH) then\nsetenv MANPATH {}:\"$MANPATH\"\nelse\nsetenv MANPATH {}\nendif\nif ($?INFOPATH) then\nsetenv INFOPATH {}:\"$INFOPATH\"\nelse\nsetenv INFOPATH {}\nendif\n",
                quote(&root),
                quote(&format!("{root}/bin:{root}/sbin")),
                quote(&man),
                quote(&format!("{man}:")),
                quote(&info),
                quote(&format!("{info}:")),
            )
        }
        ShellKind::Pwsh => {
            let q = |s: &str| s.replace('\'', "''");
            let set = |name: &str, value: String| {
                format!(
                    "[System.Environment]::SetEnvironmentVariable('{name}',{value},[System.EnvironmentVariableTarget]::Process)\n"
                )
            };
            format!(
                "{}{}{}{}",
                set("ABREW_PREFIX", format!("'{}'", q(&root))),
                set(
                    "PATH",
                    format!("('{}/bin:{}/sbin:'+$ENV:PATH)", q(&root), q(&root))
                ),
                set(
                    "MANPATH",
                    format!("('{}/share/man:'+${{ENV:MANPATH}})", q(&root))
                ),
                set(
                    "INFOPATH",
                    format!("('{}/share/info:'+${{ENV:INFOPATH}})", q(&root))
                ),
            )
        }
    };
    print!("{text}");
    ExitCode::SUCCESS
}

fn unlinked_keg(prefix: &Prefix, i: &super::prefix::Installed) -> bool {
    keg_bin_linked(prefix, &i.name, &i.version) == Some(false)
}

fn keg_bin_linked(prefix: &Prefix, name: &str, version: &str) -> Option<bool> {
    let keg = prefix.keg(name, version);
    if super::prefix::receipt_word(&keg, "keg_only").as_deref() != Some("false") {
        return None;
    }
    let entries = std::fs::read_dir(keg.join("bin"))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if entries.is_empty() {
        return None;
    }
    let mut unknown = false;
    for entry in entries {
        let name = entry.file_name();
        match super::install::link_points_to(
            &prefix.bin().join(&name),
            &keg.join("bin").join(&name),
        ) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => unknown = true,
        }
    }
    if unknown { None } else { Some(false) }
}

fn linked_keg_version(prefix: &Prefix, i: &Installed) -> Option<String> {
    let mut linked: Option<String> = None;
    for version in &i.versions {
        if keg_bin_linked(prefix, &i.name, version) == Some(true) {
            if linked
                .as_deref()
                .is_some_and(|prior| prior != version.as_str())
            {
                return None;
            }
            linked = Some(version.clone());
        }
    }
    linked
}

/// Doctor checks abrew can assert honestly about its own prefix,
/// under brew's check names where the check is brew's. The rest of
/// brew's ~45 diagnostics are facts about brew's checkout, its
/// build toolchain or the OS frameworks — not ours to report.
const DOCTOR_CHECKS: &[&str] = &[
    "check_access_directories",
    "check_for_broken_symlinks",
    "check_for_unlinked_but_not_keg_only",
    "check_missing_deps",
    "check_deprecated_disabled",
    "check_cask_deprecated_disabled",
    "check_deleted_formula",
    "check_install_receipts",
    "check_dangling_pins",
    "check_prefix_on_path",
];

fn indented(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("\n  {item}"))
        .collect::<Vec<_>>()
        .join("")
}

pub(super) fn cmd_doctor(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let quiet = has_flag(rest, "--quiet", "-q");
    if has_flag(rest, "--list-checks", "") {
        for check in DOCTOR_CHECKS {
            println!("{check}");
        }
        return ExitCode::SUCCESS;
    }
    let named = operands(rest);
    let wanted: Vec<&str> = if named.is_empty() {
        DOCTOR_CHECKS.to_vec()
    } else {
        named.iter().map(String::as_str).collect()
    };

    let formulae = prefix.installed(Kind::Formula);
    let casks = prefix.installed(Kind::Cask);
    // Index rows feed the deprecated/deleted checks; offline,
    // those checks simply have nothing to say.
    let formula_rows = api.search_index(false).ok();
    let cask_rows = api.search_index(true).ok();
    let index_flags = |kind: Kind, name: &str| -> Option<bool> {
        let rows = (match kind {
            Kind::Formula => formula_rows.as_ref(),
            Kind::Cask => cask_rows.as_ref(),
        })?;
        let row = rows
            .iter()
            .find(|row| row.name == name || row.aliases.iter().any(|a| a == name))?;
        Some(row.deprecated || row.disabled)
    };

    let mut findings: Vec<String> = Vec::new();
    let mut unknown_check = false;
    for check in wanted {
        match check {
            "check_access_directories" => {
                if prefix.ensure().is_err() {
                    findings.push(format!(
                        "prefix {} cannot be created/written",
                        prefix.root.display()
                    ));
                    continue;
                }
                let mut not_writable: Vec<String> = Vec::new();
                for sub in [
                    "etc/bash_completion.d",
                    "lib/pkgconfig",
                    "share/aclocal",
                    "share/doc",
                    "share/info",
                    "share/locale",
                    "share/man",
                    "share/zsh",
                    "share/zsh/site-functions",
                    "share/fish",
                    "var/log",
                    "bin",
                    "Cellar",
                    "Caskroom",
                ] {
                    let dir = prefix.root.join(sub);
                    if dir.is_dir() && !crate::platform::dir_writable_by_caller(&dir) {
                        not_writable.push(dir.display().to_string());
                    }
                }
                if !not_writable.is_empty() {
                    let user = std::env::var("USER").unwrap_or_default();
                    let joined = not_writable.join(" ");
                    findings.push(format!(
                        "The following directories are not writable by your user:{}\n\
                         You should change the ownership of these directories to your user,\n\
                         and make sure that you have write permission.\n  \
                         sudo chown -R {user} {joined}\n  chmod u+w {joined}",
                        indented(&not_writable)
                    ));
                }
            }
            "check_for_broken_symlinks" => {
                let broken: Vec<String> = broken_prefix_links(prefix)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                if !broken.is_empty() {
                    findings.push(format!(
                        "Broken symlinks were found:{}\nRemove them with `brew cleanup`",
                        indented(&broken)
                    ));
                }
            }
            "check_for_unlinked_but_not_keg_only" => {
                let unlinked: Vec<String> = formulae
                    .iter()
                    .filter(|i| unlinked_keg(prefix, i))
                    .map(|i| i.name.clone())
                    .collect();
                if !unlinked.is_empty() {
                    findings.push(format!(
                        "You have unlinked kegs in your Cellar.\n\
                         Leaving kegs unlinked can lead to build-trouble and cause formulae that depend on\n\
                         those kegs to fail to run properly once built.\n\
                         Run `brew link` on these:{}",
                        indented(&unlinked)
                    ));
                }
            }
            "check_missing_deps" => {
                let mut lines: Vec<String> = Vec::new();
                let mut all_missing: Vec<String> = Vec::new();
                for i in &formulae {
                    let Ok(f) = api.formula(&i.name).or_else(|_| {
                        tap::lookup(prefix, &i.name).ok_or_else(|| "unloadable".to_string())
                    }) else {
                        continue;
                    };
                    let missing: Vec<String> =
                        flat_dependencies(&f, false, |n| api.formula(n).ok())
                            .into_iter()
                            .map(|d| Formula::short_name(&d).to_string())
                            .filter(|d| prefix.installed_one(Kind::Formula, d).is_none())
                            .collect();
                    if !missing.is_empty() {
                        lines.push(format!("{}: {}", i.name, missing.join(" ")));
                        all_missing.extend(missing);
                    }
                }
                for i in &casks {
                    let Ok(c) = api.cask(&i.name) else { continue };
                    let mut missing: Vec<String> = Vec::new();
                    for d in &c.depends_formula {
                        let short = Formula::short_name(d);
                        if prefix.installed_one(Kind::Formula, short).is_none() {
                            missing.push(short.to_string());
                        }
                    }
                    for d in &c.depends_cask {
                        if prefix.installed_one(Kind::Cask, d).is_none() {
                            missing.push(d.clone());
                        }
                    }
                    if !missing.is_empty() {
                        lines.push(format!("{}: {}", i.name, missing.join(" ")));
                        all_missing.extend(missing);
                    }
                }
                if !lines.is_empty() {
                    all_missing.sort();
                    all_missing.dedup();
                    findings.push(format!(
                        "Some installed formulae or casks are missing dependencies:{}\n\
                         You should `brew install` the missing dependencies:\n  brew install {}",
                        indented(&lines),
                        all_missing.join(" ")
                    ));
                }
            }
            "check_deprecated_disabled" | "check_cask_deprecated_disabled" => {
                let (pool, kind, noun) = if check == "check_deprecated_disabled" {
                    (&formulae, Kind::Formula, "formulae")
                } else {
                    (&casks, Kind::Cask, "casks")
                };
                let mut flagged: Vec<String> = pool
                    .iter()
                    .filter(|i| index_flags(kind, &i.name) == Some(true))
                    .map(|i| i.name.clone())
                    .collect();
                flagged.sort();
                flagged.dedup();
                if !flagged.is_empty() {
                    findings.push(format!(
                        "Some installed {noun} are deprecated or disabled.\n\
                         You should find replacements for the following {noun}:\n{}",
                        indented(&flagged)
                    ));
                }
            }
            "check_deleted_formula" => {
                if let Some(rows) = &formula_rows {
                    let tapped: Vec<String> = tap::list(prefix)
                        .iter()
                        .flat_map(tap_formula_names)
                        .collect();
                    let known = |name: &str| {
                        tapped.iter().any(|t| t == name)
                            || rows.iter().any(|row| {
                                row.name == name || row.aliases.iter().any(|a| a == name)
                            })
                    };
                    let mut deleted: Vec<String> = formulae
                        .iter()
                        .map(|i| i.name.clone())
                        .filter(|name| !known(name))
                        .collect();
                    deleted.sort();
                    deleted.dedup();
                    if !deleted.is_empty() {
                        findings.push(format!(
                            "Some installed kegs have no formulae!\n\
                             This means they were either deleted or installed manually.\n\
                             You should find replacements for the following formulae:\n{}",
                            indented(&deleted)
                        ));
                    }
                }
            }
            "check_install_receipts" => {
                for i in &formulae {
                    let receipt = prefix.keg(&i.name, &i.version).join("INSTALL_RECEIPT.json");
                    if !receipt.is_file() {
                        findings.push(format!(
                            "{} {} has no install receipt (poured by something else?)",
                            i.name, i.version
                        ));
                    }
                }
            }
            "check_dangling_pins" => {
                if let Ok(entries) = std::fs::read_dir(prefix.root.join("Pinned")) {
                    for e in entries.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if prefix.pinned_version(&name).is_none() {
                            if prefix.pin_link_dangling_ours(&name) {
                                findings.push(format!(
                                    "pin for {name} points at a keg that is gone (run: abrew unpin {name})"
                                ));
                            } else {
                                findings.push(format!("pin for {name} is not owned by abrew"));
                            }
                        }
                    }
                }
            }
            "check_prefix_on_path" => {
                let path_has_bin = std::env::var_os("PATH")
                    .is_some_and(|p| std::env::split_paths(&p).any(|d| d == prefix.bin()));
                if !formulae.is_empty() && !path_has_bin {
                    findings.push(format!(
                        "{} is not on PATH (eval \"$(abrew shellenv)\")",
                        prefix.bin().display()
                    ));
                }
            }
            other => {
                super::tty::onoe!("Error: No check available by the name: {other}");
                unknown_check = true;
            }
        }
    }

    if named.is_empty()
        && let Some(_lock) = super::prefix::Lock::try_acquire(prefix)
    {
        prefix.restore_stale_asides();
        let _ = prefix.rebuild_state();
    }
    if !findings.is_empty() {
        if !quiet {
            eprintln!(
                "{}",
                super::tty::bold_err(
                    "Please note that these warnings are just used to help the Homebrew maintainers\n\
                     with debugging if you file an issue. If everything you use Homebrew for is\n\
                     working fine: please don't worry or file an issue; just ignore this. Thanks!"
                )
            );
        }
        for finding in &findings {
            eprintln!();
            super::tty::opoo!("Warning: {finding}");
        }
    }
    if findings.is_empty() && !unknown_check {
        if !quiet {
            println!("Your system is ready to brew.");
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn install_opts(rest: &[String]) -> InstallOpts {
    InstallOpts {
        ignore_dependencies: has_flag(rest, "--ignore-dependencies", ""),
        dry_run: has_flag(rest, "--dry-run", "-n"),
        force: has_flag(rest, "--force", "-f"),
        quiet: has_flag(rest, "--quiet", "-q"),
        upgrade: false,
        reinstall: false,
        as_dependency: false,
        ask: ask_mode_enabled(rest),
    }
}

/// Brew's ask mode is the default; `--no-ask`/`--yes` or a set
/// HOMEBREW_NO_ASK turns it off.
fn ask_mode_enabled(rest: &[String]) -> bool {
    if has_flag(rest, "--no-ask", "") || has_flag(rest, "--yes", "-y") {
        return false;
    }
    match std::env::var("HOMEBREW_NO_ASK") {
        Ok(v) => v.is_empty() || v == "0",
        Err(_) => true,
    }
}

/// Brew's install-time rescue for a name nothing resolves:
/// a Warning with the unavailable error, the MissingFormula
/// reason when one applies, otherwise a similar-names search
/// with install hints. Always a failure exit.
fn install_unknown_name(
    prefix: &Prefix,
    api: &Api,
    name: &str,
    want_cask: bool,
    want_formula: bool,
) -> ExitCode {
    if name == "updog" {
        super::tty::onoe!("Error: What's updog?");
        return ExitCode::FAILURE;
    }
    if want_cask {
        super::tty::opoo!(
            "Warning: {}",
            cask_unavailable(super::api::canonical_cask_token(Formula::short_name(name),))
        );
    } else {
        super::tty::opoo!("Warning: {}", formula_unavailable(api, name));
    }
    if !want_cask && let Some(reason) = missing_formula_reason(prefix, api, name) {
        eprint!("{reason}");
        if !reason.ends_with('\n') {
            eprintln!();
        }
        return ExitCode::FAILURE;
    }
    if name.contains('/') {
        return ExitCode::FAILURE;
    }
    let (types_and, types_or) = match (want_formula, want_cask) {
        (true, false) => ("formulae", "formulae"),
        (false, true) => ("casks", "casks"),
        _ => ("formulae and casks", "formulae or casks"),
    };
    println!(
        "{}",
        super::tty::ohai(&format!("Searching for similarly named {types_and}..."))
    );
    let installed_f: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Formula)
        .into_iter()
        .map(|i| i.name)
        .collect();
    let installed_c: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Cask)
        .into_iter()
        .map(|i| i.name)
        .collect();
    let mode = SearchMode::Text(name.to_string());
    let (formulae, casks, _) = match collect_name_hits(
        prefix,
        api,
        &mode,
        name,
        !want_cask,
        !want_formula,
        &installed_f,
        &installed_c,
    ) {
        Ok(hits) => hits,
        Err(code) => return code,
    };
    if !formulae.is_empty() {
        println!("{}", super::tty::ohai("Formulae"));
        print_columns(&formulae);
        println!();
        println!("To install {}, run:", formulae[0]);
        println!("  brew install {}", formulae[0]);
    }
    if !formulae.is_empty() && !casks.is_empty() {
        println!();
    }
    if !casks.is_empty() {
        println!("{}", super::tty::ohai("Casks"));
        print_columns(&casks);
        println!();
        println!("To install {}, run:", casks[0]);
        println!("  brew install --cask {}", casks[0]);
    }
    if formulae.is_empty() && casks.is_empty() {
        super::tty::onoe!("Error: No {types_or} found for {name}.");
    }
    ExitCode::FAILURE
}

pub(super) fn cmd_install(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("install needs at least one name");
    }
    let opts = install_opts(rest);
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    for name in &names {
        if want_cask {
            if let Err(e) = api.cask(name) {
                if e.contains("no such") {
                    return install_unknown_name(prefix, api, name, true, false);
                }
                return failed(&e);
            }
        } else if want_formula {
            if api.formula(name).is_err() && tap::lookup(prefix, name).is_none() {
                return install_unknown_name(prefix, api, name, false, true);
            }
        } else if api.formula(name).is_err()
            && tap::lookup(prefix, name).is_none()
            && let Err(error) = api.cask(name)
        {
            if error.contains("no such") {
                return install_unknown_name(prefix, api, name, false, false);
            }
            return failed(&error);
        }
    }
    if opts.dry_run {
        // Collect every plan first: a delegation anywhere hands the
        // whole dry run to brew before a single summary line prints,
        // and brew groups the requested formulae across the batch.
        let mut tops = Vec::new();
        let mut casks = Vec::new();
        let mut failures = 0;
        for name in &names {
            let as_cask = if want_cask {
                true
            } else if want_formula {
                false
            } else {
                api.formula(name).is_err()
            };
            if as_cask {
                let c = match api.cask(name) {
                    Ok(c) => c,
                    Err(error) => return failed(&error),
                };
                match cask::install_with_deps(api, prefix, &c, true, opts.force, false, false) {
                    Ok(_) => casks.push((c.token.clone(), cask::dry_dep_names(api, prefix, &c))),
                    Err(install::InstallError::Failed(msg)) => {
                        eprintln!("{msg}");
                        failures += 1;
                    }
                    Err(error) => return from_install(prefix, args, Err(error)),
                }
            } else {
                match install::dry_rows(api, prefix, name, &opts) {
                    Ok((top, deps)) => {
                        tops.push((Formula::short_name(name).to_string(), top, deps))
                    }
                    Err(install::InstallError::Failed(msg)) => {
                        eprintln!("{msg}");
                        failures += 1;
                    }
                    Err(error) => return from_install(prefix, args, Err(error)),
                }
            }
        }
        print_dry_sections(&tops, &casks);
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let mut landed = false;
    let mut failures = 0;
    for name in &names {
        let result = if !want_cask && !want_formula {
            if api.formula(name).is_ok() {
                install::install(api, prefix, name, &opts).map(|_| ())
            } else {
                match api.cask(name) {
                    Ok(c) => cask::install_with_deps(
                        api,
                        prefix,
                        &c,
                        opts.dry_run,
                        opts.force,
                        false,
                        false,
                    )
                    .map(|_| ()),
                    Err(error) if error.contains("no such") => {
                        return failed(&no_such(api, name, None));
                    }
                    Err(error) => return failed(&error),
                }
            }
        } else if want_cask {
            match api.cask(name) {
                Ok(c) => {
                    cask::install_with_deps(api, prefix, &c, opts.dry_run, opts.force, false, false)
                        .map(|_| ())
                }
                Err(e) if e.contains("no such") => {
                    if api.formula(name).is_ok() {
                        return failed(&format!(
                            "abrew: {name} is a formula, not a cask (drop --cask)"
                        ));
                    }
                    return failed(&no_such(api, name, Some("cask")));
                }
                Err(e) => return failed(&e),
            }
        } else {
            match install::install(api, prefix, name, &opts).map(|_| ()) {
                Err(install::InstallError::Failed(e)) if e.contains("no such") => {
                    if api.cask(name).is_ok() {
                        return failed(&format!(
                            "abrew: {name} is a cask, not a formula (drop --formula)"
                        ));
                    }
                    return failed(&no_such(api, name, Some("formula")));
                }
                other => other,
            }
        };
        match result {
            Ok(()) => landed = true,
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
            Err(error @ install::InstallError::Unsupported(_)) => {
                if names.len() > 1 && landed {
                    let install::InstallError::Unsupported(reason) = &error else {
                        unreachable!()
                    };
                    eprintln!("abrew: {reason}; skipped");
                    failures += 1;
                } else {
                    return from_install(prefix, args, Err(error));
                }
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Brew's `Formatter.version_changes` for dry-run rows: a fresh
/// install shows just the new version, a replacement shows
/// `old -> new`, columns aligned like the table printer.
fn dry_change_lines(rows: &[install::DryRow]) -> Vec<String> {
    let name_width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let old_width = rows
        .iter()
        .filter_map(|r| r.old_version.as_deref())
        .map(str::len)
        .max()
        .unwrap_or(0);
    let sep = if rows.len() == 1 { " " } else { "  " };
    rows.iter()
        .map(|r| {
            let versions = match &r.old_version {
                Some(old) => format!("{old:old_width$} -> {}", r.new_version),
                None => r.new_version.clone(),
            };
            format!("{:name_width$}{sep}{versions}", r.name)
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Prints brew's dry-run install summary: the requested formulae
/// grouped install-then-upgrade, each top formula's dependency
/// sections, then the casks with theirs. `tops` pairs each requested
/// formula's display name with its `install::dry_rows` result;
/// `casks` pairs each token with its uninstalled dependency names.
pub(super) fn print_dry_sections(
    tops: &[(String, Option<install::DryRow>, Vec<install::DryRow>)],
    casks: &[(String, Vec<String>)],
) {
    fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
        if n == 1 { one } else { many }
    }
    for upgrade in [false, true] {
        let group: Vec<&install::DryRow> = tops
            .iter()
            .filter_map(|(_, top, _)| top.as_ref())
            .filter(|r| r.old_version.is_some() == upgrade)
            .collect();
        if group.is_empty() {
            continue;
        }
        println!(
            "{}",
            super::tty::ohai(&format!(
                "Would {} {} {}:",
                if upgrade { "upgrade" } else { "install" },
                group.len(),
                plural(group.len(), "formula", "formulae")
            ))
        );
        let owned: Vec<install::DryRow> = group
            .iter()
            .map(|r| install::DryRow {
                name: r.name.clone(),
                old_version: r.old_version.clone(),
                new_version: r.new_version.clone(),
            })
            .collect();
        for line in dry_change_lines(&owned) {
            println!("{line}");
        }
    }
    for (display, _, deps) in tops {
        for upgrade in [false, true] {
            let group: Vec<install::DryRow> = deps
                .iter()
                .filter(|r| r.old_version.is_some() == upgrade)
                .map(|r| install::DryRow {
                    name: r.name.clone(),
                    old_version: r.old_version.clone(),
                    new_version: r.new_version.clone(),
                })
                .collect();
            if group.is_empty() {
                continue;
            }
            println!(
                "{}",
                super::tty::ohai(&format!(
                    "Would {} {} {} for {display}:",
                    if upgrade { "upgrade" } else { "install" },
                    group.len(),
                    plural(group.len(), "dependency", "dependencies")
                ))
            );
            for line in dry_change_lines(&group) {
                println!("{line}");
            }
        }
    }
    if !casks.is_empty() {
        println!(
            "{}",
            super::tty::ohai(&format!(
                "Would install {} {}:",
                casks.len(),
                plural(casks.len(), "cask", "casks")
            ))
        );
        println!(
            "{}",
            casks
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        for (token, deps) in casks {
            if deps.is_empty() {
                continue;
            }
            println!(
                "{}",
                super::tty::ohai(&format!(
                    "Would install {} {} for {token}:",
                    deps.len(),
                    plural(deps.len(), "dependency", "dependencies")
                ))
            );
            println!("{}", deps.join(" "));
        }
    }
}

/// The terminal width for column layout: stdout's window size,
/// else `$COLUMNS`, else 80 (brew's `Tty.width` sources).
fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCGWINSZ` writes one `winsize` into the struct
        // handed to it and reads nothing else; fd 1 is our stdout.
        let rc = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return usize::from(ws.ws_col);
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|c| *c > 0)
        .unwrap_or(80)
}

/// Brew's `Formatter.columns`: layout names in columns fitting
/// `width`, filled column-first; a non-terminal (or a single
/// column's width) falls back to one per line, which is all a
/// pipe ever sees.
fn format_columns(objects: &[String], width: usize, tty: bool) -> String {
    let lines = || objects.join("\n");
    if objects.is_empty() || !tty {
        return lines();
    }
    let gap = 2usize;
    let max_len = objects
        .iter()
        .map(|o| super::tty::visible_len(o))
        .max()
        .unwrap_or(0);
    let mut cols = (width + gap) / (max_len + gap);
    if cols < 2 {
        return lines();
    }
    let rows = objects.len().div_ceil(cols);
    cols = objects.len().div_ceil(rows);
    let col_width = (width + gap) / cols - gap;
    let mut out = String::new();
    for row in 0..rows {
        let mut cells: Vec<String> = Vec::new();
        let mut idx = row;
        while idx < objects.len() {
            cells.push(objects[idx].clone());
            idx += rows;
        }
        let last = cells.pop().unwrap_or_default();
        for cell in &cells {
            out.push_str(cell);
            for _ in super::tty::visible_len(cell)..col_width {
                out.push(' ');
            }
            out.push_str("  ");
        }
        out.push_str(&last);
        if row + 1 < rows {
            out.push('\n');
        }
    }
    out
}

/// BSD `ls` multi-column layout (what plain `brew list` runs):
/// one tab-rounded column width from the longest entry, entries
/// filled column-first and tab-padded to that width. Verified
/// byte-for-byte against `/bin/ls` on this host.
fn ls_columns(objects: &[String], width: usize) -> String {
    if objects.is_empty() {
        return String::new();
    }
    let max_len = objects
        .iter()
        .map(|o| super::tty::visible_len(o))
        .max()
        .unwrap_or(0);
    let col_width = (max_len + 8) & !7;
    let cols = (width / col_width).max(1);
    let rows = objects.len().div_ceil(cols);
    let mut out = String::new();
    for row in 0..rows {
        for col in 0..cols {
            let idx = col * rows + row;
            if idx >= objects.len() {
                break;
            }
            let name = &objects[idx];
            out.push_str(name);
            if col + 1 < cols && (col + 1) * rows + row < objects.len() {
                let mut cnt = 0;
                while cnt < col_width - name.chars().count() {
                    out.push('\t');
                    cnt += 8;
                }
            }
        }
        out.push('\n');
    }
    out
}

/// Prints names through [`format_columns`] against the real stdout.
pub(super) fn print_columns_pub(objects: &[String]) {
    print_columns(objects);
}

fn print_columns(objects: &[String]) {
    use std::io::IsTerminal as _;
    if objects.is_empty() {
        return;
    }
    println!(
        "{}",
        format_columns(objects, terminal_width(), std::io::stdout().is_terminal())
    );
}

/// Brew's `Utils::Text.to_sentence`: "a", "a and b", "a, b, and c".
pub(super) fn to_sentence(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (last, head) = items.split_last().expect("non-empty");
            format!("{}, and {last}", head.join(", "))
        }
    }
}

pub(super) fn cmd_uninstall(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("uninstall needs at least one name");
    }
    let ignore = has_flag(rest, "--ignore-dependencies", "");
    let dry = has_flag(rest, "--dry-run", "-n");
    let force = has_flag(rest, "--force", "-f");
    let zap = has_flag(rest, "--zap", "");
    if !ignore && !dry && !has_flag(rest, "--cask", "") {
        let removing: std::collections::BTreeSet<String> = names
            .iter()
            .map(|n| Formula::short_name(n).to_string())
            .collect();
        let mut reqs: Vec<String> = Vec::new();
        let mut deps: Vec<String> = Vec::new();
        for name in &names {
            let short = Formula::short_name(name);
            let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
                continue;
            };
            if let Ok(on) = install::dependents(api, prefix, short) {
                let on: Vec<String> = on.into_iter().filter(|d| !removing.contains(d)).collect();
                if !on.is_empty() {
                    reqs.push(prefix.keg(short, &inst.version).display().to_string());
                    for d in on {
                        if !deps.contains(&d) {
                            deps.push(d);
                        }
                    }
                }
            }
        }
        if !reqs.is_empty() {
            eprintln!(
                "Error: Refusing to uninstall {}\nbecause {} {} required by {}, which {} currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies {}",
                to_sentence(&reqs),
                if reqs.len() == 1 { "it" } else { "they" },
                if reqs.len() == 1 { "is" } else { "are" },
                to_sentence(&deps),
                if deps.len() == 1 { "is" } else { "are" },
                names.join(" ")
            );
            return ExitCode::FAILURE;
        }
    }
    let mut failures = 0;
    let mut completed = false;
    for name in &names {
        let short = Formula::short_name(name);
        let cask_short = super::api::canonical_cask_token(short);
        if !zap
            && !has_flag(rest, "--cask", "")
            && !has_flag(rest, "--formula", "")
            && prefix.installed_one(Kind::Formula, short).is_some()
            && prefix.installed_one(Kind::Cask, cask_short).is_some()
        {
            eprintln!(
                "abrew: {short} is installed as both a formula and a cask; say --formula or --cask"
            );
            failures += 1;
            continue;
        }
        let as_cask = zap
            || has_flag(rest, "--cask", "")
            || (!has_flag(rest, "--formula", "")
                && prefix.installed_one(Kind::Formula, short).is_none()
                && prefix.installed_one(Kind::Cask, cask_short).is_some());
        if as_cask
            && !force
            && !dry
            && prefix.cask_is_pinned(cask_short)
            && let Some(inst) = prefix.installed_one(Kind::Cask, cask_short)
        {
            if zap {
                // Brew silently skips pinned casks when zapping.
                continue;
            }
            eprintln!(
                "Error: {} is pinned. You must unpin it to uninstall.",
                prefix.specified_name(&inst)
            );
            failures += 1;
            continue;
        }
        let r = if as_cask {
            if zap {
                match api.cask(cask_short) {
                    Ok(c) => cask::zap(prefix, cask_short, &c, dry, force),
                    Err(_) => Err(install::InstallError::Failed(format!(
                        "Error: {}",
                        formula_unavailable(api, name)
                    ))),
                }
            } else if force
                && has_flag(rest, "--cask", "")
                && prefix.installed_one(Kind::Cask, cask_short).is_none()
            {
                Ok(())
            } else {
                // Stanza directives need the definition; without it
                // (offline, unknown token) uninstall stays
                // receipt-driven.
                let resolved = api.cask(cask_short).ok();
                cask::uninstall(prefix, cask_short, dry, resolved.as_ref())
            }
        } else {
            let pinned_refusal = !force && prefix.pinned_version(short).is_some();
            if !dry
                && !pinned_refusal
                && let Some(inst) = prefix.installed_one(Kind::Formula, short)
            {
                if force {
                    println!(
                        "Uninstalling {short}... ({})",
                        keg_abv(&prefix.cellar().join(short))
                    );
                } else {
                    let targets = install::uninstall_versions(prefix, &inst, false);
                    let keg = prefix.keg(short, &targets[0]);
                    println!("Uninstalling {}... ({})", keg.display(), keg_abv(&keg));
                }
            }
            install::uninstall(api, prefix, name, ignore, force, dry).inspect(|_| {
                if !dry
                    && !force
                    && let Some(left) = prefix.installed_one(Kind::Formula, short)
                    && !left.versions.is_empty()
                {
                    println!(
                        "{short} {} {} still installed.",
                        to_sentence(&left.versions),
                        if left.versions.len() == 1 {
                            "is"
                        } else {
                            "are"
                        }
                    );
                    println!("To remove all versions, run:");
                    println!("  brew uninstall --force {short}");
                }
            })
        };
        match r {
            Ok(()) => completed = true,
            Err(e @ install::InstallError::Unsupported(_)) => {
                return from_install(
                    prefix,
                    args,
                    Err(super::after_native_progress(e, completed)),
                );
            }
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_reinstall(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("reinstall needs at least one name");
    }
    let opts = InstallOpts {
        force: true,
        reinstall: true,
        ..install_opts(rest)
    };
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    if opts.dry_run {
        // Brew's reinstall has no dry run; abrew's extension reports
        // through the same grouped summary as install.
        let mut tops = Vec::new();
        let mut casks = Vec::new();
        let mut failures = 0;
        for name in &names {
            let short = Formula::short_name(name);
            let cask_short = super::api::canonical_cask_token(short);
            let as_cask = if want_cask {
                true
            } else if want_formula || prefix.installed_one(Kind::Formula, short).is_some() {
                false
            } else if prefix.installed_one(Kind::Cask, cask_short).is_some() {
                true
            } else {
                api.formula(name).is_err() && api.cask(cask_short).is_ok()
            };
            if as_cask {
                match api.cask(cask_short) {
                    Ok(c) => {
                        match cask::install_with_deps(api, prefix, &c, true, true, false, true) {
                            Ok(_) => {
                                casks.push((c.token.clone(), cask::dry_dep_names(api, prefix, &c)))
                            }
                            Err(install::InstallError::Failed(msg)) => {
                                eprintln!("{msg}");
                                failures += 1;
                            }
                            Err(error) => return from_install(prefix, args, Err(error)),
                        }
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        failures += 1;
                    }
                }
            } else {
                match install::dry_rows(api, prefix, short, &opts) {
                    Ok((top, deps)) => tops.push((short.to_string(), top, deps)),
                    Err(install::InstallError::Failed(msg)) => {
                        eprintln!("{msg}");
                        failures += 1;
                    }
                    Err(error) => return from_install(prefix, args, Err(error)),
                }
            }
        }
        print_dry_sections(&tops, &casks);
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let mut completed = false;
    let mut failures = 0;
    // Brew builds every reinstall context first (fetching each
    // formula's bottle tab), then fetches every download under
    // one heading, then pours.
    let mut heading_names: Vec<String> = Vec::new();
    for name in &names {
        let short = Formula::short_name(name);
        let cask_short = super::api::canonical_cask_token(short);
        let as_cask = if want_cask {
            true
        } else if want_formula || prefix.installed_one(Kind::Formula, short).is_some() {
            false
        } else if prefix.installed_one(Kind::Cask, cask_short).is_some() {
            true
        } else {
            api.formula(name).is_err() && api.cask(cask_short).is_ok()
        };
        if as_cask || prefix.installed_one(Kind::Formula, short).is_none() {
            continue;
        }
        if prefix.pinned_version(short).is_some() {
            continue;
        }
        if let Ok(f) = api
            .formula(name)
            .or_else(|_| tap::lookup(prefix, name).ok_or_else(|| "unavailable".to_string()))
        {
            narrate_reinstall_tab(prefix, &f);
            heading_names.push(short.to_string());
        }
    }
    narrate_fetch_heading(&heading_names);
    for name in &names {
        let short = Formula::short_name(name);
        let cask_short = super::api::canonical_cask_token(short);
        let as_cask = if want_cask {
            true
        } else if want_formula || prefix.installed_one(Kind::Formula, short).is_some() {
            false
        } else if prefix.installed_one(Kind::Cask, cask_short).is_some() {
            true
        } else {
            api.formula(name).is_err() && api.cask(cask_short).is_ok()
        };
        // Brew refuses to reinstall a pinned package (both kinds).
        let pinned_inst = if as_cask {
            prefix
                .cask_is_pinned(cask_short)
                .then(|| prefix.installed_one(Kind::Cask, cask_short))
                .flatten()
        } else {
            prefix
                .pinned_version(short)
                .is_some()
                .then(|| prefix.installed_one(Kind::Formula, short))
                .flatten()
        };
        if let Some(inst) = pinned_inst {
            eprintln!(
                "Error: {} is pinned. You must unpin it to reinstall.",
                prefix.specified_name(&inst)
            );
            failures += 1;
            continue;
        }
        let r: Result<(), install::InstallError> = if as_cask {
            match api.cask(cask_short) {
                Ok(c) => cask::install_with_deps(api, prefix, &c, opts.dry_run, true, false, true)
                    .map(|_| ()),
                Err(_) => Err(install::InstallError::Failed(format!(
                    "Error: {}",
                    cask_unavailable(cask_short)
                ))),
            }
        } else if api.formula(name).is_err() && tap::lookup(prefix, name).is_none() {
            Err(install::InstallError::Failed(format!(
                "Error: {}",
                formula_unavailable(api, name)
            )))
        } else {
            // Brew's per-formula banner (the trailing space is
            // where restored options would be listed).
            let full = api
                .formula(name)
                .ok()
                .or_else(|| tap::lookup(prefix, name))
                .map(|f| {
                    f.full_name
                        .strip_prefix("homebrew/core/")
                        .map_or_else(|| f.full_name.clone(), str::to_string)
                })
                .unwrap_or_else(|| short.to_string());
            println!(
                "{}",
                super::tty::oh1(&format!("Reinstalling {} ", super::tty::ident(&full)))
            );
            install::install(api, prefix, short, &opts).map(|_| ())
        };
        completed |= super::native_attempt_may_have_progress(&r);
        match r {
            Ok(()) => {
                let target = if as_cask { cask_short } else { short };
                if let Err(error) = auto_cleanup_old_versions(prefix, api, opts.dry_run, &[target])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(e @ install::InstallError::Unsupported(_)) => {
                return from_install(
                    prefix,
                    args,
                    Err(super::after_native_progress(e, completed)),
                );
            }
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn outdated_rows(
    prefix: &Prefix,
    api: &Api,
    casks: bool,
    greedy: bool,
) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    if casks {
        for i in prefix.installed(Kind::Cask) {
            let c = api.cask(&i.name)?;
            if c.auto_updates && !greedy {
                continue;
            }
            if version_cmp(&i.version, &c.version) == Ordering::Less {
                out.push((i.name, i.version, c.version));
            }
        }
    } else {
        for i in prefix.installed(Kind::Formula) {
            let f = api.formula(&i.name)?;
            if version_cmp(&i.version, &install::keg_version(&f)) == Ordering::Less {
                out.push((i.name, i.version, install::keg_version(&f)));
            }
        }
    }
    Ok(out)
}

fn outdated_json_row(prefix: &Prefix, kind: Kind, name: &str, latest: &str) -> aterm_json::Value {
    let installed = prefix.installed_one(kind, name);
    let versions: Vec<aterm_json::Value> = installed
        .as_ref()
        .map(|i| {
            i.versions
                .iter()
                .cloned()
                .map(aterm_json::Value::String)
                .collect()
        })
        .unwrap_or_default();
    let pinned_version = match kind {
        Kind::Formula => prefix.pinned_version(name),
        Kind::Cask => prefix.cask_pinned_version(name),
    };
    let pinned = pinned_version.is_some();
    let mut map = aterm_json::Map::new();
    map.insert(
        "name".to_string(),
        aterm_json::Value::String(name.to_string()),
    );
    map.insert(
        "installed_versions".to_string(),
        aterm_json::Value::Array(versions),
    );
    map.insert(
        "current_version".to_string(),
        aterm_json::Value::String(latest.to_string()),
    );
    map.insert("pinned".to_string(), aterm_json::Value::Bool(pinned));
    map.insert(
        "pinned_version".to_string(),
        pinned_version
            .map(aterm_json::Value::String)
            .unwrap_or(aterm_json::Value::Null),
    );
    aterm_json::Value::Object(map)
}

const fn outdated_failed(any: bool, named: bool) -> bool {
    any && named
}

pub(super) fn cmd_outdated(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let scope: Vec<String> = operands(rest)
        .iter()
        .map(|n| Formula::short_name(n).to_string())
        .collect();
    for name in &scope {
        let unknown = package_definitively_unknown(
            prefix,
            api,
            name,
            !has_flag(rest, "--cask", ""),
            !has_flag(rest, "--formula", ""),
        );
        if unknown {
            return failed(&format!(
                "Error: {}",
                if has_flag(rest, "--cask", "") && !has_flag(rest, "--formula", "") {
                    cask_unavailable(name)
                } else {
                    formula_unavailable(api, name)
                }
            ));
        }
    }
    if has_flag(rest, "--json=v1", "") {
        return failed("abrew: outdated --json=v1 is not supported; use --json=v2");
    }
    let json = has_flag(rest, "--json", "") || has_flag(rest, "--json=v2", "");
    let mut collected: [Vec<(String, String, String)>; 2] = [Vec::new(), Vec::new()];
    for (slot, casks) in [(0usize, false), (1usize, true)] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        match outdated_rows(prefix, api, casks, has_flag(rest, "--greedy", "")) {
            Ok(mut rows) => {
                if !scope.is_empty() {
                    rows.retain(|(name, _, _)| {
                        scope.iter().any(|s| {
                            s == name || (casks && super::api::canonical_cask_token(s) == name)
                        })
                    });
                }
                collected[slot] = rows;
            }
            Err(error) => return failed(&error),
        }
    }
    let any = collected.iter().any(|r| !r.is_empty());
    if json {
        let rows_json = |rows: &[(String, String, String)], kind: Kind| {
            rows.iter()
                .map(|(name, _, latest)| outdated_json_row(prefix, kind, name, latest))
                .collect::<Vec<_>>()
        };
        let mut map = aterm_json::Map::new();
        map.insert(
            "formulae".to_string(),
            aterm_json::Value::Array(rows_json(&collected[0], Kind::Formula)),
        );
        map.insert(
            "casks".to_string(),
            aterm_json::Value::Array(rows_json(&collected[1], Kind::Cask)),
        );
        let value = aterm_json::Value::Object(map);
        match aterm_json::to_string(&value) {
            Ok(text) => println!("{text}"),
            Err(e) => return failed(&format!("abrew: {e}")),
        }
    } else {
        // Brew prints names alone unless verbose — a terminal or
        // --verbose, either overridden by --quiet; verbose groups
        // every outdated keg version and marks pins.
        let verbose = (super::tty::stdout_tty() || has_flag(rest, "--verbose", "-v"))
            && !has_flag(rest, "--quiet", "-q");
        for (rows, casks) in collected.iter().zip([false, true]) {
            let kind = if casks { Kind::Cask } else { Kind::Formula };
            for (name, have, latest) in rows {
                let inst = prefix.installed_one(kind, name);
                // Brew prints the specified name: the full name
                // with an official tap prefix stripped.
                let shown = match &inst {
                    Some(i) if !casks => prefix.specified_name(i),
                    _ => name.clone(),
                };
                if !verbose {
                    println!("{shown}");
                    continue;
                }
                let versions = match &inst {
                    Some(i) => i
                        .versions
                        .iter()
                        .filter(|v| version_cmp(v, latest) == Ordering::Less)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    None => have.clone(),
                };
                let versions = if versions.is_empty() {
                    have.clone()
                } else {
                    versions
                };
                let pinned_at = if casks {
                    prefix.cask_pinned_version(name)
                } else {
                    prefix.pinned_version(name)
                };
                let pin = match pinned_at {
                    Some(p) => format!(" [pinned at {p}]"),
                    _ => String::new(),
                };
                println!("{shown} ({versions}) < {latest}{pin}");
            }
        }
    }
    if outdated_failed(any, !scope.is_empty()) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Brew's bottle-manifest prelude: the heading prints once,
/// only when at least one manifest actually downloads.
fn narrate_manifest_prelude(prefix: &Prefix, formulae: &[Formula]) {
    let mut heading_shown = false;
    for f in formulae {
        if install::bottle_manifest_cached(prefix, f) {
            continue;
        }
        if !heading_shown {
            eprintln!("{}", super::tty::oh1("Downloading bottle manifests"));
            heading_shown = true;
        }
        if let Ok(true) = install::fetch_bottle_manifest(prefix, f) {
            eprintln!("✔︎ Bottle Manifest {} ({})", f.name, install::keg_version(f));
        }
    }
}

/// Brew's reinstall context fetches the bottle tab directly
/// (not through the queue): a Downloading line, plus the
/// cached-location line on a cache hit.
fn narrate_reinstall_tab(prefix: &Prefix, f: &Formula) {
    let Some(bottle) = f.bottle_for(&super::machine::Machine::this().bottle_tags()) else {
        return;
    };
    let Some(repo) = super::api::ghcr_repo_of(&bottle.url) else {
        return;
    };
    let version = install::keg_version(f);
    let tag = if f.bottle_rebuild > 0 {
        format!("{version}-{}", f.bottle_rebuild)
    } else {
        version
    };
    println!(
        "{}",
        super::tty::ohai(&format!(
            "Downloading https://ghcr.io/v2/{repo}/manifests/{tag}"
        ))
    );
    if install::bottle_manifest_cached(prefix, f) {
        println!(
            "Already downloaded: {}",
            install::bottle_manifest_dest(prefix, f).display()
        );
    } else {
        let _ = install::fetch_bottle_manifest(prefix, f);
    }
}

/// Brew's shared download-queue heading on stderr.
fn narrate_fetch_heading(names: &[String]) {
    if names.is_empty() {
        return;
    }
    let named: Vec<String> = names.iter().map(|n| super::tty::ident(n)).collect();
    eprintln!(
        "{}",
        super::tty::oh1(&format!("Fetching downloads for: {}", to_sentence(&named)))
    );
}

/// The formulae behind outdated rows, for the manifest prelude.
fn row_formulae(api: &Api, prefix: &Prefix, names: &[&str]) -> Vec<Formula> {
    names
        .iter()
        .filter_map(|name| api.formula(name).ok().or_else(|| tap::lookup(prefix, name)))
        .collect()
}

/// Brew's `Formatter.version_changes` in redirected output:
/// name and version columns padded to their widest entry, a
/// two-space separator (one space for a single change), and the
/// download size in parentheses when known.
fn version_change_lines(rows: &[(String, String, String, Option<String>)]) -> Vec<String> {
    let name_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let sep = if rows.len() == 1 { " " } else { "  " };
    let old_width = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
    let versions: Vec<String> = rows
        .iter()
        .map(|r| format!("{:old_width$} -> {}", r.1, r.2))
        .collect();
    let versions_width = versions.iter().map(|v| v.len()).max().unwrap_or(0);
    rows.iter()
        .zip(versions)
        .map(|(r, versions)| {
            let mut parts = vec![
                format!("{:name_width$}", r.0),
                format!("{versions:versions_width$}"),
            ];
            if let Some(size) = &r.3 {
                parts.push(format!("({size})"));
            }
            parts.join(sep).trim_end().to_string()
        })
        .collect()
}

/// A formula's bottle download size for a dry-run table, by HEAD
/// request. Best effort, like brew's optional bottle size.
fn upgrade_bottle_size(prefix: &Prefix, api: &Api, name: &str) -> Option<String> {
    let f = api
        .formula(name)
        .ok()
        .or_else(|| tap::lookup(prefix, name))?;
    let bottle = f.bottle_for(&super::machine::Machine::this().bottle_tags())?;
    let mut headers: Vec<String> = Vec::new();
    if let Some(repo) = super::api::ghcr_repo_of(&bottle.url) {
        headers.push(format!(
            "Authorization: Bearer {}",
            super::api::ghcr_token(&repo).ok()?
        ));
    }
    let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
    fetch::head_size(&bottle.url, &refs).map(disk_usage_readable)
}

pub(super) fn cmd_upgrade(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let mut opts = install_opts(rest);
    opts.upgrade = true;
    if has_flag(rest, "--cask", "") {
        return cmd_upgrade_casks(prefix, api, args, rest, &opts, operands(rest), false);
    }
    let names = operands(rest);
    if !names.is_empty() && !has_flag(rest, "--formula", "") {
        let (cask_names, formula_names): (Vec<String>, Vec<String>) =
            names.into_iter().partition(|name| {
                let short = Formula::short_name(name);
                let cask_short = super::api::canonical_cask_token(short);
                prefix.installed_one(Kind::Formula, short).is_none()
                    && prefix.installed_one(Kind::Cask, cask_short).is_some()
            });
        if !formula_names.is_empty() {
            let code = upgrade_formula_names(prefix, api, args, &formula_names, &opts);
            if code != ExitCode::SUCCESS {
                return code;
            }
        }
        if !cask_names.is_empty() {
            return cmd_upgrade_casks(
                prefix,
                api,
                args,
                rest,
                &opts,
                cask_names,
                !formula_names.is_empty(),
            );
        }
        return ExitCode::SUCCESS;
    }
    let names = operands(rest);
    let Some(_name) = names.first() else {
        let rows = match outdated_rows(prefix, api, false, true) {
            Ok(rows) => rows,
            Err(error) => return failed(&error),
        };
        if rows.is_empty() {
            // Brew says nothing when there is nothing to do.
            return ExitCode::SUCCESS;
        }
        let (pinned, rows): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|(name, _, _)| prefix.is_pinned(name));
        if !opts.quiet && !rows.is_empty() {
            let names: Vec<&str> = rows.iter().map(|(n, _, _)| n.as_str()).collect();
            narrate_manifest_prelude(prefix, &row_formulae(api, prefix, &names));
        }
        // Brew's upgrade summary, shown for the dry run and as the
        // ask-mode preview before a real run.
        let print_would = |rows: &[(String, String, String)],
                           pinned: &[(String, String, String)]| {
            if !rows.is_empty() {
                println!(
                    "{}",
                    super::tty::oh1(&format!(
                        "Would upgrade {} outdated {}",
                        rows.len(),
                        if rows.len() == 1 {
                            "package"
                        } else {
                            "packages"
                        }
                    ))
                );
                let changes: Vec<_> = rows
                    .iter()
                    .map(|(name, have, latest)| {
                        (
                            name.clone(),
                            have.clone(),
                            latest.clone(),
                            upgrade_bottle_size(prefix, api, name),
                        )
                    })
                    .collect();
                for line in version_change_lines(&changes) {
                    println!("{line}");
                }
            }
            if !pinned.is_empty() {
                println!(
                    "{}",
                    super::tty::oh1(&format!(
                        "{} Pinned {}",
                        pinned.len(),
                        if pinned.len() == 1 {
                            "formula"
                        } else {
                            "formulae"
                        }
                    ))
                );
                let changes: Vec<_> = pinned
                    .iter()
                    .map(|(n, h, l)| (n.clone(), h.clone(), l.clone(), None))
                    .collect();
                for line in version_change_lines(&changes) {
                    println!("{line}");
                }
            }
        };
        if opts.dry_run {
            print_would(&rows, &pinned);
            return ExitCode::SUCCESS;
        }
        // Ask mode: the preview summary, the prompt, then the
        // shared download heading before any pour.
        let mut previewed_pinned = false;
        if opts.ask {
            if !rows.is_empty() {
                print_would(&rows, &pinned);
                previewed_pinned = !pinned.is_empty();
            }
            if let Err(e) = install::ask_to_proceed("upgrade") {
                return from_install(prefix, args, Err(e));
            }
        }
        if !rows.is_empty() {
            let names: Vec<String> = rows.iter().map(|(n, _, _)| n.clone()).collect();
            narrate_fetch_heading(&names);
        }
        let mut failures = 0;
        let mut upgraded: Vec<(String, String, String, Option<String>)> = Vec::new();
        for (name, have, latest) in rows {
            // Brew's per-formula upgrade message: the headline,
            // then the indented version change (with the older
            // version only for an opt-linked keg).
            println!(
                "{}",
                super::tty::oh1(&format!("Upgrading {}", super::tty::ident(&name)))
            );
            match prefix.optlinked_version(&name) {
                Some(opt) => println!("  {opt} -> {latest} "),
                None => println!("  -> {latest} "),
            }
            match install::install(api, prefix, &name, &opts) {
                Ok(now) if now != have => {
                    upgraded.push((name.clone(), have, now, None));
                    if !opts.dry_run
                        && let Err(error) =
                            auto_cleanup_old_versions(prefix, api, false, &[name.as_str()])
                    {
                        eprintln!("abrew: {error}");
                        failures += 1;
                    }
                }
                Ok(_) => {}
                Err(install::InstallError::Unsupported(reason)) => {
                    eprintln!("abrew: {name}: {reason}; skipped");
                    failures += 1;
                }
                Err(install::InstallError::Aborted) => return ExitCode::from(1),
                Err(install::InstallError::Failed(error)) => {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
        }
        // Brew's final summary appears once at least two packages
        // were upgraded.
        if upgraded.len() >= 2 {
            println!(
                "{}",
                super::tty::oh1(&format!(
                    "Upgraded {} outdated {}",
                    upgraded.len(),
                    if upgraded.len() == 1 {
                        "package"
                    } else {
                        "packages"
                    }
                ))
            );
            for line in version_change_lines(&upgraded) {
                println!("{line}");
            }
        }
        // Brew does not repeat a summary section it already showed
        // in the preview, unless at least two packages upgraded.
        if !pinned.is_empty() && (!previewed_pinned || upgraded.len() >= 2) {
            println!(
                "{}",
                super::tty::oh1(&format!(
                    "{} Pinned {}",
                    pinned.len(),
                    if pinned.len() == 1 {
                        "formula"
                    } else {
                        "formulae"
                    }
                ))
            );
            let changes: Vec<_> = pinned
                .iter()
                .map(|(n, h, l)| (n.clone(), h.clone(), l.clone(), None))
                .collect();
            for line in version_change_lines(&changes) {
                println!("{line}");
            }
        }
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    };
    upgrade_formula_names(prefix, api, args, &names, &opts)
}

fn upgrade_formula_names(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    names: &[String],
    opts: &install::InstallOpts,
) -> ExitCode {
    let mut failures = 0;
    let mut landed = false;
    let mut changes: Vec<(String, String, String, Option<String>)> = Vec::new();
    // Brew fetches the planned formulae's bottle manifests first,
    // then every download under one heading.
    let planned: Vec<Formula> = names
        .iter()
        .filter_map(|name| {
            let short = Formula::short_name(name);
            let before = prefix.installed_one(Kind::Formula, short)?;
            if prefix.is_pinned(short) {
                return None;
            }
            let f = api
                .formula(name)
                .ok()
                .or_else(|| tap::lookup(prefix, name))?;
            (version_cmp(&before.version, &install::keg_version(&f)) == Ordering::Less).then_some(f)
        })
        .collect();
    if !opts.quiet && !planned.is_empty() {
        narrate_manifest_prelude(prefix, &planned);
    }
    if !opts.dry_run && !planned.is_empty() {
        narrate_fetch_heading(&planned.iter().map(|f| f.name.clone()).collect::<Vec<_>>());
    }
    for name in names {
        let short = Formula::short_name(name);
        let Some(before) = prefix.installed_one(Kind::Formula, short) else {
            eprintln!(
                "{}",
                absent_error(api, prefix, name, format!("{short} not installed"))
            );
            failures += 1;
            continue;
        };
        if prefix.is_pinned(short) {
            let latest = api
                .formula(name)
                .ok()
                .or_else(|| tap::lookup(prefix, name))
                .map(|f| install::keg_version(&f));
            match latest {
                Some(latest) if version_cmp(&before.version, &latest) != Ordering::Less => {
                    super::tty::opoo!("Warning: {short} {} already installed", before.version);
                }
                _ => {
                    eprintln!("abrew: {short} is pinned; skipping");
                    failures += 1;
                }
            }
            continue;
        }
        if opts.dry_run {
            // Brew's dry run resolves and reports; it installs
            // nothing and prints no per-formula install output.
            let latest = api
                .formula(name)
                .ok()
                .or_else(|| tap::lookup(prefix, name))
                .map(|f| install::keg_version(&f));
            match latest {
                Some(latest) if version_cmp(&before.version, &latest) == Ordering::Less => {
                    changes.push((
                        short.to_string(),
                        before.version.clone(),
                        latest,
                        upgrade_bottle_size(prefix, api, name),
                    ));
                }
                Some(_) => {
                    super::tty::opoo!("Warning: {short} {} already installed", before.version);
                }
                None => {
                    super::tty::onoe!("Error: {}", formula_unavailable(api, name));
                    failures += 1;
                }
            }
            continue;
        }
        // Brew's per-formula upgrade message before the install.
        let latest = api
            .formula(name)
            .ok()
            .or_else(|| tap::lookup(prefix, name))
            .map(|f| install::keg_version(&f));
        if let Some(latest) = &latest
            && version_cmp(&before.version, latest) == Ordering::Less
        {
            println!(
                "{}",
                super::tty::oh1(&format!("Upgrading {}", super::tty::ident(short)))
            );
            match prefix.optlinked_version(short) {
                Some(opt) => println!("  {opt} -> {latest} "),
                None => println!("  -> {latest} "),
            }
        }
        let result = install::install(api, prefix, name, opts);
        landed |= super::native_attempt_may_have_progress(&result);
        match result {
            Ok(installed_now) => {
                if installed_now != before.version {
                    changes.push((
                        short.to_string(),
                        before.version.clone(),
                        installed_now.clone(),
                        None,
                    ));
                }
                if !opts.dry_run
                    && installed_now != before.version
                    && let Err(error) = auto_cleanup_old_versions(prefix, api, false, &[name])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
            Err(error @ install::InstallError::Unsupported(_)) => {
                if names.len() > 1 && landed {
                    let install::InstallError::Unsupported(reason) = &error else {
                        unreachable!()
                    };
                    eprintln!("abrew: {reason}; skipped");
                    failures += 1;
                } else {
                    return from_install(prefix, args, Err(error));
                }
            }
        }
    }
    // Brew's final summary: always for a dry run, otherwise once
    // at least two packages were upgraded.
    if !changes.is_empty() && (opts.dry_run || changes.len() >= 2) {
        println!(
            "{}",
            super::tty::oh1(&format!(
                "{} {} requested outdated {}",
                if opts.dry_run {
                    "Would upgrade"
                } else {
                    "Upgraded"
                },
                changes.len(),
                if changes.len() == 1 {
                    "package"
                } else {
                    "packages"
                }
            ))
        );
        for line in version_change_lines(&changes) {
            println!("{line}");
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_upgrade_casks(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    opts: &install::InstallOpts,
    names: Vec<String>,
    prior_native_progress: bool,
) -> ExitCode {
    let mut failures = 0;
    let greedy = has_flag(rest, "--greedy", "");
    // Resolve every candidate up front, partition like brew's
    // outdated_casks: unknown-name errors, current-version and
    // :latest notices (named only), then pinned, then the summary.
    let mut upgradable: Vec<(super::formula::Cask, String)> = Vec::new();
    let mut pinned_rows: Vec<(String, String, String, Option<String>)> = Vec::new();
    let mut candidates: Vec<(String, String)> = Vec::new();
    if !names.is_empty() {
        for name in &names {
            let short = Formula::short_name(name);
            let cask_short = super::api::canonical_cask_token(short);
            let Some(inst) = prefix.installed_one(Kind::Cask, cask_short) else {
                super::tty::onoe!("Error: Cask '{cask_short}' is not installed.");
                failures += 1;
                continue;
            };
            candidates.push((cask_short.to_string(), inst.version));
        }
    } else {
        match outdated_rows(prefix, api, true, greedy) {
            Ok(rows) => candidates = rows.into_iter().map(|(n, have, _)| (n, have)).collect(),
            Err(error) => return failed(&error),
        }
    }
    for (token, have) in candidates {
        let c = match api.cask(&token) {
            Ok(c) => c,
            Err(error) => return from_install(prefix, args, Err(error.into())),
        };
        if super::formula::version_cmp(&have, &c.version) != std::cmp::Ordering::Less {
            if !names.is_empty() && !opts.quiet {
                if have == "latest" {
                    super::tty::opoo!(
                        "Warning: Not upgrading {token}, the downloaded artifact has not changed"
                    );
                } else {
                    super::tty::opoo!(
                        "Warning: Not upgrading {token}, the latest version is already installed"
                    );
                }
            }
            continue;
        }
        if prefix.cask_is_pinned(&token) {
            pinned_rows.push((token, have, c.version.clone(), None));
            continue;
        }
        upgradable.push((c, have));
    }
    if !pinned_rows.is_empty() {
        let count = pinned_rows.len();
        if !names.is_empty() {
            eprintln!(
                "Error: Not upgrading {count} pinned {}:",
                if count == 1 { "package" } else { "packages" }
            );
            failures += 1;
        } else if !opts.quiet {
            super::tty::opoo!(
                "Warning: Not upgrading {count} pinned {}:",
                if count == 1 { "package" } else { "packages" }
            );
        }
        if !opts.quiet {
            for line in version_change_lines(&pinned_rows) {
                eprintln!("{line}");
            }
        }
    }
    if upgradable.is_empty() {
        if failures == 0 {
            return ExitCode::SUCCESS;
        }
        return ExitCode::FAILURE;
    }
    if names.is_empty() && !greedy && !super::no_env_hints() {
        println!(
            "Some casks with `auto_updates true` or `version :latest` may still require `--greedy`,"
        );
        println!("`HOMEBREW_UPGRADE_GREEDY` or `HOMEBREW_UPGRADE_GREEDY_CASKS` to be upgraded.");
        println!("Hide these hints with `HOMEBREW_NO_ENV_HINTS=1` (see `man brew`).");
    }
    if !opts.quiet {
        let verb = if opts.dry_run {
            "Would upgrade"
        } else {
            "Upgrading"
        };
        println!(
            "==> {verb} {} outdated {}:",
            upgradable.len(),
            if upgradable.len() == 1 {
                "package"
            } else {
                "packages"
            }
        );
        let table: Vec<(String, String, String, Option<String>)> = upgradable
            .iter()
            .map(|(c, have)| (c.token.clone(), have.clone(), c.version.clone(), None))
            .collect();
        for line in version_change_lines(&table) {
            println!("{line}");
        }
    }
    if opts.dry_run {
        if failures == 0 {
            return ExitCode::SUCCESS;
        }
        return ExitCode::FAILURE;
    }
    let mut completed = prior_native_progress;
    for (c, have) in upgradable {
        let token = c.token.clone();
        if !opts.quiet {
            println!(
                "{}",
                super::tty::oh1(&format!("Upgrading {}", super::tty::ident(&token)))
            );
            println!("  {have} -> {}", c.version);
        }
        let result =
            cask::install_with_deps(api, prefix, &c, opts.dry_run, opts.force, true, false);
        completed |= super::native_attempt_may_have_progress(&result);
        match result {
            Ok(now) => {
                if !opts.dry_run
                    && now != have
                    && let Err(error) =
                        auto_cleanup_old_versions(prefix, api, false, &[token.as_str()])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(install::InstallError::Unsupported(reason)) => {
                if !names.is_empty() {
                    return from_install(
                        prefix,
                        args,
                        Err(super::after_native_progress(
                            install::InstallError::Unsupported(reason),
                            completed,
                        )),
                    );
                }
                eprintln!("abrew: {token}: {reason}; skipped");
                failures += 1;
            }
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(error)) => {
                eprintln!("abrew: {error}");
                failures += 1;
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct UpdateReport {
    new_formulae: Vec<(String, String)>,
    new_casks: Vec<(String, String)>,
    deleted_formulae: Vec<String>,
    deleted_casks: Vec<String>,
    outdated_formulae: Vec<String>,
    outdated_casks: Vec<String>,
}

fn update_report(
    old: Option<&[super::api::IndexRow]>,
    new: &[super::api::IndexRow],
    installed: &[Installed],
    new_report: bool,
) -> (Vec<(String, String)>, Vec<String>, Vec<String>) {
    let installed_names: std::collections::BTreeSet<&str> =
        installed.iter().map(|i| i.name.as_str()).collect();
    let (mut added, mut deleted) = (Vec::new(), Vec::new());
    if let Some(old) = old {
        let old_names: std::collections::BTreeSet<&str> =
            old.iter().map(|r| r.name.as_str()).collect();
        let new_names: std::collections::BTreeSet<&str> =
            new.iter().map(|r| r.name.as_str()).collect();
        for row in new {
            if !old_names.contains(row.name.as_str())
                && !installed_names.contains(row.name.as_str())
            {
                added.push((row.name.clone(), row.desc.clone()));
            }
        }
        for row in old {
            if !new_names.contains(row.name.as_str()) && installed_names.contains(row.name.as_str())
            {
                deleted.push(row.name.clone());
            }
        }
        added.sort();
        deleted.sort();
        if !new_report {
            added.clear();
        }
    }
    let latest: std::collections::BTreeMap<&str, &super::api::IndexRow> =
        new.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut outdated = Vec::new();
    for inst in installed {
        if let Some(row) = latest.get(inst.name.as_str())
            && !row.auto_updates
            && !row.version.is_empty()
            && version_cmp(&inst.version, &row.version) == Ordering::Less
        {
            outdated.push(inst.name.clone());
        }
    }
    outdated.sort();
    (added, deleted, outdated)
}

fn print_update_report(r: &UpdateReport) -> bool {
    let mut printed = false;
    let new_section = |title: &str, rows: &[(String, String)], printed: &mut bool| {
        if rows.is_empty() {
            return;
        }
        *printed = true;
        println!("{}", super::tty::oh1(title));
        for (name, desc) in rows {
            if desc.is_empty() {
                println!("{name}");
            } else {
                println!("{name}: {desc}");
            }
        }
    };
    let name_section = |title: &str, rows: &[String], printed: &mut bool| {
        if rows.is_empty() {
            return;
        }
        *printed = true;
        println!("{}", super::tty::oh1(title));
        for name in rows {
            println!("{name}");
        }
    };
    new_section("New Formulae", &r.new_formulae, &mut printed);
    new_section("New Casks", &r.new_casks, &mut printed);
    name_section(
        "Deleted Installed Formulae",
        &r.deleted_formulae,
        &mut printed,
    );
    name_section("Deleted Installed Casks", &r.deleted_casks, &mut printed);
    name_section("Outdated Formulae", &r.outdated_formulae, &mut printed);
    name_section("Outdated Casks", &r.outdated_casks, &mut printed);
    let (of, oc) = (r.outdated_formulae.len(), r.outdated_casks.len());
    if of + oc > 0 {
        let mut msg = String::new();
        if of > 0 {
            msg += &format!("{of} outdated formula{}", if of == 1 { "" } else { "e" });
        }
        if oc > 0 {
            if !msg.is_empty() {
                msg += " and ";
            }
            msg += &format!("{oc} outdated cask{}", if oc == 1 { "" } else { "s" });
        }
        let pronoun = if of + oc == 1 { "it" } else { "them" };
        let upgrade = match (of, oc) {
            (_, 0) => "brew upgrade",
            (0, _) => "brew upgrade --cask",
            _ => "brew upgrade and brew upgrade --cask",
        };
        println!();
        println!("You have {msg} installed.");
        println!("You can upgrade {pronoun} with {upgrade}");
        println!("or list {pronoun} with brew outdated.");
    }
    printed
}

pub(super) fn cmd_update(prefix: &Prefix, api: &Api) -> ExitCode {
    let old_f = api.cached_index_rows(false);
    let old_c = api.cached_index_rows(true);
    let mut tap_failures = 0usize;
    for t in tap::list(prefix) {
        if let Err(error) = tap::update(prefix, &t) {
            eprintln!(
                "abrew: warning: {}",
                error.strip_prefix("abrew: ").unwrap_or(&error)
            );
            tap_failures += 1;
        }
    }
    let staged = (api.index_rows_fresh(false), api.index_rows_fresh(true));
    if let (Ok(_), Ok(_)) = &staged {
        api.refresh_except(&["formula-index.json", "cask-index.json"]);
    }
    match staged {
        (Ok(f), Ok(c)) => {
            let report_new = std::env::var_os("HOMEBREW_NO_UPDATE_REPORT_NEW").is_none();
            let inst_f = prefix.installed(Kind::Formula);
            let inst_c = prefix.installed(Kind::Cask);
            let (new_f, del_f, out_f) = update_report(old_f.as_deref(), &f, &inst_f, report_new);
            let (new_c, del_c, out_c) = update_report(
                old_c.as_deref(),
                &c,
                &inst_c,
                report_new && !inst_c.is_empty(),
            );
            let report = UpdateReport {
                new_formulae: new_f,
                new_casks: new_c,
                deleted_formulae: del_f,
                deleted_casks: del_c,
                outdated_formulae: out_f,
                outdated_casks: out_c,
            };
            if !print_update_report(&report) {
                println!("Already up-to-date.");
            }
            if tap_failures > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        (Err(e), _) | (_, Err(e)) => failed(&e),
    }
}

fn remove_links_into(prefix: &Prefix, dir: &std::path::Path) {
    fn walk(root: &std::path::Path, dir: &std::path::Path, depth: u8) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(&path)
                    && install::link_target_points_into(&path, &target, dir)
                {
                    let _ = std::fs::remove_file(&path);
                }
            } else if meta.is_dir() {
                walk(&path, dir, depth - 1);
            }
        }
    }
    for name in [
        "bin",
        "sbin",
        "lib",
        "include",
        "share",
        "etc",
        "Applications",
        "Library",
    ] {
        walk(&prefix.root.join(name), dir, 8);
    }
}

fn auto_cleanup_old_versions(
    prefix: &Prefix,
    api: &Api,
    dry: bool,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    if super::no_install_cleanup() {
        return Ok((0, 0));
    }
    cleanup_old_versions(prefix, api, dry, scope)
}

fn disk_usage_readable(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB"] {
        if (size * 10.0).round() / 10.0 < 1000.0 {
            break;
        }
        size /= 1000.0;
        unit = next;
    }
    let rounded = (size * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{}{unit}", rounded as u64)
    } else {
        format!("{rounded:.1}{unit}")
    }
}

/// (dev, ino) for hardlink de-duplication; platforms without
/// inode numbers count every link.
fn hardlink_key(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn dir_disk_usage(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut seen: std::collections::BTreeSet<(u64, u64)> = std::collections::BTreeSet::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            // Brew counts each directory entry's own size too.
            total = total.saturating_add(meta.len());
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if hardlink_key(&meta).is_none_or(|key| seen.insert(key)) {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Brew's cleanup prune pass (`prune_prefix_symlinks_and_directories`):
/// broken symlinks and then-empty non-anchor directories leave with
/// a bare cleanup. Anchor directories themselves always stay.
fn prune_prefix_links(prefix: &Prefix, dry: bool) {
    const ANCHORS: &[&str] = &[
        "bin",
        "etc",
        "include",
        "lib",
        "sbin",
        "share",
        "opt",
        "var/homebrew/linked",
    ];
    let mut links = 0usize;
    let mut dirs_removed = 0usize;
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for anchor in ANCHORS {
        let root = prefix.root.join(anchor);
        if root.is_dir() {
            walk_prune_links(&root, dry, &mut links, &mut subdirs);
        }
    }
    let caskroom = prefix.caskroom();
    if let Ok(entries) = std::fs::read_dir(&caskroom) {
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_symlink()) && !path.exists() {
                if dry {
                    println!("Would remove (broken link): {}", path.display());
                } else if std::fs::remove_file(&path).is_ok() {
                    links += 1;
                }
            }
        }
    }
    subdirs.sort();
    for dir in subdirs.iter().rev() {
        let empty = std::fs::read_dir(dir).is_ok_and(|mut it| it.next().is_none());
        if !empty {
            continue;
        }
        if dry {
            println!("Would remove (empty directory): {}", dir.display());
        } else if std::fs::remove_dir(dir).is_ok() {
            dirs_removed += 1;
        }
    }
    if !dry && (links > 0 || dirs_removed > 0) {
        if dirs_removed > 0 {
            println!(
                "Pruned {links} symbolic links and {dirs_removed} directories from {}",
                prefix.root.display()
            );
        } else {
            println!(
                "Pruned {links} symbolic links from {}",
                prefix.root.display()
            );
        }
    }
}

fn walk_prune_links(
    dir: &std::path::Path,
    dry: bool,
    links: &mut usize,
    subdirs: &mut Vec<std::path::PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            if !path.exists() {
                if dry {
                    println!("Would remove (broken link): {}", path.display());
                } else if std::fs::remove_file(&path).is_ok() {
                    *links += 1;
                }
            }
        } else if ft.is_dir() {
            subdirs.push(path.clone());
            walk_prune_links(&path, dry, links, subdirs);
        }
    }
}

/// Every broken symlink under the prefix link trees (doctor's
/// `check_for_broken_symlinks`, same walk as the cleanup prune).
fn broken_prefix_links(prefix: &Prefix) -> Vec<std::path::PathBuf> {
    const ANCHORS: &[&str] = &[
        "bin",
        "etc",
        "include",
        "lib",
        "sbin",
        "share",
        "opt",
        "var/homebrew/linked",
    ];
    let mut out = Vec::new();
    for anchor in ANCHORS {
        let root = prefix.root.join(anchor);
        if root.is_dir() {
            collect_broken_links(&root, &mut out);
        }
    }
    out.sort();
    out
}

fn collect_broken_links(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            if !path.exists() {
                out.push(path);
            }
        } else if ft.is_dir() {
            collect_broken_links(&path, out);
        }
    }
}

fn cleanup_old_versions(
    prefix: &Prefix,
    api: &Api,
    dry: bool,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    let mut removed = 0;
    let mut freed = 0u64;
    let protected = no_cleanup_names();
    for kind in [Kind::Formula, Kind::Cask] {
        for i in prefix.installed(kind) {
            if kind == Kind::Formula && no_cleanup_formula(api, &i.name, &protected) {
                continue;
            }
            let active = prefix
                .opt(&i.name)
                .read_link()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            if !scope.is_empty() && !scope.iter().any(|name| Formula::short_name(name) == i.name) {
                continue;
            }
            let pinned = (kind == Kind::Formula)
                .then(|| prefix.pinned_version(&i.name))
                .flatten();
            for v in &i.versions {
                if *v != i.version
                    && pinned.as_deref() != Some(v.as_str())
                    && active.as_deref() != Some(v.as_str())
                {
                    let dir = match kind {
                        Kind::Formula => prefix.keg(&i.name, v),
                        Kind::Cask => prefix.cask_dir(&i.name, v),
                    };
                    if !super::prefix::owned_receipt(&dir, kind, &i.name, v) {
                        continue;
                    }
                    let usage = dir_disk_usage(&dir);
                    if dry {
                        println!(
                            "Would remove: {} ({})",
                            dir.display(),
                            disk_usage_readable(usage)
                        );
                    } else {
                        println!(
                            "Removing: {}... ({})",
                            dir.display(),
                            disk_usage_readable(usage)
                        );
                        remove_links_into(prefix, &dir);
                        std::fs::remove_dir_all(&dir)
                            .map_err(|e| format!("abrew: cannot remove {}: {e}", dir.display()))?;
                    }
                    freed = freed.saturating_add(usage);
                    removed += 1;
                }
            }
        }
    }
    Ok((removed, freed))
}

fn prune_downloads(
    prefix: &Prefix,
    dry: bool,
    max_age: Option<std::time::Duration>,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    if !scope.is_empty() {
        return Ok((0, 0));
    }
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    let mut freed = 0u64;
    let entries = match std::fs::read_dir(prefix.cache()) {
        Ok(entries) => entries,
        Err(_) => return Ok((0, 0)),
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .is_ok_and(|t| t.is_file() && !t.is_symlink())
        {
            continue;
        }
        if let Some(max) = max_age {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= max);
            if !old {
                continue;
            }
        }
        let path = entry.path();
        let usage = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if dry {
            println!(
                "Would remove: {} ({})",
                path.display(),
                disk_usage_readable(usage)
            );
        } else {
            println!(
                "Removing: {}... ({})",
                path.display(),
                disk_usage_readable(usage)
            );
            std::fs::remove_file(&path)
                .map_err(|e| format!("abrew: cannot remove {}: {e}", path.display()))?;
        }
        freed = freed.saturating_add(usage);
        removed += 1;
    }
    Ok((removed, freed))
}

/// Parse and validate `--prune` cache age options before running cleanup.
pub(super) fn cleanup_max_age(
    rest: &[String],
) -> Result<Option<Option<std::time::Duration>>, &'static str> {
    const INVALID: &str = "cleanup --prune takes all or a whole number of days";
    let mut max_age = None;
    for value in rest.iter().filter_map(|a| a.strip_prefix("--prune=")) {
        let age = if value == "all" {
            None
        } else {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(INVALID);
            }
            let seconds = value
                .parse::<u64>()
                .ok()
                .and_then(|days| days.checked_mul(86_400))
                .ok_or(INVALID)?;
            Some(std::time::Duration::from_secs(seconds))
        };
        if max_age.is_none() {
            max_age = Some(age);
        }
    }
    Ok(if has_flag(rest, "--scrub", "-s") {
        Some(None)
    } else {
        max_age
    })
}

#[derive(Debug, PartialEq, Eq)]
enum UnpinOutcome {
    Remove,
    WarnNotPinned,
    ErrorNotInstalled,
    FailUnknown,
}

const fn unpin_outcome(pinned: bool, installed: bool, definitively_unknown: bool) -> UnpinOutcome {
    if pinned {
        UnpinOutcome::Remove
    } else if installed {
        UnpinOutcome::WarnNotPinned
    } else if definitively_unknown {
        UnpinOutcome::FailUnknown
    } else {
        UnpinOutcome::ErrorNotInstalled
    }
}

pub(super) fn cmd_unpin(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    if operands(rest).is_empty() {
        eprintln!(
            "Error: Invalid usage: This command requires at least 1 installed formula or installed cask argument."
        );
        return ExitCode::from(1);
    }
    let mut failures = 0;
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !super::valid_component(short) {
            eprintln!("abrew: invalid formula name {short}");
            failures += 1;
            continue;
        }
        let token = super::api::canonical_cask_token(short);
        // A kind switch restricts resolution the way brew's
        // only_formula_or_cask does.
        let want_cask = has_flag(rest, "--cask", "");
        let want_formula = has_flag(rest, "--formula", "");
        let pinned = (!want_cask
            && (prefix.pinned_version(short).is_some() || prefix.pin_link_dangling_ours(short)))
            || (!want_formula && prefix.cask_pin_link_present(token));
        let installed = (!want_cask && prefix.installed_one(Kind::Formula, short).is_some())
            || (!want_formula && prefix.installed_one(Kind::Cask, token).is_some());
        let unknown = !pinned
            && !installed
            && package_definitively_unknown(prefix, api, short, !want_cask, !want_formula);
        let display = if want_cask { token } else { short };
        match unpin_outcome(pinned, installed, unknown) {
            UnpinOutcome::Remove => {
                let removed = if prefix.cask_pin_link_present(token)
                    && prefix.pinned_version(short).is_none()
                    && !prefix.pin_link_dangling_ours(short)
                {
                    prefix.unpin_cask(token)
                } else {
                    prefix.unpin(short)
                };
                if let Err(error) = removed {
                    eprintln!("{error}");
                    failures += 1;
                }
            }
            UnpinOutcome::WarnNotPinned => super::tty::opoo!("Warning: {display} not pinned"),
            UnpinOutcome::ErrorNotInstalled => {
                super::tty::onoe!("Error: {display} not installed")
            }
            UnpinOutcome::FailUnknown => {
                if want_cask {
                    super::tty::onoe!("Error: {}", cask_unavailable(token));
                } else {
                    super::tty::onoe!("Error: {}", formula_unavailable(api, short));
                }
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_cleanup(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let max_age = match cleanup_max_age(rest) {
        Ok(age) => age,
        Err(error) => return usage(error),
    };
    let dry = has_flag(rest, "--dry-run", "-n");
    let named = operands(rest);
    let protected = no_cleanup_names();
    let mut refused = false;
    let scope: Vec<&str> = named
        .iter()
        .map(|s| super::api::canonical_cask_token(Formula::short_name(s)))
        .filter(|short| {
            if no_cleanup_formula(api, short, &protected) {
                eprintln!(
                    "Error: Refusing to clean {short} because it is listed in HOMEBREW_NO_CLEANUP_FORMULAE!"
                );
                refused = true;
                false
            } else {
                true
            }
        })
        .collect();
    let mut freed = match cleanup_old_versions(prefix, api, dry, &scope) {
        Ok((_, bytes)) => bytes,
        Err(error) => return failed(&error),
    };
    if let Some(max_age) = max_age {
        match prune_downloads(prefix, dry, max_age, &scope) {
            Ok((_, bytes)) => freed = freed.saturating_add(bytes),
            Err(error) => return failed(&error),
        }
    }
    if named.is_empty() {
        prune_prefix_links(prefix, dry);
    }
    if freed > 0 {
        if dry {
            println!(
                "{}",
                super::tty::ohai(&format!(
                    "This operation would free approximately {} of disk space.",
                    disk_usage_readable(freed)
                ))
            );
        } else {
            println!(
                "{}",
                super::tty::ohai(&format!(
                    "This operation has freed approximately {} of disk space.",
                    disk_usage_readable(freed)
                ))
            );
        }
    }
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

const MAX_REQUEST_RECEIPT_BYTES: usize = 64 * 1024;

/// Return whether any installed version of `i` was explicitly requested.
pub(super) fn requested(
    prefix: &Prefix,
    installed: &super::prefix::Installed,
) -> Result<bool, String> {
    let mut any = false;
    for version in &installed.versions {
        let file = prefix
            .keg(&installed.name, version)
            .join("INSTALL_RECEIPT.json");
        let bytes = crate::metadata_io::read_bounded_regular(&file, MAX_REQUEST_RECEIPT_BYTES)
            .map_err(|e| format!("abrew: cannot read request receipt {}: {e}", file.display()))?;
        let value: aterm_json::Value = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("abrew: bad receipt {}: {e}", file.display()))?;
        match value
            .get("installed_on_request")
            .and_then(aterm_json::Value::as_str)
        {
            Some("true") => any = true,
            Some("false") => {}
            _ => {
                return Err(format!(
                    "abrew: unknown request status in {}",
                    file.display()
                ));
            }
        }
    }
    Ok(any)
}

/// `HOMEBREW_NO_CLEANUP_FORMULAE` entries (comma-separated short
/// names): formulae cleanup and autoremove must never take.
fn no_cleanup_names() -> Vec<String> {
    std::env::var("HOMEBREW_NO_CLEANUP_FORMULAE")
        .unwrap_or_default()
        .split(',')
        .map(|n| Formula::short_name(n.trim()).to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Whether a formula is protected by that list, by name or alias
/// (brew's Cleanup.skip_clean_formula?).
fn no_cleanup_formula(api: &Api, name: &str, protected: &[String]) -> bool {
    if protected.iter().any(|p| p == name) {
        return true;
    }
    api.formula(name)
        .is_ok_and(|f| f.aliases.iter().any(|a| protected.contains(a)))
}

fn autoremove_targets(
    prefix: &Prefix,
    read: impl FnMut(&str) -> Result<Formula, String>,
) -> Result<Vec<String>, String> {
    let installed = prefix.installed_checked(Kind::Formula)?;
    // HOMEBREW_NO_CLEANUP_FORMULAE names formulae (and thereby
    // their dependencies) autoremove must never take.
    let protected = no_cleanup_names();
    let mut roots = Vec::new();
    let mut pinned = std::collections::BTreeSet::new();
    for i in &installed {
        let pin = prefix.is_pinned_checked(&i.name)?;
        if pin {
            pinned.insert(i.name.clone());
        }
        if requested(prefix, i)? || pin || protected.contains(&i.name) {
            roots.push(i.name.clone());
        }
    }
    let books = install::dependency_books(&installed, read)?;
    let mut needed = std::collections::BTreeSet::new();
    while let Some(name) = roots.pop() {
        if !needed.insert(name.clone()) {
            continue;
        }
        let (_, formula) = books
            .iter()
            .find(|(i, _)| i.name == name)
            .ok_or_else(|| format!("abrew: cannot complete installed dependency {name}"))?;
        roots.extend(
            formula
                .dependencies
                .iter()
                .map(|d| Formula::short_name(d).to_string()),
        );
    }
    let mut remaining: std::collections::BTreeSet<_> = installed
        .iter()
        .filter(|i| !needed.contains(&i.name) && !pinned.contains(&i.name))
        .map(|i| i.name.clone())
        .collect();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .find(|candidate| {
                !books.iter().any(|(i, f)| {
                    remaining.contains(&i.name)
                        && i.name != **candidate
                        && f.dependencies
                            .iter()
                            .any(|d| Formula::short_name(d) == candidate.as_str())
                })
            })
            .cloned()
            .ok_or_else(|| {
                "abrew: cyclic orphan dependencies; automatic deletion refused".to_string()
            })?;
        remaining.remove(&next);
        ordered.push(next);
    }
    Ok(ordered)
}

pub(super) fn cmd_autoremove(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let dry = has_flag(rest, "--dry-run", "-n");
    let targets = match autoremove_targets(prefix, |n| api.dependency_formula(n)) {
        Ok(targets) => targets,
        Err(e) => return failed(&e),
    };
    if targets.is_empty() {
        // Brew says nothing when nothing is removable.
        return ExitCode::SUCCESS;
    }
    let mut names: Vec<String> = targets
        .iter()
        .map(|n| {
            prefix
                .installed_one(Kind::Formula, n)
                .map(|i| prefix.specified_name(&i))
                .unwrap_or_else(|| n.clone())
        })
        .collect();
    names.sort();
    println!(
        "==> {} {} unneeded {}:",
        if dry {
            "Would autoremove"
        } else {
            "Autoremoving"
        },
        names.len(),
        if names.len() == 1 {
            "formula"
        } else {
            "formulae"
        }
    );
    for name in &names {
        println!("{name}");
    }
    if dry {
        return ExitCode::SUCCESS;
    }
    for name in targets {
        if let Some(inst) = prefix.installed_one(Kind::Formula, &name) {
            let targets_v = install::uninstall_versions(prefix, &inst, false);
            let keg = prefix.keg(&name, &targets_v[0]);
            println!("Uninstalling {}... ({})", keg.display(), keg_abv(&keg));
        }
        if let Err(e) = install::uninstall(api, prefix, &name, false, false, false) {
            return from_install(prefix, &[], Err(e));
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_link(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    link: bool,
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("link needs a formula name");
    }
    let dry_run = has_flag(rest, "--dry-run", "-n");
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    // Cask artifact linking is not native; refuse before any
    // name is (un)linked when a named cask is installed.
    if !want_formula {
        for name in &names {
            let token = super::api::canonical_cask_token(Formula::short_name(name));
            if prefix.installed_one(Kind::Cask, token).is_some() {
                return super::from_install(
                    prefix,
                    args,
                    Err(install::InstallError::Unsupported(
                        "cask artifact linking is not native".to_string(),
                    )),
                );
            }
        }
    }
    for name in names {
        let short = Formula::short_name(&name);
        if want_cask {
            // An installed cask was refused above; what remains is
            // brew's cask resolution failure.
            let token = super::api::canonical_cask_token(short);
            if package_definitively_unknown(prefix, api, token, false, true) {
                return failed(&format!("Error: {}", cask_unavailable(token)));
            }
            return failed(&format!("Error: Cask '{token}' is not installed."));
        }
        if !link {
            let inst = prefix.installed_one(Kind::Formula, short);
            if dry_run {
                if inst.is_none() {
                    return failed(&absent_error(
                        api,
                        prefix,
                        short,
                        no_such_keg(prefix, short),
                    ));
                }
                match install::unlink_targets(prefix, short) {
                    Ok(mut targets) => {
                        targets.sort();
                        println!("Would remove:");
                        for t in targets {
                            println!("{}", t.display());
                        }
                    }
                    Err(e) => return failed(&e),
                }
                continue;
            }
            let installed = inst.is_some();
            let shown = match &inst {
                Some(i) => prefix.keg(short, &i.version).display().to_string(),
                None => short.to_string(),
            };
            match install::unlink_keg(prefix, short) {
                Ok(0) if !installed => match install::remove_dangling_links(prefix, short) {
                    Ok(0) => {
                        return failed(&absent_error(
                            api,
                            prefix,
                            short,
                            no_such_keg(prefix, short),
                        ));
                    }
                    Ok(n) => println!("Unlinking {shown}... {n} symlinks removed."),
                    Err(e) => return failed(&e),
                },
                Ok(n) => println!("Unlinking {shown}... {n} symlinks removed."),
                Err(e) => return failed(&e),
            }
            continue;
        }
        let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
            return failed(&absent_error(
                api,
                prefix,
                short,
                no_such_keg(prefix, short),
            ));
        };
        let f = match api.formula(short) {
            Ok(f) => f,
            Err(e) => return failed(&e),
        };
        if f.keg_only && !has_flag(rest, "--force", "-f") {
            println!("{short} is keg-only; refreshing its opt link only");
        }
        let keg = prefix.keg(short, &inst.version);
        if dry_run && !f.keg_only && install::is_linked(prefix, short, &inst.version) {
            super::tty::opoo!("Warning: Already linked: {}", keg.display());
            println!("To relink, run:");
            println!("  brew unlink {short} && brew link {short}");
            continue;
        }
        if dry_run {
            match install::link_destinations(prefix, &f) {
                Ok(mut targets) => {
                    targets.sort();
                    println!("Would link:");
                    for t in targets {
                        println!("{}", t.display());
                    }
                }
                Err(e) => return from_install(prefix, args, Err(e)),
            }
            continue;
        }
        if !f.keg_only && install::is_linked(prefix, short, &inst.version) {
            super::tty::opoo!("Warning: Already linked: {}", keg.display());
            println!("To relink, run:");
            println!("  brew unlink {short} && brew link {short}");
            continue;
        }
        match install::relink(prefix, &f) {
            Ok(n) => println!("Linking {}... {n} symlinks created.", keg.display()),
            Err(e) => return from_install(prefix, args, Err(e)),
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_switch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    let (Some(name), Some(version)) = (ops.first(), ops.get(1)) else {
        return usage("switch needs <formula> <version>");
    };
    let short = Formula::short_name(name);
    if !super::valid_component(short) || !super::valid_cask_version(version) {
        return failed("abrew: invalid formula name or version");
    }
    let f = match api.formula(short) {
        Ok(f) => f,
        Err(e) => return failed(&e),
    };
    if let Err(e) = install::switch_keg(prefix, short, version, f.keg_only) {
        return from_install(prefix, args, Err(e));
    }
    println!("{short} now at {version}");
    ExitCode::SUCCESS
}

#[cfg(unix)]
struct LongLister {
    users: std::collections::BTreeMap<u32, String>,
    groups: std::collections::BTreeMap<u32, String>,
    offsets: std::collections::BTreeMap<i64, i64>,
    now: i64,
}

#[cfg(unix)]
impl LongLister {
    fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        LongLister {
            users: Default::default(),
            groups: Default::default(),
            offsets: Default::default(),
            now,
        }
    }

    fn offset_at(&mut self, secs: i64) -> i64 {
        *self
            .offsets
            .entry(secs)
            .or_insert_with(|| aterm_uds::zone::local_offset_at(secs).unwrap_or(0))
    }

    fn date(&mut self, mtime: i64) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let local = i128::from(mtime) + i128::from(self.offset_at(mtime));
        let days = i64::try_from(local.div_euclid(86_400)).expect("local seconds fit i64 days");
        let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
        let month = MONTHS
            .get(usize::try_from(month - 1).unwrap_or(0))
            .copied()
            .unwrap_or("Jan");
        let age = i128::from(self.now) - i128::from(mtime);
        if !(-3_600..=15_552_000).contains(&age) {
            format!("{month} {day:>2}  {year}")
        } else {
            let secs = local.rem_euclid(86_400);
            format!("{month} {day:>2} {:02}:{:02}", secs / 3_600, secs / 60 % 60)
        }
    }

    fn user(&mut self, uid: u32) -> String {
        self.users
            .entry(uid)
            .or_insert_with(|| {
                aterm_uds::account::user_name(uid).unwrap_or_else(|| uid.to_string())
            })
            .clone()
    }

    fn group(&mut self, gid: u32) -> String {
        self.groups
            .entry(gid)
            .or_insert_with(|| gid.to_string())
            .clone()
    }

    fn line(&mut self, path: &std::path::Path) -> String {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return format!("?????????? {}", path.display());
        };
        let user = self.user(meta.uid());
        let group = self.group(meta.gid());
        let date = self.date(meta.mtime());
        let mut shown = path.display().to_string();
        if meta.file_type().is_symlink()
            && let Ok(target) = std::fs::read_link(path)
        {
            shown = format!("{shown} -> {}", target.display());
        }
        format!(
            "{} {:>2} {} {} {:>8} {} {}",
            mode_string(&meta),
            meta.nlink(),
            user,
            group,
            meta.len(),
            date,
            shown
        )
    }
}

#[cfg(unix)]
fn mode_string(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let mode = meta.mode();
    let kind = if meta.is_dir() {
        'd'
    } else if meta.file_type().is_symlink() {
        'l'
    } else {
        '-'
    };
    let triplet = |shift: u32, special: u32, mark: char| -> [char; 3] {
        let bits = (mode >> shift) & 0o7;
        let exec = if mode & special != 0 {
            if bits & 0o1 != 0 {
                mark.to_ascii_lowercase()
            } else {
                mark
            }
        } else if bits & 0o1 != 0 {
            'x'
        } else {
            '-'
        };
        [
            if bits & 0o4 != 0 { 'r' } else { '-' },
            if bits & 0o2 != 0 { 'w' } else { '-' },
            exec,
        ]
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    for chars in [
        triplet(6, 0o4000, 'S'),
        triplet(3, 0o2000, 'S'),
        triplet(0, 0o1000, 'T'),
    ] {
        out.extend(chars);
    }
    out
}

fn list_json(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let opt_str = |v: Option<String>| match v {
        Some(s) => aterm_json::Value::String(s),
        None => aterm_json::Value::Null,
    };
    let version_values = |i: &Installed| {
        let mut vs = i.versions.clone();
        vs.sort();
        vs.dedup();
        vs.into_iter().map(aterm_json::Value::String).collect()
    };
    let mut formulae = Vec::new();
    if !has_flag(rest, "--cask", "") {
        for i in prefix.installed(Kind::Formula) {
            let opt = prefix.optlinked_version(&i.name);
            let linked = linked_keg_version(prefix, &i);
            let mut map = aterm_json::Map::new();
            map.insert("name".into(), aterm_json::Value::String(i.name.clone()));
            map.insert(
                "versions".into(),
                aterm_json::Value::Array(version_values(&i)),
            );
            map.insert("linked_version".into(), opt_str(linked));
            map.insert("optlinked_version".into(), opt_str(opt));
            map.insert(
                "pinned_version".into(),
                opt_str(prefix.pinned_version(&i.name)),
            );
            formulae.push(aterm_json::Value::Object(map));
        }
    }
    let mut casks = Vec::new();
    if !has_flag(rest, "--formula", "") {
        for i in prefix.installed(Kind::Cask) {
            let mut map = aterm_json::Map::new();
            map.insert("token".into(), aterm_json::Value::String(i.name.clone()));
            map.insert(
                "versions".into(),
                aterm_json::Value::Array(version_values(&i)),
            );
            map.insert("pinned_version".into(), aterm_json::Value::Null);
            casks.push(aterm_json::Value::Object(map));
        }
    }
    let mut root = aterm_json::Map::new();
    root.insert("formulae".into(), aterm_json::Value::Array(formulae));
    root.insert("casks".into(), aterm_json::Value::Array(casks));
    match aterm_json::to_string(&aterm_json::Value::Object(root)) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => failed(&format!("abrew: cannot encode list JSON: {e}")),
    }
}

fn multiple_version_lines(rows: &[Installed]) -> Vec<String> {
    let mut rows: Vec<&Installed> = rows.iter().filter(|i| i.versions.len() >= 2).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows.iter()
        .map(|i| format!("{} {}", i.name, i.versions.join(" ")))
        .collect()
}

fn list_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(d) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    walk(&e.path(), out);
                } else if e.file_name() != ".DS_Store" {
                    out.push(e.path());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// `list <name>` for an absent package: unknown names take the
/// unavailable errors; a known formula is brew's missing keg,
/// and a known cask is brew's `find` failure over the caskroom.
fn list_absent_error(prefix: &Prefix, api: &Api, kind: Kind, short: &str, lookup: &str) -> String {
    match kind {
        Kind::Formula => absent_error(api, prefix, short, no_such_keg(prefix, short)),
        Kind::Cask => {
            if package_definitively_unknown(prefix, api, short, false, true) {
                format!("Error: {}", cask_unavailable(lookup))
            } else {
                let dir = prefix.caskroom().join(lookup);
                format!(
                    "Error: Failure while executing; `/usr/bin/env find {} -not -type d -not -name .DS_Store -print` exited with 1. Here's the output:\nfind: {}: No such file or directory\nfind: {}: No such file or directory",
                    dir.display(),
                    dir.display(),
                    dir.display()
                )
            }
        }
    }
}

pub(super) fn cmd_list(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    if has_flag(rest, "--json", "") && !has_flag(rest, "--versions", "") {
        return super::usage_failed(
            "list",
            "Error: Invalid usage: `brew list --json` requires `--versions`.",
        );
    }
    if has_flag(rest, "--json", "") {
        return list_json(prefix, rest);
    }
    let versions = has_flag(rest, "--versions", "");
    let pinned = has_flag(rest, "--pinned", "");
    let multiple = has_flag(rest, "--multiple", "");
    let full_name = has_flag(rest, "--full-name", "");
    let long = has_flag(rest, "--long", "-l");
    let named = operands(rest);
    if multiple {
        let mut failed_any = false;
        let rows: Vec<Installed> = if named.is_empty() {
            prefix.installed(Kind::Formula)
        } else {
            named
                .iter()
                .filter_map(
                    |n| match prefix.installed_one(Kind::Formula, Formula::short_name(n)) {
                        Some(i) => Some(i),
                        None => {
                            failed_any = true;
                            None
                        }
                    },
                )
                .collect()
        };
        for line in multiple_version_lines(&rows) {
            println!("{line}");
        }
        return if failed_any {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    if pinned && !named.is_empty() {
        // Brew's named pinned listing: a pinned entry per name
        // found in the Cellar/Caskroom, a warning for found but
        // unpinned, a silent failure for absent names; entries
        // print sorted.
        let want_cask = has_flag(rest, "--cask", "");
        let want_formula = has_flag(rest, "--formula", "");
        let mut entries: Vec<String> = Vec::new();
        let mut failed_any = false;
        for name in &named {
            let short = Formula::short_name(name);
            let token = super::api::canonical_cask_token(short);
            let mut entry: Option<String> = None;
            let mut package_name: Option<&str> = None;
            if !want_cask && prefix.cellar().join(short).exists() {
                package_name = Some(short);
                entry = prefix.pinned_version(short).map(|v| {
                    if versions {
                        format!("{short} {v}")
                    } else {
                        short.to_string()
                    }
                });
            }
            if !want_formula && prefix.caskroom().join(token).symlink_metadata().is_ok() {
                if package_name.is_none() {
                    package_name = Some(token);
                }
                if entry.is_none() {
                    entry = prefix.cask_pinned_version(token).map(|v| {
                        if versions {
                            format!("{token} {v}")
                        } else {
                            token.to_string()
                        }
                    });
                }
            }
            match entry {
                Some(entry) => entries.push(entry),
                None => match package_name {
                    Some(found) => super::tty::opoo!("Warning: {found} not pinned"),
                    None => failed_any = true,
                },
            }
        }
        entries.sort();
        for entry in entries {
            println!("{entry}");
        }
        return if failed_any {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    if versions && !named.is_empty() {
        let mut rows = Vec::new();
        let mut any_absent = false;
        for name in &named {
            let short = Formula::short_name(name);
            let cask_short = super::api::canonical_cask_token(short);
            let kind = if has_flag(rest, "--cask", "")
                || (!has_flag(rest, "--formula", "")
                    && prefix.installed_one(Kind::Formula, short).is_none()
                    && prefix.installed_one(Kind::Cask, cask_short).is_some())
            {
                Kind::Cask
            } else {
                Kind::Formula
            };
            let lookup = if kind == Kind::Cask {
                cask_short
            } else {
                short
            };
            match prefix.installed_one(kind, lookup) {
                Some(inst) => rows.push(inst),
                None => any_absent = true,
            }
        }
        for inst in rows {
            println!("{} {}", inst.name, inst.versions.join(" "));
        }
        return if any_absent {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    if !named.is_empty() {
        let _ = long;
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        let mut seen: Vec<(Kind, String)> = Vec::new();
        for name in &named {
            let short = Formula::short_name(name).to_string();
            let cask_short = super::api::canonical_cask_token(&short).to_string();
            let kind = if has_flag(rest, "--cask", "")
                || (!has_flag(rest, "--formula", "")
                    && prefix.installed_one(Kind::Formula, &short).is_none()
                    && prefix.installed_one(Kind::Cask, &cask_short).is_some())
            {
                Kind::Cask
            } else {
                Kind::Formula
            };
            let lookup = if kind == Kind::Cask {
                &cask_short
            } else {
                &short
            };
            let Some(inst) = prefix.installed_one(kind, lookup) else {
                return failed(&list_absent_error(prefix, api, kind, &short, lookup));
            };
            if seen.contains(&(kind, lookup.clone())) {
                continue;
            }
            seen.push((kind, lookup.clone()));
            match kind {
                Kind::Formula => {
                    // Brew lists the current (opt-linked) keg only.
                    let current = prefix
                        .optlinked_version(&short)
                        .or_else(|| linked_keg_version(prefix, &inst))
                        .unwrap_or_else(|| inst.version.clone());
                    roots.push(prefix.keg(&short, &current));
                }
                Kind::Cask => roots.push(prefix.caskroom().join(&cask_short)),
            }
        }
        for root in &roots {
            for path in list_files(root) {
                println!("{}", path.display());
            }
        }
        return ExitCode::SUCCESS;
    }
    for casks in [false, true] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        let kind = if casks { Kind::Cask } else { Kind::Formula };
        #[cfg(unix)]
        let mut lister = long.then(LongLister::new);
        // Plain name listings go through brew's column layout on a
        // terminal (lines when piped or with -1). Brew's pinned
        // listing is plain lines even on a terminal.
        let plain_names =
            !versions && !long && !has_flag(rest, "-1", "") && !has_flag(rest, "--pinned", "");
        // The no-name default listing is brew's `ls` over the
        // Cellar/Caskroom: BSD ls columns and, on a terminal,
        // section headings the kind switches suppress.
        let ls_mode = plain_names && !full_name;
        let mut names: Vec<String> = Vec::new();
        for i in prefix.installed(kind) {
            if has_flag(rest, "--pinned", "")
                && !(if casks {
                    prefix.cask_is_pinned(&i.name)
                } else {
                    prefix.is_pinned(&i.name)
                })
            {
                continue;
            }
            #[cfg(unix)]
            if let Some(lister) = &mut lister {
                let dir = match kind {
                    Kind::Formula => prefix.keg(&i.name, &i.version),
                    Kind::Cask => prefix.cask_dir(&i.name, &i.version),
                };
                println!("{}", lister.line(&dir));
                continue;
            }
            let shown = if full_name {
                prefix.specified_name(&i)
            } else {
                i.name.clone()
            };
            if versions {
                // A pinned listing names the pinned version only.
                let shown_versions = if pinned {
                    match kind {
                        Kind::Formula => prefix.pinned_version(&i.name),
                        Kind::Cask => prefix.cask_pinned_version(&i.name),
                    }
                    .unwrap_or_else(|| i.versions.join(" "))
                } else {
                    i.versions.join(" ")
                };
                println!("{} {}", shown, shown_versions);
            } else if plain_names {
                names.push(shown);
            } else {
                println!("{shown}");
            }
        }
        if plain_names && !names.is_empty() {
            if ls_mode {
                use std::io::IsTerminal as _;
                if std::io::stdout().is_terminal() {
                    let suppressed = if casks {
                        has_flag(rest, "--cask", "")
                    } else {
                        has_flag(rest, "--formula", "")
                    };
                    if !suppressed {
                        println!(
                            "{}",
                            super::tty::ohai(if casks { "Casks" } else { "Formulae" })
                        );
                    }
                    print!("{}", ls_columns(&names, terminal_width()));
                    if !casks && !has_flag(rest, "--formula", "") {
                        println!();
                    }
                } else {
                    print_columns(&names);
                }
            } else {
                print_columns(&names);
            }
        }
    }
    ExitCode::SUCCESS
}

fn lookup_error(api: &Api, name: &str, kind: &str, e: String) -> String {
    if !e.starts_with("abrew: no such ") {
        return e;
    }
    if kind == "cask" {
        format!("Error: {}", cask_unavailable(Formula::short_name(name)))
    } else {
        format!("Error: {}", formula_unavailable(api, name))
    }
}

enum FetchJob {
    Cask(super::formula::Cask),
    Formula(Formula, Vec<Formula>),
}

/// The tap a fetch heading names: a tap formula's own tap, else
/// homebrew/core.
fn fetch_tap(f: &Formula) -> String {
    let parts: Vec<&str> = f.full_name.split('/').collect();
    if parts.len() == 3 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        "homebrew/core".to_string()
    }
}

pub(super) fn cmd_fetch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("fetch needs a name");
    }
    let mut jobs = Vec::new();
    for name in &names {
        if has_flag(rest, "--cask", "") {
            match api.cask(name) {
                Ok(c) => {
                    let c = match cask::resolve(&c, super::machine::Machine::this()) {
                        Ok(c) => c.into_owned(),
                        Err(e) => return from_install(prefix, args, Err(e)),
                    };
                    if !super::valid_component(&c.token) || !super::valid_cask_version(&c.version) {
                        return failed("abrew: unsupported cask token/version");
                    }
                    if c.disabled {
                        return failed(&format!("abrew: cask {} is disabled upstream", c.token));
                    }
                    jobs.push(FetchJob::Cask(c));
                }
                Err(e) => {
                    return failed(&lookup_error(api, name, "cask", e));
                }
            }
            continue;
        }
        match api.formula(name) {
            Ok(f) => {
                let deps = if has_flag(rest, "--deps", "") {
                    match install::fetch_plan(api, name) {
                        Ok(plan) => plan.into_iter().filter(|dep| dep.name != f.name).collect(),
                        Err(e) => return from_install(prefix, args, Err(e)),
                    }
                } else {
                    Vec::new()
                };
                jobs.push(FetchJob::Formula(f, deps));
            }
            Err(e) => {
                return failed(&lookup_error(api, name, "formula", e));
            }
        }
    }
    // Brew's output: a "Fetching:" bucket line when more than one
    // download runs, a per-package heading on stdout, and a ✔︎ line
    // on stderr per completed download (bottle manifest first).
    let with_deps = has_flag(rest, "--deps", "");
    let mut bucket: Vec<String> = Vec::new();
    for job in &jobs {
        match job {
            FetchJob::Cask(c) => bucket.push(c.token.clone()),
            FetchJob::Formula(f, deps) => {
                bucket.push(f.name.clone());
                bucket.extend(deps.iter().map(|d| d.name.clone()));
            }
        }
    }
    {
        let mut deduped: Vec<String> = Vec::new();
        for name in bucket {
            if !deduped.contains(&name) {
                deduped.push(name);
            }
        }
        bucket = deduped;
    }
    if bucket.len() > 1 {
        println!("Fetching: {}", bucket.join(", "));
    }
    let fetch_one = |f: &Formula| -> Result<(), ExitCode> {
        match install::fetch_bottle_manifest(prefix, f) {
            Ok(true) => {
                eprintln!("✔︎ Bottle Manifest {} ({})", f.name, install::keg_version(f));
            }
            Ok(false) => {}
            Err(e) => return Err(from_install(prefix, args, Err(e))),
        }
        match install::fetch_only(prefix, f) {
            Ok(_) => {
                eprintln!("✔︎ Bottle {} ({})", f.name, install::keg_version(f));
                Ok(())
            }
            Err(e) => Err(from_install(prefix, args, Err(e))),
        }
    };
    for job in &jobs {
        match job {
            FetchJob::Cask(c) => {
                println!(
                    "{}",
                    super::tty::ohai(&format!("Fetching {} from homebrew/cask", c.token))
                );
                if let Some(plan) = cask::git_raw_plan(c) {
                    match cask::fetch_git_raw(prefix, c, &plan) {
                        Ok(_) => eprintln!("✔︎ Cask {} ({})", c.token, c.version),
                        Err(e) => return from_install(prefix, args, Err(e)),
                    }
                    continue;
                }
                let dest = prefix.cache().join(cask::download_name(c));
                let expected = c.sha256.as_deref().unwrap_or("no_check");
                let reusable = c.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
                if !reusable && let Err(e) = fetch::curl_to_file(&c.url, &dest, &[]) {
                    return failed(&e);
                }
                if let Err(e) = fetch::verify_sha256(&dest, expected) {
                    return failed(&e);
                }
                eprintln!("✔︎ Cask {} ({})", c.token, c.version);
            }
            FetchJob::Formula(f, deps) => {
                if !with_deps {
                    println!(
                        "{}",
                        super::tty::ohai(&format!("Fetching {} from {}", f.name, fetch_tap(f)))
                    );
                }
                for dep in deps {
                    if let Err(code) = fetch_one(dep) {
                        return code;
                    }
                }
                if let Err(code) = fetch_one(f) {
                    return code;
                }
            }
        }
    }
    ExitCode::SUCCESS
}

fn flat_dependencies(
    f: &Formula,
    direct: bool,
    mut read: impl FnMut(&str) -> Option<Formula>,
) -> std::collections::BTreeSet<String> {
    let root = Formula::short_name(&f.name).to_string();
    let mut seen: std::collections::BTreeSet<String> = [root.clone()].into_iter().collect();
    let mut stack: Vec<String> = f.dependencies.clone();
    while let Some(d) = stack.pop() {
        let short = Formula::short_name(&d).to_string();
        if seen.insert(short.clone())
            && !direct
            && let Some(df) = read(&short)
        {
            stack.extend(df.dependencies.iter().cloned());
        }
    }
    seen.remove(&root);
    seen
}

pub(super) fn cmd_deps(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let tree = has_flag(rest, "--tree", "");
    let graph = has_flag(rest, "--graph", "");
    let dot = has_flag(rest, "--dot", "");
    if tree && graph {
        return super::usage_failed(
            "deps",
            "Error: Invalid usage: Options --tree and --graph are mutually exclusive.",
        );
    }
    if has_flag(rest, "--installed", "") && has_flag(rest, "--missing", "") {
        return super::usage_failed(
            "deps",
            "Error: Invalid usage: Options --installed and --missing are mutually exclusive.",
        );
    }
    if dot && !graph {
        return super::usage_failed(
            "deps",
            "Error: Invalid usage: `--dot` cannot be passed without `--graph`.",
        );
    }
    let prune = has_flag(rest, "--prune", "");
    let direct = has_flag(rest, "--direct", "-1")
        || has_flag(rest, "--1", "")
        || has_flag(rest, "--declared", "");
    let for_each = has_flag(rest, "--for-each", "");
    let union = has_flag(rest, "--union", "");
    let topological = has_flag(rest, "--topological", "-n");
    let full_name = has_flag(rest, "--full-name", "");
    let annotate = has_flag(rest, "--annotate", "");
    let missing_only = has_flag(rest, "--missing", "");
    let include_requirements = has_flag(rest, "--include-requirements", "");
    let filter = super::api::DepFilter {
        include_build: has_flag(rest, "--include-build", ""),
        include_test: has_flag(rest, "--include-test", ""),
        include_optional: has_flag(rest, "--include-optional", ""),
        skip_recommended: has_flag(rest, "--skip-recommended", ""),
    };
    let names = operands(rest);
    let installed_only = has_flag(rest, "--installed", "");
    let is_installed = |name: &str| {
        prefix
            .installed_one(Kind::Formula, Formula::short_name(name))
            .is_some()
    };
    // Brew warns whenever the answer is not the actual runtime
    // dependencies: a named formula is not installed, or a mode
    // flag widened/narrowed the set. Reasons in brew's order —
    // the last matching one wins.
    if !super::no_env_hints() {
        let mut reason: Option<&str> = None;
        if !names.is_empty() && !names.iter().all(|n| is_installed(n)) {
            reason = Some(if installed_only {
                "not all the named formulae were installed"
            } else {
                "`--installed` was not passed"
            });
        }
        if direct {
            reason = Some("--direct was passed");
        }
        if tree {
            reason = Some("--tree was passed");
        }
        if graph {
            reason = Some("--graph was passed");
        }
        if filter.skip_recommended {
            reason = Some("--skip-recommended was passed");
        }
        if missing_only {
            reason = Some("--missing was passed");
        }
        if has_flag(rest, "--include-implicit", "") {
            reason = Some("--include-implicit was passed");
        }
        if filter.include_build {
            reason = Some("--include-build was passed");
        }
        if filter.include_test {
            reason = Some("--include-test was passed");
        }
        if filter.include_optional {
            reason = Some("--include-optional was passed");
        }
        if include_requirements {
            reason = Some("--include-requirements was passed");
        }
        if let Some(reason) = reason {
            super::tty::opoo!(
                "Warning: `brew deps` is not the actual runtime dependencies because {reason}!"
            );
            eprintln!("This means dependencies may differ from a formula's declared dependencies.");
            eprintln!("Hide these hints with `HOMEBREW_NO_ENV_HINTS=1` (see `man brew`).");
        }
    }
    let want_formula = has_flag(rest, "--formula", "") || has_flag(rest, "--formulae", "");
    let want_cask = has_flag(rest, "--cask", "") || has_flag(rest, "--casks", "");
    // A cask's graph deps are its formula deps only (CaskDependent
    // in brew); `depends_on cask:` entries are requirements.
    let cask_row = |api: &Api, token: &str| -> Option<super::api::DepRow> {
        let c = api.cask(token).ok()?;
        let deps: Vec<String> = c
            .depends_formula
            .iter()
            .map(|d| Formula::short_name(d).to_string())
            .collect();
        let requirements = api
            .cask_value(token)
            .map(|v| super::formula::cask_requirement_displays(&v))
            .unwrap_or_default();
        Some(super::api::DepRow {
            full_name: c.token.clone(),
            name: c.token,
            deps,
            build_deps: Vec::new(),
            test_deps: Vec::new(),
            recommended_deps: Vec::new(),
            optional_deps: Vec::new(),
            requirements,
        })
    };
    let read_row = |name: &str| -> Option<super::api::DepRow> {
        api.formula(name)
            .ok()
            .map(|f| dep_row_of_formula(&f))
            .or_else(|| tap::lookup(prefix, name).map(|f| dep_row_of_formula_sourced(&f, true)))
            .or_else(|| cask_row(api, name))
    };
    let expander = DepExpander::new(&read_row, filter, missing_only, &is_installed);
    // Root rows: named packages, the installed set, or (bare)
    // everything in the indexes.
    let mut roots: Vec<super::api::DepRow> = Vec::new();
    if !names.is_empty() {
        for name in &names {
            let row = if want_cask && !want_formula {
                cask_row(api, name)
            } else if want_formula && !want_cask {
                api.formula(name)
                    .ok()
                    .map(|f| dep_row_of_formula(&f))
                    .or_else(|| {
                        tap::lookup(prefix, name).map(|f| dep_row_of_formula_sourced(&f, true))
                    })
            } else {
                read_row(name)
            };
            match row {
                Some(row) => roots.push(row),
                None => {
                    return failed(&format!(
                        "Error: {}",
                        if want_cask && !want_formula {
                            cask_unavailable(Formula::short_name(name))
                        } else {
                            formula_unavailable(api, name)
                        }
                    ));
                }
            }
        }
    } else if installed_only {
        if !want_cask {
            for i in prefix.installed(Kind::Formula) {
                if let Some(row) = read_row(&i.name) {
                    roots.push(row);
                }
            }
        }
        if !want_formula {
            for i in prefix.installed(Kind::Cask) {
                if let Some(row) = cask_row(api, &i.name) {
                    roots.push(row);
                }
            }
        }
        roots.sort_by(|a, b| a.name.cmp(&b.name));
    } else if tree || graph {
        return usage("deps --tree/--graph needs a formula name (or --installed)");
    } else {
        if !want_cask {
            match api.deps_index(false) {
                Ok(mut all) => roots.append(&mut all),
                Err(error) => return failed(&error),
            }
        }
        if !want_formula {
            match api.deps_index(true) {
                Ok(mut all) => roots.append(&mut all),
                Err(error) => return failed(&error),
            }
        }
        roots.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for root in &roots {
        expander.seed(root.clone());
    }
    let deps_of = |root: &super::api::DepRow| -> Vec<(String, super::api::DepTags)> {
        let mut deps: Vec<(String, super::api::DepTags)> = if direct {
            root.edges()
                .into_iter()
                .filter(|(name, tags)| {
                    tags.passes(&filter) && !(missing_only && is_installed(name))
                })
                .collect()
        } else {
            (*expander.expand(&root.name)).clone()
        };
        if include_requirements {
            // Requirements ride as `:display` pseudo-entries,
            // gathered recursively like Requirement.expand.
            let mut seen: std::collections::BTreeSet<String> = Default::default();
            let mut gather =
                |row: &super::api::DepRow, out: &mut Vec<(String, super::api::DepTags)>| {
                    for display in &row.requirements {
                        if missing_only && requirement_satisfied(prefix, display) {
                            continue;
                        }
                        let entry = format!(":{display}");
                        if seen.insert(entry.clone()) {
                            out.push((
                                entry,
                                super::api::DepTags {
                                    required: true,
                                    ..Default::default()
                                },
                            ));
                        }
                    }
                };
            let expanded_names: Vec<String> = deps.iter().map(|(name, _)| name.clone()).collect();
            gather(root, &mut deps);
            if !direct {
                for name in &expanded_names {
                    if let Some(row) = expander.row(name) {
                        gather(&row, &mut deps);
                    }
                }
            }
        }
        deps
    };
    let display_name = |expander: &DepExpander, name: &str, tags: &super::api::DepTags| -> String {
        let mut out = if full_name {
            expander
                .row(name)
                .map_or_else(|| name.to_string(), |r| r.full_name)
        } else {
            name.to_string()
        };
        if annotate {
            if tree || graph {
                out.push(' ');
            }
            if tags.build {
                out.push_str(" [build]");
            }
            if tags.test {
                out.push_str(" [test]");
            }
            if tags.optional {
                out.push_str(" [optional]");
            }
            if tags.recommended {
                out.push_str(" [recommended]");
            }
        }
        out
    };
    if graph {
        let mut sorted = roots.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let code = dot_code(&expander, &sorted, !direct, missing_only, &is_installed);
        if dot {
            println!("{code}");
        } else {
            // Brew opens the DOT text in GraphvizOnline.
            open_browser(&[format!(
                "https://dreampuf.github.io/GraphvizOnline/#{}",
                erb_url_encode(&code)
            )]);
        }
        return ExitCode::SUCCESS;
    }
    if tree {
        let mut sorted = roots.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let mut circular = false;
        for root in &sorted {
            println!("{}", root.full_name);
            let mut seen: std::collections::BTreeMap<String, bool> = Default::default();
            let mut lines: Vec<String> = Vec::new();
            deps_tree_lines(
                &expander,
                root,
                &filter,
                missing_only,
                &is_installed,
                prune,
                !direct,
                include_requirements,
                prefix,
                &display_name,
                &mut seen,
                "",
                &mut circular,
                &mut lines,
            );
            for line in lines {
                println!("{line}");
            }
            println!();
        }
        return if circular {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    if for_each || names.is_empty() {
        let mut sorted = roots.clone();
        if !names.is_empty() || for_each {
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
        }
        for root in &sorted {
            let mut deps = deps_of(root);
            if installed_only {
                deps.retain(|(name, _)| name.starts_with(':') || is_installed(name));
            }
            deps.sort_by(|a, b| a.0.cmp(&b.0));
            let shown: Vec<String> = deps
                .iter()
                .map(|(name, tags)| display_name(&expander, name, tags))
                .collect();
            println!("{}: {}", root.full_name, shown.join(" "));
        }
        return ExitCode::SUCCESS;
    }
    let mut lists = roots.iter().map(&deps_of);
    let mut all: Vec<(String, super::api::DepTags)> = lists.next().unwrap_or_default();
    for list in lists {
        if union {
            for item in list {
                match all.iter_mut().find(|(name, _)| *name == item.0) {
                    Some(existing) => {
                        existing.1 = super::api::DepTags::merge(&[existing.1, item.1]);
                    }
                    None => all.push(item),
                }
            }
        } else {
            all.retain(|(name, _)| list.iter().any(|(n, _)| n == name));
        }
    }
    if installed_only {
        all.retain(|(name, _)| name.starts_with(':') || is_installed(name));
    }
    let mut shown: Vec<String> = all
        .iter()
        .map(|(name, tags)| display_name(&expander, name, tags))
        .collect();
    shown.dedup();
    if !topological {
        shown.sort();
    }
    for line in shown {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

/// Brew's DOT graph (`deps --graph --dot`): insertion-ordered
/// nodes, direct passing edges each, tag-driven attributes.
fn dot_code(
    expander: &DepExpander,
    roots: &[super::api::DepRow],
    recursive: bool,
    missing_only: bool,
    is_installed: &dyn Fn(&str) -> bool,
) -> String {
    fn graph_deps(
        expander: &DepExpander,
        row: &super::api::DepRow,
        recursive: bool,
        missing_only: bool,
        is_installed: &dyn Fn(&str) -> bool,
        order: &mut Vec<String>,
        edges: &mut std::collections::BTreeMap<String, Vec<(String, super::api::DepTags)>>,
    ) {
        if edges.contains_key(&row.name) {
            return;
        }
        let dependables: Vec<(String, super::api::DepTags)> = row
            .edges()
            .into_iter()
            .filter(|(name, tags)| {
                tags.passes(&expander.filter) && !(missing_only && is_installed(name))
            })
            .collect();
        order.push(row.name.clone());
        edges.insert(row.name.clone(), dependables.clone());
        if recursive {
            for (dep, _) in dependables {
                if let Some(child) = expander.row(&dep) {
                    graph_deps(
                        expander,
                        &child,
                        true,
                        missing_only,
                        is_installed,
                        order,
                        edges,
                    );
                }
            }
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut edges: std::collections::BTreeMap<String, Vec<(String, super::api::DepTags)>> =
        Default::default();
    for root in roots {
        graph_deps(
            expander,
            root,
            recursive,
            missing_only,
            is_installed,
            &mut order,
            &mut edges,
        );
    }
    let mut lines: Vec<String> = Vec::new();
    for from in &order {
        for (dep, tags) in &edges[from] {
            let mut attrs: Vec<&str> = Vec::new();
            if tags.build {
                attrs.push("style = dotted");
            }
            if tags.test {
                attrs.push("arrowhead = empty");
            }
            if tags.optional {
                attrs.push("color = red");
            } else if tags.recommended {
                attrs.push("color = green");
            }
            let mut tag_names: Vec<&str> = Vec::new();
            if tags.build {
                tag_names.push(":build");
            }
            if tags.test {
                tag_names.push(":test");
            }
            if tags.optional {
                tag_names.push(":optional");
            }
            if tags.recommended {
                tag_names.push(":recommended");
            }
            let mut line = format!("  \"{from}\" -> \"{dep}\"");
            if !attrs.is_empty() {
                line.push_str(&format!(" [{}]", attrs.join(", ")));
            }
            if !tag_names.is_empty() {
                line.push_str(&format!(" # {}", tag_names.join(", ")));
            }
            lines.push(line);
        }
    }
    format!("digraph {{\n{}\n}}", lines.join("\n"))
}

/// Whether a requirement (by its display string) is already
/// satisfied on this machine — brew's `--missing` prune.
fn requirement_satisfied(prefix: &Prefix, display: &str) -> bool {
    let machine = super::machine::Machine::this();
    if display == "macOS" {
        return true;
    }
    if let Some(rest) = display.strip_prefix("macOS ") {
        let Some((cmp, versions)) = rest.split_once(' ') else {
            return false;
        };
        let Some(current) = machine.macos_version() else {
            return false;
        };
        return versions.split(" / ").any(|v| {
            let ord = super::formula::version_cmp(current, v);
            match cmp {
                ">=" => ord != std::cmp::Ordering::Less,
                "<=" => ord != std::cmp::Ordering::Greater,
                "==" => current == v || current.starts_with(&format!("{v}.")),
                _ => false,
            }
        });
    }
    if let Some(arch) = display.strip_suffix(" architecture") {
        return arch
            == if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            };
    }
    if display.starts_with("Xcode") {
        return true;
    }
    // A cask-token requirement (display is the capitalized token).
    let mut chars = display.chars();
    let token: String = match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => return false,
    };
    prefix.installed_one(Kind::Cask, &token).is_some()
}

/// Brew's `recursive_deps_tree`: `│` continuations, path-based
/// circular marks (a failure), `--prune` collapsing any node
/// already visited anywhere in this tree.
#[allow(clippy::too_many_arguments)]
fn deps_tree_lines(
    expander: &DepExpander,
    row: &super::api::DepRow,
    filter: &super::api::DepFilter,
    missing_only: bool,
    is_installed: &dyn Fn(&str) -> bool,
    prune: bool,
    recursive: bool,
    include_requirements: bool,
    pfx: &Prefix,
    display_name: &dyn Fn(&DepExpander, &str, &super::api::DepTags) -> String,
    seen: &mut std::collections::BTreeMap<String, bool>,
    prefix: &str,
    circular: &mut bool,
    out: &mut Vec<String>,
) {
    let mut dependables: Vec<(String, super::api::DepTags)> = Vec::new();
    if include_requirements {
        // Brew's dependables(): requirements first, then deps.
        for display in &row.requirements {
            if missing_only && requirement_satisfied(pfx, display) {
                continue;
            }
            dependables.push((
                format!(":{display}"),
                super::api::DepTags {
                    required: true,
                    ..Default::default()
                },
            ));
        }
    }
    dependables.extend(
        row.edges()
            .into_iter()
            .filter(|(name, tags)| tags.passes(filter) && !(missing_only && is_installed(name))),
    );
    seen.insert(row.name.clone(), true);
    let last_index = dependables.len().saturating_sub(1);
    for (i, (dep, tags)) in dependables.iter().enumerate() {
        let last = i == last_index;
        let branch = if last { "└──" } else { "├──" };
        let mut line = format!("{prefix}{branch} {}", display_name(expander, dep, tags));
        let is_circular = seen.get(dep).copied().unwrap_or(false);
        let pruned = prune && seen.contains_key(dep);
        if is_circular {
            line.push_str(" (CIRCULAR DEPENDENCY)");
            *circular = true;
        } else if pruned {
            line.push_str(" (PRUNED)");
        }
        out.push(line);
        if recursive && !is_circular && !pruned {
            let addition = if last { "    " } else { "│   " };
            if let Some(child) = expander.row(dep) {
                deps_tree_lines(
                    expander,
                    &child,
                    filter,
                    missing_only,
                    is_installed,
                    prune,
                    true,
                    include_requirements,
                    pfx,
                    display_name,
                    seen,
                    &format!("{prefix}{addition}"),
                    circular,
                    out,
                );
            }
        }
    }
    seen.insert(row.name.clone(), false);
}

/// name → (runtime deps, build deps) for `uses` closure walks.
type DepGraph = std::collections::BTreeMap<String, super::api::DepRow>;

/// Brew's `Dependency.expand` over the declared graph, with its
/// cross-candidate cache: an expansion computed under one root
/// (where `--include-test` counts only at that root) is reused
/// verbatim by later roots. Results are occurrence-merged like
/// `merge_repeats`: first position wins, tags merged.
type DepExpansion = std::rc::Rc<Vec<(String, super::api::DepTags)>>;

struct DepExpander<'a> {
    graph: std::cell::RefCell<DepGraph>,
    memo: std::cell::RefCell<std::collections::BTreeMap<String, DepExpansion>>,
    read: &'a dyn Fn(&str) -> Option<super::api::DepRow>,
    filter: super::api::DepFilter,
    prune_installed: bool,
    installed: &'a dyn Fn(&str) -> bool,
}

impl<'a> DepExpander<'a> {
    fn new(
        read: &'a dyn Fn(&str) -> Option<super::api::DepRow>,
        filter: super::api::DepFilter,
        prune_installed: bool,
        installed: &'a dyn Fn(&str) -> bool,
    ) -> DepExpander<'a> {
        DepExpander {
            graph: Default::default(),
            memo: Default::default(),
            read,
            filter,
            prune_installed,
            installed,
        }
    }

    fn seed(&self, row: super::api::DepRow) {
        self.graph.borrow_mut().insert(row.name.clone(), row);
    }

    fn row(&self, name: &str) -> Option<super::api::DepRow> {
        if let Some(hit) = self.graph.borrow().get(name) {
            return Some(hit.clone());
        }
        let entry = (self.read)(name)?;
        self.graph
            .borrow_mut()
            .insert(name.to_string(), entry.clone());
        Some(entry)
    }

    fn expand(&self, root: &str) -> std::rc::Rc<Vec<(String, super::api::DepTags)>> {
        self.expand_from(root, root, &mut Vec::new())
    }

    fn expand_from(
        &self,
        node: &str,
        root: &str,
        stack: &mut Vec<String>,
    ) -> std::rc::Rc<Vec<(String, super::api::DepTags)>> {
        if let Some(hit) = self.memo.borrow().get(node) {
            return hit.clone();
        }
        if stack.iter().any(|n| n == node) {
            return std::rc::Rc::new(Vec::new());
        }
        stack.push(node.to_string());
        let mut occurrences: Vec<(String, super::api::DepTags)> = Vec::new();
        if let Some(row) = self.row(node) {
            let node_filter = if node == root {
                self.filter
            } else {
                super::api::DepFilter {
                    include_test: false,
                    ..self.filter
                }
            };
            for (dep, tags) in row.edges() {
                if dep == node || !tags.passes(&node_filter) {
                    continue;
                }
                if self.prune_installed && (self.installed)(&dep) {
                    continue;
                }
                if stack.iter().any(|n| n == &dep) {
                    continue;
                }
                let sub = self.expand_from(&dep, root, stack);
                occurrences.extend(sub.iter().cloned());
                occurrences.push((dep, tags));
            }
        }
        stack.pop();
        let merged = merge_dep_occurrences(occurrences);
        let out = std::rc::Rc::new(merged);
        self.memo.borrow_mut().insert(node.to_string(), out.clone());
        out
    }
}

fn merge_dep_occurrences(
    occurrences: Vec<(String, super::api::DepTags)>,
) -> Vec<(String, super::api::DepTags)> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: std::collections::BTreeMap<String, Vec<super::api::DepTags>> =
        Default::default();
    for (name, tags) in occurrences {
        if !by_name.contains_key(&name) {
            order.push(name.clone());
        }
        by_name.entry(name).or_default().push(tags);
    }
    order
        .into_iter()
        .map(|name| {
            let tags = super::api::DepTags::merge(&by_name[&name]);
            (name, tags)
        })
        .collect()
}

fn dep_row_of_formula(f: &Formula) -> super::api::DepRow {
    dep_row_of_formula_sourced(f, false)
}

/// `from_tap`: brew's API struct generation drops a formula's
/// requirements, but a source-loaded (tap) formula keeps them —
/// only the tap path may surface them to `--include-requirements`.
fn dep_row_of_formula_sourced(f: &Formula, from_tap: bool) -> super::api::DepRow {
    let shorts = |deps: &[String]| -> Vec<String> {
        deps.iter()
            .map(|d| Formula::short_name(d).to_string())
            .collect()
    };
    super::api::DepRow {
        full_name: f.full_name.clone(),
        name: f.name.clone(),
        deps: shorts(&f.dependencies),
        build_deps: shorts(&f.build_dependencies),
        test_deps: shorts(&f.test_dependencies),
        recommended_deps: shorts(&f.recommended_dependencies),
        optional_deps: shorts(&f.optional_dependencies),
        requirements: if from_tap {
            f.requirements
                .iter()
                .map(super::formula::requirement_display)
                .collect()
        } else {
            Vec::new()
        },
    }
}

pub(super) fn cmd_uses(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("uses needs a formula name");
    }
    let installed_only = has_flag(rest, "--installed", "");
    let missing_only = has_flag(rest, "--missing", "");
    let recursive = has_flag(rest, "--recursive", "");
    let filter = super::api::DepFilter {
        include_build: has_flag(rest, "--include-build", ""),
        include_test: has_flag(rest, "--include-test", ""),
        include_optional: has_flag(rest, "--include-optional", ""),
        skip_recommended: has_flag(rest, "--skip-recommended", ""),
    };
    // Any tag switch turns brew off the tab-closure fast path for
    // --installed; our declared stand-in narrows the same way.
    let tag_flags = filter.include_build
        || filter.include_test
        || filter.include_optional
        || filter.skip_recommended
        || has_flag(rest, "--include-implicit", "");
    let want_formula = has_flag(rest, "--formula", "") || has_flag(rest, "--formulae", "");
    let want_cask = has_flag(rest, "--cask", "") || has_flag(rest, "--casks", "");
    let targets: Vec<String> = names
        .iter()
        .map(|n| Formula::short_name(n).to_string())
        .collect();
    let installed_f: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Formula)
        .into_iter()
        .map(|i| i.name)
        .collect();
    let installed_c: std::collections::BTreeSet<String> = prefix
        .installed(Kind::Cask)
        .into_iter()
        .map(|i| i.name)
        .collect();
    // Brew warns (and continues with a placeholder) for a name no
    // formula answers to; dependents of such a name are then an error.
    let mut unknown = false;
    for t in &targets {
        if !installed_f.contains(t) && api.formula(t).is_err() {
            super::tty::opoo!("Warning: {}", formula_unavailable(api, t));
            unknown = true;
        }
    }
    // Rows are the candidate dependents. With --installed brew's
    // formula side reads install-time runtime closures from tabs,
    // which abrew receipts don't record; the declared closure over
    // the current graph stands in (as it does for info --json).
    let mut rows: Vec<super::api::DepRow> = Vec::new();
    if !want_cask {
        if installed_only {
            for name in &installed_f {
                match api.formula(name) {
                    Ok(f) => rows.push(dep_row_of_formula(&f)),
                    Err(error) => return failed(&error),
                }
            }
        } else {
            match api.deps_index(false) {
                Ok(mut all) => rows.append(&mut all),
                Err(error) => return failed(&error),
            }
        }
    }
    if !want_formula {
        if installed_only {
            for token in &installed_c {
                match api.cask(token) {
                    Ok(c) => rows.push(super::api::DepRow {
                        full_name: c.token.clone(),
                        name: c.token,
                        deps: c
                            .depends_formula
                            .iter()
                            .map(|d| Formula::short_name(d).to_string())
                            .collect(),
                        build_deps: Vec::new(),
                        test_deps: Vec::new(),
                        recommended_deps: Vec::new(),
                        optional_deps: Vec::new(),
                        requirements: Vec::new(),
                    }),
                    Err(error) => return failed(&error),
                }
            }
        } else {
            match api.deps_index(true) {
                Ok(mut all) => rows.append(&mut all),
                Err(error) => return failed(&error),
            }
        }
    }
    if missing_only {
        rows.retain(|r| !installed_f.contains(&r.name) && !installed_c.contains(&r.name));
    }
    // The formula graph for recursive expansion, seeded from the
    // rows already in hand and extended on demand (a dependency
    // outside the row set still resolves through it).
    let read_row = |name: &str| api.formula(name).ok().map(|f| dep_row_of_formula(&f));
    let never_installed = |_: &str| false;
    let expander = DepExpander::new(&read_row, filter, false, &never_installed);
    for row in &rows {
        expander.seed(row.clone());
    }
    // The installed stand-in closure: plain reachability over
    // passing edges (test edges count only at the root, as in
    // Dependency.expand's prune block).
    let deep_filter = super::api::DepFilter {
        include_test: false,
        ..filter
    };
    let closure_contains = |row: &super::api::DepRow, target: &str| -> bool {
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut queue: Vec<String> = row
            .edges()
            .into_iter()
            .filter(|(_, tags)| tags.passes(&filter))
            .map(|(name, _)| name)
            .collect();
        while let Some(cur) = queue.pop() {
            if cur == target {
                return true;
            }
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(next) = expander.row(&cur) {
                queue.extend(
                    next.edges()
                        .into_iter()
                        .filter(|(_, tags)| tags.passes(&deep_filter))
                        .map(|(name, _)| name),
                );
            }
        }
        false
    };
    let mut hits: Vec<String> = rows
        .into_iter()
        .filter(|row| {
            if targets.contains(&row.name) {
                return false;
            }
            // Evaluate the expansion even for non-matching rows:
            // brew's cache fills in candidate order regardless.
            let expanded = if recursive {
                Some(expander.expand(&row.name))
            } else {
                None
            };
            targets.iter().all(|t| {
                if let Some(expanded) = &expanded {
                    expanded.iter().any(|(name, _)| name == t)
                } else if installed_only && !tag_flags && installed_f.contains(&row.name) {
                    closure_contains(row, t)
                } else {
                    row.edge_passes(t, &filter)
                }
            })
        })
        .map(|row| row.name)
        .collect();
    hits.sort();
    hits.dedup();
    print_columns(&hits);
    if unknown && !hits.is_empty() {
        super::tty::onoe!("Error: Missing formulae should not have dependents!");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_leaves(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let on_request_only = has_flag(rest, "--installed-on-request", "-r");
    let as_dependency_only = has_flag(rest, "--installed-as-dependency", "-p");
    let installed = prefix.installed(Kind::Formula);
    // Brew unions every installed keg's tab runtime_dependencies
    // (the install-time closure) with installed casks' direct
    // formula deps; abrew receipts record no tab closure, so the
    // declared closure over the current graph stands in.
    let mut needed: std::collections::BTreeSet<String> = Default::default();
    let mut formulae: Vec<(super::prefix::Installed, Formula)> = Vec::new();
    for i in &installed {
        let f = match api.formula(&i.name) {
            Ok(f) => f,
            Err(error) => return failed(&error),
        };
        let mut queue: Vec<String> = f
            .dependencies
            .iter()
            .map(|d| Formula::short_name(d).to_string())
            .collect();
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        while let Some(cur) = queue.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            needed.insert(cur.clone());
            if let Ok(dep) = api.formula(&cur) {
                queue.extend(
                    dep.dependencies
                        .iter()
                        .map(|d| Formula::short_name(d).to_string()),
                );
            }
        }
        formulae.push((i.clone(), f));
    }
    for token in prefix.installed(Kind::Cask) {
        match api.cask(&token.name) {
            Ok(c) => {
                for d in &c.depends_formula {
                    needed.insert(Formula::short_name(d).to_string());
                }
            }
            Err(error) => return failed(&error),
        }
    }
    let mut leaves: Vec<String> = Vec::new();
    for (i, f) in &formulae {
        // Brew rejects a leaf when any of its possible names (name,
        // aliases, old names) is depended upon.
        let depended = std::iter::once(f.name.as_str())
            .chain(f.aliases.iter().map(String::as_str))
            .chain(f.oldnames.iter().map(String::as_str))
            .any(|n| needed.contains(Formula::short_name(n)));
        if depended {
            continue;
        }
        let on_request = super::prefix::receipt_on_request(&prefix.keg(&i.name, &i.version));
        if on_request_only && !on_request {
            continue;
        }
        if as_dependency_only && on_request {
            continue;
        }
        leaves.push(prefix.specified_name(i));
    }
    leaves.sort();
    for leaf in leaves {
        println!("{leaf}");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_zap(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("zap needs at least one cask");
    }
    let force = has_flag(rest, "--force", "-f");
    let mut failures = 0;
    let mut completed = false;
    for name in &names {
        let token = super::api::canonical_cask_token(Formula::short_name(name));
        if !force
            && prefix.cask_is_pinned(token)
            && prefix.installed_one(Kind::Cask, token).is_some()
        {
            continue;
        }
        let r = match api.cask(token) {
            Ok(c) => cask::zap(prefix, token, &c, false, force),
            Err(_) => Err(install::InstallError::Failed(format!(
                "Error: {}",
                cask_unavailable(token)
            ))),
        };
        match r {
            Ok(()) => completed = true,
            Err(e @ install::InstallError::Unsupported(_)) => {
                return from_install(
                    prefix,
                    args,
                    Err(super::after_native_progress(e, completed)),
                );
            }
            Err(install::InstallError::Aborted) => return ExitCode::from(1),
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_missing(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let scope: Vec<String> = operands(rest)
        .iter()
        .map(|n| Formula::short_name(n).to_string())
        .collect();
    let hidden: Vec<String> = rest
        .iter()
        .filter_map(|arg| arg.strip_prefix("--hide="))
        .flat_map(|v| v.split(','))
        .map(|part| Formula::short_name(part.trim()).to_string())
        .collect();
    let is_hidden = |name: &str| hidden.iter().any(|h| h == name);
    // Brew resolves named formulae and casks whether or not they
    // are installed; bare means every installed package. A line
    // per package lists its missing recursive dependencies,
    // space-joined, prefixed with the package name only when
    // more than one package is in scope.
    let mut formulae: Vec<Formula> = Vec::new();
    let mut casks: Vec<super::formula::Cask> = Vec::new();
    if scope.is_empty() {
        for i in prefix.installed(Kind::Formula) {
            match api.formula(&i.name).or_else(|_| {
                tap::lookup(prefix, &i.name).ok_or_else(|| format!("abrew: no formula {}", i.name))
            }) {
                Ok(f) => formulae.push(f),
                Err(error) => return failed(&error),
            }
        }
        for i in prefix.installed(Kind::Cask) {
            match api.cask(&i.name) {
                Ok(c) => casks.push(c),
                Err(error) => return failed(&error),
            }
        }
    } else {
        for name in &scope {
            if !has_flag(rest, "--cask", "")
                && let Ok(f) = api.formula(name).or_else(|_| {
                    tap::lookup(prefix, name).ok_or_else(|| format!("abrew: no formula {name}"))
                })
            {
                formulae.push(f);
                continue;
            }
            match api.cask(name) {
                Ok(c) if !has_flag(rest, "--formula", "") => casks.push(c),
                _ => {
                    return failed(&format!(
                        "Error: {}",
                        if has_flag(rest, "--cask", "") {
                            cask_unavailable(name)
                        } else {
                            formula_unavailable(api, name)
                        }
                    ));
                }
            }
        }
    }
    let show_name = formulae.len() + casks.len() > 1;
    let mut any = false;
    let mut report = |name: &str, missing: Vec<String>| {
        if missing.is_empty() {
            return;
        }
        any = true;
        if show_name {
            println!("{name}: {}", missing.join(" "));
        } else {
            println!("{}", missing.join(" "));
        }
    };
    for f in &formulae {
        let missing: Vec<String> = flat_dependencies(f, false, |n| api.formula(n).ok())
            .into_iter()
            .map(|d| Formula::short_name(&d).to_string())
            .filter(|d| !is_hidden(d) && prefix.installed_one(Kind::Formula, d).is_none())
            .collect();
        report(&f.name, missing);
    }
    for c in &casks {
        let mut missing: Vec<String> = Vec::new();
        for d in &c.depends_formula {
            let short = Formula::short_name(d);
            if !is_hidden(short) && prefix.installed_one(Kind::Formula, short).is_none() {
                missing.push(short.to_string());
            }
        }
        for d in &c.depends_cask {
            if !is_hidden(d) && prefix.installed_one(Kind::Cask, d).is_none() {
                missing.push(d.clone());
            }
        }
        report(&c.token, missing);
    }
    if any {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `brew options`: print declared formula options or delegate tap-only formulae.
pub(super) fn cmd_options(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let compact = has_flag(rest, "--compact", "");
    let names = operands(rest);
    let tapped = || {
        from_install(
            prefix,
            args,
            Err(install::InstallError::Unsupported(
                "options for tapped formulae live in tap recipes".to_string(),
            )),
        )
    };
    let rows: Vec<super::api::OptionsRow> = if names.is_empty() {
        if !tap::list(prefix).is_empty() {
            return tapped();
        }
        match api.options_index() {
            Ok(rows) => rows,
            Err(e) => return failed(&e),
        }
    } else if has_flag(rest, "--installed", "") {
        let mut rows = Vec::new();
        for i in prefix.installed(Kind::Formula) {
            match api.formula_value(&i.name) {
                Ok(v) => {
                    let (options, head) = super::api::parse_options_record(&v);
                    let full_name = v
                        .get("full_name")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or(&i.name)
                        .to_string();
                    rows.push(super::api::OptionsRow {
                        full_name,
                        options,
                        head,
                    });
                }
                Err(_) => return tapped(),
            }
        }
        rows
    } else {
        let mut rows = Vec::new();
        for name in &names {
            match api.formula_value(name) {
                Ok(v) => {
                    let (options, head) = super::api::parse_options_record(&v);
                    let full_name = v
                        .get("full_name")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or(name)
                        .to_string();
                    rows.push(super::api::OptionsRow {
                        full_name,
                        options,
                        head,
                    });
                }
                Err(_) if tap::lookup(prefix, name).is_some() => return tapped(),
                Err(_) => return failed(&format!("Error: {}", formula_unavailable(api, name))),
            }
        }
        rows
    };
    print_options(&rows, compact);
    ExitCode::SUCCESS
}

fn print_options(rows: &[super::api::OptionsRow], compact: bool) {
    let multiple = rows.len() > 1;
    for row in rows {
        if row.options.is_empty() {
            continue;
        }
        if compact {
            let flags: Vec<String> = row.options.iter().map(|(f, _)| format!("--{f}")).collect();
            println!("{}", flags.join(" "));
        } else {
            if multiple {
                println!("{}", row.full_name);
            }
            for (flag, desc) in &row.options {
                println!("--{flag}");
                println!("\t{desc}");
            }
            if row.head {
                println!("--HEAD");
                println!("\tInstall HEAD version");
            }
            println!();
        }
    }
}

fn is_macho(head: &[u8]) -> bool {
    matches!(
        head.get(..4),
        Some(
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    ) && !crate::macho::is_java_class(head)
}

#[cfg(test)]
fn linked_dylibs(path: &std::path::Path) -> Result<Vec<String>, String> {
    let names = crate::macho::load_names_at(path)
        .map_err(|error| format!("inspection failed: {error}"))?
        .once();
    let mut rows = names.id;
    for dylib in names.loads {
        if !rows.contains(&dylib) {
            rows.push(dylib);
        }
    }
    Ok(rows)
}

#[cfg(test)]
fn linked_library_exists(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// One keg's linkage classification, brew's LinkageChecker
/// buckets (the macOS set; unwanted-system is Linux-only there
/// and `:no_linkage` tags are not in the API record, so those
/// sections never print).
#[derive(Default)]
struct LinkageReport {
    system: Vec<String>,
    brewed: std::collections::BTreeMap<String, Vec<String>>,
    variable: Vec<String>,
    broken_dylibs: Vec<String>,
    broken_deps: std::collections::BTreeMap<String, Vec<String>>,
    executable_path: Vec<String>,
    files_missing_rpaths: Vec<String>,
    reverse: std::collections::BTreeMap<String, Vec<String>>,
    indirect: Vec<String>,
    undeclared: Vec<String>,
    unnecessary: Vec<String>,
    conflicts: Vec<String>,
}

fn macho_is_executable(bytes: &[u8]) -> bool {
    fn filetype(bytes: &[u8]) -> Option<u32> {
        let be = match bytes.get(..4)? {
            [0xFE, 0xED, 0xFA, 0xCF] | [0xFE, 0xED, 0xFA, 0xCE] => true,
            [0xCF, 0xFA, 0xED, 0xFE] | [0xCE, 0xFA, 0xED, 0xFE] => false,
            [0xCA, 0xFE, 0xBA, 0xBE] => {
                let off = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?) as usize;
                return filetype(bytes.get(off..)?);
            }
            _ => return None,
        };
        let raw: [u8; 4] = bytes.get(12..16)?.try_into().ok()?;
        Some(if be {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        })
    }
    filetype(bytes) == Some(2) // MH_EXECUTE
}

/// The dependency a prefix dylib path belongs to
/// (`<prefix>/opt/<name>/…` or `<prefix>/Cellar/<name>/…`).
fn dylib_dep(prefix: &Prefix, dylib: &str) -> Option<String> {
    let rest = dylib.strip_prefix(&format!("{}/", prefix.root.display()))?;
    let mut parts = rest.split('/');
    match (parts.next(), parts.next()) {
        (Some("opt" | "Cellar"), Some(name)) if !name.is_empty() => Some(name.to_string()),
        _ => None,
    }
}

/// Lexical path normalization (brew's Pathname#cleanpath).
fn cleanpath(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Brew's variable resolution for one load name: @loader_path
/// always expands to the file's directory, @executable_path
/// only for executables, and @rpath tries each (already
/// loader/executable-resolved) run path in order, first
/// existing target wins. Unresolved names stay raw.
fn resolve_load_name(
    name: &str,
    file_dir: &std::path::Path,
    executable: bool,
    rpaths: &[String],
) -> String {
    if let Some(rest) = name.strip_prefix("@loader_path") {
        return cleanpath(&file_dir.join(format!(".{rest}")))
            .to_string_lossy()
            .into_owned();
    }
    if executable && let Some(rest) = name.strip_prefix("@executable_path") {
        return cleanpath(&file_dir.join(format!(".{rest}")))
            .to_string_lossy()
            .into_owned();
    }
    if let Some(rest) = name.strip_prefix("@rpath") {
        for rpath in rpaths {
            let target = format!("{}{rest}", rpath.trim_end_matches('/'));
            if std::path::Path::new(&target).exists() {
                return target;
            }
        }
    }
    name.to_string()
}

fn scan_linkage(prefix: &Prefix, keg: &std::path::Path) -> LinkageReport {
    let mut report = LinkageReport::default();
    let mut seen_dylibs: Vec<String> = Vec::new();
    let push_unique = |list: &mut Vec<String>, item: &str| {
        if !list.iter().any(|x| x == item) {
            list.push(item.to_string());
        }
    };
    fn walk(
        dir: &std::path::Path,
        prefix: &Prefix,
        keg: &std::path::Path,
        report: &mut LinkageReport,
        seen_dylibs: &mut Vec<String>,
    ) {
        let push_unique = |list: &mut Vec<String>, item: &str| {
            if !list.iter().any(|x| x == item) {
                list.push(item.to_string());
            }
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(&p, prefix, keg, report, seen_dylibs);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            if !is_macho(&bytes) {
                continue;
            }
            let Ok(names) = crate::macho::load_names(&bytes) else {
                continue;
            };
            let names = names.once();
            let executable = macho_is_executable(&bytes);
            let file_dir = p.parent().unwrap_or(dir);
            let rpaths: Vec<String> = names
                .rpaths
                .iter()
                .map(|r| resolve_load_name(r, file_dir, executable, &[]))
                .collect();
            // Weakly loaded dylibs may not exist on disk; brew's
            // checker excludes them from every linkage category.
            let loads: Vec<String> = names
                .loads
                .iter()
                .filter(|n| !names.weak_loads.contains(n))
                .map(|n| resolve_load_name(n, file_dir, executable, &rpaths))
                .collect();
            let mut has_rpath_lib = false;
            for dylib in &loads {
                report.reverse.entry(dylib.clone()).or_default().push(
                    p.strip_prefix(keg)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .into_owned(),
                );
                if !has_rpath_lib && dylib.starts_with("@rpath/") {
                    has_rpath_lib = true;
                    if names.rpaths.is_empty() {
                        push_unique(&mut report.files_missing_rpaths, &p.to_string_lossy());
                    }
                }
                if seen_dylibs.contains(dylib) {
                    continue;
                }
                seen_dylibs.push(dylib.clone());
                if dylib.starts_with("@rpath") {
                    push_unique(&mut report.variable, dylib);
                    continue;
                }
                if dylib.starts_with("@executable_path") && !executable {
                    push_unique(&mut report.executable_path, dylib);
                    continue;
                }
                if !dylib.starts_with('/') {
                    // Brew's Keg.for raises NotAKeg here (@loader_path
                    // and friends land in System libraries).
                    push_unique(&mut report.system, dylib);
                    continue;
                }
                let exists = std::path::Path::new(dylib.as_str()).exists();
                match dylib_dep(prefix, dylib) {
                    Some(dep) if exists => {
                        let shown = prefix
                            .installed_one(Kind::Formula, &dep)
                            .map(|i| prefix.specified_name(&i))
                            .unwrap_or_else(|| dep.clone());
                        push_unique(report.brewed.entry(shown).or_default(), dylib);
                    }
                    Some(dep) => {
                        push_unique(report.broken_deps.entry(dep).or_default(), dylib);
                    }
                    None if dylib.starts_with("/usr/lib/") || dylib.starts_with("/System/") => {
                        push_unique(&mut report.system, dylib);
                    }
                    None if exists => push_unique(&mut report.system, dylib),
                    None => push_unique(&mut report.broken_dylibs, dylib),
                }
            }
        }
    }
    let _ = push_unique;
    walk(keg, prefix, keg, &mut report, &mut seen_dylibs);
    report.system.sort();
    report.variable.sort();
    report.broken_dylibs.sort();
    report.executable_path.sort();
    report.files_missing_rpaths.sort();
    for v in report.brewed.values_mut() {
        v.sort();
    }
    for v in report.broken_deps.values_mut() {
        v.sort();
    }
    for v in report.reverse.values_mut() {
        v.sort();
    }
    report
}

/// Declared/indirect/undeclared analysis against the formula
/// record, brew's check_formula_deps (minus `:no_linkage`, which
/// the API record does not carry).
fn analyse_linkage_deps(prefix: &Prefix, api: &Api, f: &Formula, report: &mut LinkageReport) {
    let short = |n: &str| Formula::short_name(n).to_string();
    let declared: Vec<String> = f.dependencies.iter().map(|d| short(d)).collect();
    let recursive: Vec<String> = flat_dependencies(f, false, |n| api.formula(n).ok())
        .iter()
        .map(|d| short(d))
        .collect();
    let brewed_names: Vec<String> = report
        .brewed
        .keys()
        .map(|k| short(k))
        .filter(|n| *n != f.name)
        .collect();
    for full in report.brewed.keys() {
        let name = short(full);
        if name == f.name {
            continue;
        }
        if recursive.contains(&name) {
            if !declared.contains(&name) {
                report.indirect.push(full.clone());
            }
        } else {
            report.undeclared.push(full.clone());
        }
    }
    let broken: Vec<String> = report.broken_deps.keys().cloned().collect();
    for d in &declared {
        let has_bin = prefix
            .installed_one(Kind::Formula, d)
            .is_some_and(|i| prefix.keg(d, &i.version).join("bin").is_dir());
        if !has_bin && !brewed_names.contains(d) && !broken.contains(d) {
            report.unnecessary.push(
                prefix
                    .installed_one(Kind::Formula, d)
                    .map(|i| prefix.specified_name(&i))
                    .unwrap_or_else(|| d.clone()),
            );
        }
    }
    let mut by_stem: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for full in report.brewed.keys() {
        let name = short(full);
        let stem = name.split('@').next().unwrap_or(&name).to_string();
        let entry = by_stem.entry(stem).or_default();
        if !entry.contains(&name) {
            entry.push(name);
        }
    }
    for variants in by_stem.values() {
        if variants.len() >= 2 {
            report.conflicts.extend(variants.iter().cloned());
        }
    }
    report.indirect.sort();
    report.undeclared.sort();
    report.unnecessary.sort();
    report.conflicts.sort();
}

fn print_linkage_items(label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("{label}:");
    for item in items {
        println!("  {item}");
    }
}

fn print_linkage_brewed(label: &str, map: &std::collections::BTreeMap<String, Vec<String>>) {
    if map.is_empty() {
        return;
    }
    println!("{label}:");
    for (dep, items) in map {
        for item in items {
            println!("  {item} ({dep})");
        }
    }
}

pub(super) fn cmd_linkage(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    if !cfg!(target_os = "macos") {
        println!("linkage inspection reads Mach-O load commands: macOS only");
        return ExitCode::SUCCESS;
    }
    let test = has_flag(rest, "--test", "");
    let strict = has_flag(rest, "--strict", "");
    let reverse = has_flag(rest, "--reverse", "");
    let names = operands(rest);
    let mut kegs: Vec<(String, super::prefix::Installed)> = Vec::new();
    if names.is_empty() {
        for inst in prefix.installed(Kind::Formula) {
            kegs.push((inst.name.clone(), inst));
        }
    } else {
        for name in &names {
            let short = Formula::short_name(name);
            let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
                return failed(&format!("Error: {}", no_such_keg(prefix, short)));
            };
            kegs.push((short.to_string(), inst));
        }
    }
    let mut failed_any = false;
    for (short, inst) in &kegs {
        if kegs.len() > 1 {
            println!("{}", super::tty::ohai(&format!("Checking {short} linkage")));
        }
        let keg = prefix.keg(short, &inst.version);
        let mut report = scan_linkage(prefix, &keg);
        if let Ok(f) = api
            .formula(short)
            .or_else(|_| tap::lookup(prefix, short).ok_or_else(|| "no record".to_string()))
        {
            analyse_linkage_deps(prefix, api, &f, &mut report);
        }
        if reverse {
            let total = report.reverse.len();
            for (i, (dylib, files)) in report.reverse.iter().enumerate() {
                println!("{dylib}");
                for f in files {
                    println!("  {f}");
                }
                if i + 1 < total {
                    println!();
                }
            }
            continue;
        }
        if test {
            print_linkage_items("Missing libraries", &report.broken_dylibs);
            // Broken dependencies print as the deps' dylib paths.
            let broken_flat: Vec<String> = report.broken_deps.values().flatten().cloned().collect();
            print_linkage_items("Broken dependencies", &broken_flat);
            print_linkage_items("Conflicting libraries", &report.conflicts);
            print_linkage_items("Indirect dependencies with linkage", &report.indirect);
            if strict {
                print_linkage_items("Undeclared dependencies with linkage", &report.undeclared);
                print_linkage_items("Files with missing rpath", &report.files_missing_rpaths);
                print_linkage_items(
                    "@executable_path references in libraries",
                    &report.executable_path,
                );
            }
            let mut issues = !report.broken_dylibs.is_empty()
                || !report.broken_deps.is_empty()
                || !report.conflicts.is_empty()
                || !report.indirect.is_empty();
            if strict {
                issues |= !report.undeclared.is_empty()
                    || !report.files_missing_rpaths.is_empty()
                    || !report.executable_path.is_empty();
            }
            failed_any |= issues;
        } else {
            print_linkage_items("System libraries", &report.system);
            print_linkage_brewed("Homebrew libraries", &report.brewed);
            print_linkage_items("Indirect dependencies with linkage", &report.indirect);
            print_linkage_items("@rpath-referenced libraries", &report.variable);
            print_linkage_items("Missing libraries", &report.broken_dylibs);
            let broken_flat: Vec<String> = report.broken_deps.values().flatten().cloned().collect();
            print_linkage_items("Broken dependencies", &broken_flat);
            print_linkage_items("Undeclared dependencies with linkage", &report.undeclared);
            print_linkage_items("Dependencies with no linkage", &report.unnecessary);
            print_linkage_items("Files with missing rpath", &report.files_missing_rpaths);
            print_linkage_items(
                "@executable_path references in libraries",
                &report.executable_path,
            );
        }
    }
    if failed_any {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_log(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    let Some(name) = names.first() else {
        for t in tap::list(prefix) {
            println!("{}", super::tty::ohai(&format!("tap {}", t.name())));
            let _ = fetch::run_inherit(
                aterm_uds::os_path!("git"),
                &["-C", &t.path.to_string_lossy(), "log", "--oneline", "-5"],
            );
        }
        return ExitCode::SUCCESS;
    };
    let short = Formula::short_name(name);
    let token = super::api::canonical_cask_token(short);
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    if !want_cask && prefix.installed_one(Kind::Formula, short).is_some() {
        println!("{short}: installed by abrew (see INSTALL_RECEIPT.json in the keg)");
    } else if !want_formula && prefix.installed_one(Kind::Cask, token).is_some() {
        println!("{token}: installed by abrew (see INSTALL_RECEIPT.json in the caskroom)");
    } else {
        let shown = if want_cask { token } else { short };
        println!("{shown}: no abrew install record (abrew keeps no build logs)");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_tap(prefix: &Prefix, rest: &[String]) -> ExitCode {
    if has_flag(rest, "--list-official", "") {
        for t in [
            "homebrew/bundle",
            "homebrew/cask",
            "homebrew/core",
            "homebrew/services",
        ] {
            println!("{t}");
        }
        return ExitCode::SUCCESS;
    }
    let ops = operands(rest);
    if ops.is_empty() {
        for t in tap::list(prefix) {
            println!("{}", t.name());
        }
        return ExitCode::SUCCESS;
    }
    let url = ops.get(1).map(String::as_str);
    match tap::add(prefix, ops.first().map_or("", String::as_str), url) {
        Ok((t, newly)) => {
            if newly {
                println!("Tapped {} ({})", t.name(), t.path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

/// Brew's `Utils.pluralize(word, n, include_count:)` for the words
/// tap-info uses (`formula` is the irregular one).
fn pluralize_with_count(word: &str, count: usize) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        let plural = match word {
            "formula" => "formulae".to_string(),
            w => format!("{w}s"),
        };
        format!("{count} {plural}")
    }
}

/// Sorted `.rb` file stems under `dir`, walking shard subdirectories
/// (formula dirs shard by first letter).
fn rb_stems(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    collect_rb_stems(dir, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_rb_stems(dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "rb") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        } else if depth < 2 && entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_rb_stems(&path, depth + 1, out);
        }
    }
}

fn tap_formula_names(tap: &tap::Tap) -> Vec<String> {
    let names = rb_stems(&tap.path.join("Formula"));
    if names.is_empty() {
        rb_stems(&tap.path)
    } else {
        names
    }
}

fn tap_cask_tokens(tap: &tap::Tap) -> Vec<String> {
    rb_stems(&tap.path.join("Casks"))
}

fn tap_command_names(tap: &tap::Tap) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(tap.path.join("cmd")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.strip_prefix("brew-").unwrap_or(stem).to_string());
            }
        }
    }
    names.sort();
    names
}

fn tap_contents(tap: &tap::Tap) -> Vec<String> {
    let mut contents = Vec::new();
    let commands = tap_command_names(tap).len();
    if commands > 0 {
        contents.push(pluralize_with_count("command", commands));
    }
    let casks = tap_cask_tokens(tap).len();
    if casks > 0 {
        contents.push(pluralize_with_count("cask", casks));
    }
    let formulae = tap_formula_names(tap).len();
    if formulae > 0 {
        contents.push(pluralize_with_count("formula", formulae));
    }
    contents
}

fn tap_git(tap: &tap::Tap, args: &[&str]) -> Option<String> {
    let mut full = vec!["-C", tap.path.to_str()?];
    full.extend_from_slice(args);
    let out = super::fetch::run_capture("git", &full).ok()?;
    if !out.status.success() {
        return None;
    }
    let trimmed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub(super) fn cmd_tap_info(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let taps = tap::list(prefix);
    if has_flag(rest, "--json", "") || rest.iter().any(|s| s.starts_with("--json=")) {
        let names: Vec<String> = if has_flag(rest, "--installed", "") || operands(rest).is_empty() {
            taps.iter().map(|t| t.name()).collect()
        } else {
            operands(rest)
                .iter()
                .map(|op| {
                    tap::parse_tap_name(op)
                        .map(|(user, repo)| format!("{user}/{repo}"))
                        .unwrap_or_else(|| op.to_lowercase())
                })
                .collect()
        };
        let values: Vec<aterm_json::Value> = names
            .iter()
            .map(|name| tap_json_value(prefix, api, name, taps.iter().find(|t| t.name() == *name)))
            .collect();
        println!("{}", aterm_json::Value::Array(values));
        return ExitCode::SUCCESS;
    }
    if has_flag(rest, "--installed", "") {
        for (i, tap) in taps.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_tap_detail(prefix, tap);
        }
        return ExitCode::SUCCESS;
    }
    let ops = operands(rest);
    if ops.is_empty() {
        let mut formulae = 0usize;
        let mut commands = 0usize;
        for tap in &taps {
            formulae += tap_formula_names(tap).len();
            commands += tap_command_names(tap).len();
        }
        // `private?` is a GitHub API fact in brew; offline we cannot
        // know it, and brew's own unauthenticated check also reports
        // public. Privacy only feeds this count.
        let mut info = pluralize_with_count("tap", taps.len());
        info.push_str(", 0 private");
        info.push_str(&format!(", {}", pluralize_with_count("formula", formulae)));
        info.push_str(&format!(", {}", pluralize_with_count("command", commands)));
        let taps_dir = prefix.taps_dir();
        if taps_dir.is_dir() {
            info.push_str(&format!(", {}", keg_abv(&taps_dir)));
        }
        println!("{info}");
        return ExitCode::SUCCESS;
    }
    let mut failed = false;
    let mut first = true;
    for op in &ops {
        let name = tap::parse_tap_name(op)
            .map(|(user, repo)| format!("{user}/{repo}"))
            .unwrap_or_else(|| op.to_lowercase());
        if !first {
            println!();
        }
        first = false;
        match taps.iter().find(|t| t.name() == name) {
            None => {
                println!("{name}: Not installed");
                failed = true;
            }
            Some(tap) => print_tap_detail(prefix, tap),
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Every `.rb` file under `dir`, sorted (brew's `formula_files`).
fn rb_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rb") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Brew `Tap#to_hash` for `tap-info --json`. `private` is a
/// GitHub API fact brew reports as public when unauthenticated;
/// `trusted` follows brew's default (official taps trusted).
fn tap_json_value(
    prefix: &Prefix,
    api: &Api,
    name: &str,
    installed: Option<&tap::Tap>,
) -> aterm_json::Value {
    let (user_raw, repo) =
        tap::parse_tap_name(name).unwrap_or_else(|| (name.to_string(), String::new()));
    let user = if user_raw.eq_ignore_ascii_case("homebrew") {
        "Homebrew".to_string()
    } else if user_raw.eq_ignore_ascii_case("linuxbrew") {
        "Linuxbrew".to_string()
    } else {
        user_raw.clone()
    };
    let official = user == "Homebrew";
    let path = installed.map_or_else(
        || {
            prefix
                .taps_dir()
                .join(user_raw.to_lowercase())
                .join(format!("homebrew-{repo}"))
        },
        |tap| tap.path.clone(),
    );
    let (formula_names, cask_tokens) = match installed {
        Some(tap) => (tap_formula_names(tap), tap_cask_tokens(tap)),
        None if official && repo == "core" => (
            api.index(false)
                .map(|rows| rows.into_iter().map(|(n, _)| n).collect())
                .unwrap_or_default(),
            Vec::new(),
        ),
        None if official && repo == "cask" => (
            Vec::new(),
            api.index(true)
                .map(|rows| rows.into_iter().map(|(n, _)| n).collect())
                .unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    let strings = |items: Vec<String>| {
        aterm_json::Value::Array(items.into_iter().map(aterm_json::Value::String).collect())
    };
    let mut map = aterm_json::Map::new();
    map.insert(
        "name".into(),
        aterm_json::Value::String(format!("{user}/{repo}").to_lowercase()),
    );
    map.insert("user".into(), aterm_json::Value::String(user.clone()));
    map.insert("repo".into(), aterm_json::Value::String(repo.clone()));
    map.insert("repository".into(), aterm_json::Value::String(repo.clone()));
    map.insert(
        "path".into(),
        aterm_json::Value::String(path.display().to_string()),
    );
    map.insert(
        "installed".into(),
        aterm_json::Value::Bool(installed.is_some()),
    );
    map.insert("official".into(), aterm_json::Value::Bool(official));
    map.insert("trusted".into(), aterm_json::Value::Bool(official));
    map.insert("formula_names".into(), strings(formula_names));
    map.insert("cask_tokens".into(), strings(cask_tokens));
    if let Some(tap) = installed {
        let paths = |files: Vec<std::path::PathBuf>| {
            strings(files.iter().map(|f| f.display().to_string()).collect())
        };
        map.insert(
            "formula_files".into(),
            paths(rb_files(&tap.path.join("Formula"))),
        );
        map.insert(
            "cask_files".into(),
            paths(rb_files(&tap.path.join("Casks"))),
        );
        map.insert(
            "command_files".into(),
            paths(rb_files(&tap.path.join("cmd"))),
        );
        let default_remote = format!("https://github.com/{user}/homebrew-{repo}");
        let remote = tap_git(tap, &["config", "remote.origin.url"]);
        map.insert(
            "remote".into(),
            remote
                .clone()
                .map_or(aterm_json::Value::Null, aterm_json::Value::String),
        );
        map.insert(
            "custom_remote".into(),
            aterm_json::Value::Bool(remote.as_deref() != Some(default_remote.as_str())),
        );
        map.insert("private".into(), aterm_json::Value::Bool(false));
        map.insert(
            "HEAD".into(),
            aterm_json::Value::String(
                tap_git(tap, &["rev-parse", "--verify", "--quiet", "HEAD"])
                    .unwrap_or_else(|| "(none)".to_string()),
            ),
        );
        map.insert(
            "last_commit".into(),
            aterm_json::Value::String(
                tap_git(tap, &["show", "-s", "--format=%cr", "HEAD"])
                    .unwrap_or_else(|| "never".to_string()),
            ),
        );
        map.insert(
            "branch".into(),
            aterm_json::Value::String(
                tap_git(tap, &["rev-parse", "--abbrev-ref", "HEAD"])
                    .unwrap_or_else(|| "(none)".to_string()),
            ),
        );
    }
    aterm_json::Value::Object(map)
}

/// Brew's installed-tap block (cmd/tap-info.rb `print_tap_info` +
/// `print_tap_listings`). Per-entry decoration uses brew's rescue
/// semantics — installed mark only — because outdated/deprecated
/// would require executing the tap's Ruby.
fn print_tap_detail(prefix: &Prefix, tap: &tap::Tap) {
    let mut info = format!("{}: Installed", tap.name());
    let contents = tap_contents(tap);
    if contents.is_empty() {
        info.push_str("\nNo commands/casks/formulae");
    } else {
        info.push_str(&format!("\n{}", contents.join(", ")));
    }
    info.push_str(&format!(
        "\n{} ({})",
        tap.path.display(),
        keg_abv(&tap.path)
    ));
    let remote = tap_git(tap, &["config", "remote.origin.url"]);
    info.push_str(&format!("\nFrom: {}", remote.as_deref().unwrap_or("N/A")));
    let default_remote = format!("https://github.com/{}/homebrew-{}", tap.user, tap.repo);
    if let Some(url) = &remote
        && *url != default_remote
    {
        info.push_str(&format!("\norigin: {url}"));
    }
    let head = tap_git(tap, &["rev-parse", "--verify", "--quiet", "HEAD"]);
    info.push_str(&format!("\nHEAD: {}", head.as_deref().unwrap_or("(none)")));
    let last = tap_git(tap, &["show", "-s", "--format=%cr", "HEAD"]);
    info.push_str(&format!(
        "\nlast commit: {}",
        last.as_deref().unwrap_or("never")
    ));
    if let Some(branch) = tap_git(tap, &["rev-parse", "--abbrev-ref", "HEAD"])
        && branch != "main"
        && branch != "master"
    {
        info.push_str(&format!("\nbranch: {branch}"));
    }
    println!("{info}");

    let commands = tap_command_names(tap);
    if !commands.is_empty() {
        println!("{}", super::tty::ohai("Commands"));
        println!("{}", commands.join(", "));
    }
    let mut installed = prefix.installed(Kind::Formula);
    installed.extend(prefix.installed(Kind::Cask));
    let is_installed = |name: &str, kind: Kind| {
        installed
            .iter()
            .any(|rec| rec.kind == kind && rec.name == name)
    };
    print_tap_section(
        "Formulae",
        &tap_formula_names(tap),
        Kind::Formula,
        &is_installed,
        remote.as_deref(),
    );
    print_tap_section(
        "Casks",
        &tap_cask_tokens(tap),
        Kind::Cask,
        &is_installed,
        remote.as_deref(),
    );
}

fn print_tap_section(
    label: &str,
    all: &[String],
    kind: Kind,
    is_installed: &dyn Fn(&str, Kind) -> bool,
    remote: Option<&str>,
) {
    if all.is_empty() {
        return;
    }
    let decorate = |name: &String| pretty_search_name(name, None, is_installed(name, kind), false);
    println!("{}", super::tty::ohai(label));
    if all.len() <= 30 {
        let shown: Vec<String> = all.iter().map(decorate).collect();
        print_columns(&shown);
        return;
    }
    let installed: Vec<&String> = all.iter().filter(|name| is_installed(name, kind)).collect();
    if installed.is_empty() {
        super::tty::opoo!(
            "Warning: Tap has more than 30 {} and none are installed.",
            label.to_lowercase()
        );
        if let Some(url) = remote {
            println!("See: {url}");
        }
    } else {
        super::tty::opoo!(
            "Warning: Tap has more than 30 {}; showing only installed entries.",
            label.to_lowercase()
        );
        let shown: Vec<String> = installed.iter().map(|name| decorate(name)).collect();
        print_columns(&shown);
    }
}

pub(super) fn cmd_bundle(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    prepared: Option<Vec<bundle::BrewfileEntry>>,
) -> ExitCode {
    let sub = rest
        .first()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'))
        .unwrap_or("install");
    match sub {
        "status" => {
            bundle::bundle_status(prefix);
            ExitCode::SUCCESS
        }
        "remove" => from_install(prefix, args, bundle::bundle_remove(prefix)),
        "dump" => {
            let text = bundle::brewfile_dump(prefix);
            let mut file: Option<std::path::PathBuf> = None;
            let mut force = false;
            let mut it = rest[1..].iter();
            while let Some(arg) = it.next() {
                if arg == "--file" {
                    file = it.next().map(std::path::PathBuf::from);
                } else if let Some(path) = arg.strip_prefix("--file=") {
                    file = Some(std::path::PathBuf::from(path));
                } else if matches!(arg.as_str(), "--force" | "-f") {
                    force = true;
                }
            }
            match file {
                None => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Some(path) => match write_bundle_dump(&path, text.as_bytes(), force) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => failed(&format!("abrew: cannot write {}: {e}", path.display())),
                },
            }
        }
        "install" | "check" => match prepared {
            Some(entries) => bundle_file(prefix, api, args, &entries, sub),
            None => failed("abrew: Brewfile was not admitted"),
        },
        _ => super::unsupported("bundle subcommand is not native"),
    }
}

fn write_bundle_dump(path: &std::path::Path, text: &[u8], force: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    options.open(path)?.write_all(text)
}

fn bundle_file(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    entries: &[bundle::BrewfileEntry],
    sub: &str,
) -> ExitCode {
    let dry = sub == "check";
    let mut missing = false;
    let mut landed = false;
    let mut failures = 0;
    for entry in entries {
        let result: Result<(), install::InstallError> = match entry {
            bundle::BrewfileEntry::Tap(n) => {
                if dry {
                    let have = bundle::entry_installed(prefix, entry);
                    missing |= !have;
                    println!("tap {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else {
                    match tap::add(prefix, n, None) {
                        Ok(_) => {
                            landed = true;
                            Ok(())
                        }
                        Err(e) => return failed(&e),
                    }
                }
            }
            bundle::BrewfileEntry::Brew(n) => {
                let have = bundle::entry_installed(prefix, entry);
                if dry {
                    missing |= !have;
                    println!("brew {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else if !have {
                    install::install(api, prefix, n, &InstallOpts::default()).map(|_| ())
                } else {
                    Ok(())
                }
            }
            bundle::BrewfileEntry::Cask(n) => {
                let have = bundle::entry_installed(prefix, entry);
                if dry {
                    missing |= !have;
                    println!("cask {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else if !have {
                    if n.contains('/') && !n.starts_with("homebrew/cask/") {
                        Err(install::InstallError::Unsupported(format!(
                            "qualified cask {n} needs its tap recipe"
                        )))
                    } else {
                        match api.cask(n) {
                            Ok(c) => {
                                cask::install_with_deps(api, prefix, &c, false, false, false, false)
                                    .map(|_| ())
                            }
                            Err(e) => return failed(&e),
                        }
                    }
                } else {
                    Ok(())
                }
            }
        };
        match result {
            Ok(()) => {
                if !dry {
                    landed = true;
                }
            }
            Err(e @ install::InstallError::Unsupported(_)) if landed && entries.len() > 1 => {
                let install::InstallError::Unsupported(reason) = &e else {
                    unreachable!()
                };
                eprintln!("abrew: skipping Brewfile entry ({reason})");
                failures += 1;
            }
            Err(e) => return from_install(prefix, args, Err(e)),
        }
    }
    if missing || failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn unlinked_keg_judges_only_receipt_recorded_linkable_kegs() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let mk = |name: &str, keg_only: Option<&str>, with_bin: bool| {
            let dir = p.keg(name, "1.0");
            std::fs::create_dir_all(&dir).unwrap();
            let mut fields = vec![
                ("installed_by", "abrew"),
                ("name", name),
                ("version", "1.0"),
            ];
            if let Some(v) = keg_only {
                fields.push(("keg_only", v));
            }
            p.write_receipt(&dir, &fields).unwrap();
            if with_bin {
                std::fs::create_dir_all(dir.join("bin")).unwrap();
                std::fs::write(dir.join("bin").join(name), b"#!/bin/sh\n").unwrap();
            }
            super::super::prefix::Installed {
                name: name.to_string(),
                version: "1.0".to_string(),
                kind: Kind::Formula,
                versions: vec!["1.0".to_string()],
            }
        };
        let demo = mk("demo", Some("false"), true);
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(keg_bin_linked(&p, "demo", "1.0"), Some(false));
        assert_eq!(linked_keg_version(&p, &demo), None);
        let linked = p.bin().join("demo");
        std::fs::write(&linked, b"foreign file").unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &demo), None);
        std::fs::remove_file(&linked).unwrap();
        let foreign = p.keg("other", "1.0").join("bin/demo");
        std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        std::fs::write(&foreign, b"other keg").unwrap();
        super::super::prefix::symlink_file(&foreign, &linked).unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &demo), None);
        std::fs::remove_file(&linked).unwrap();
        let newer = p.keg("demo", "2.0");
        std::fs::create_dir_all(newer.join("bin")).unwrap();
        std::fs::write(newer.join("bin/demo"), b"newer version").unwrap();
        p.write_receipt(
            &newer,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "2.0"),
                ("keg_only", "false"),
            ],
        )
        .unwrap();
        let admitted = p.installed_one(Kind::Formula, "demo").unwrap();
        super::super::prefix::symlink_file(&p.keg("demo", "1.0"), &p.opt("demo")).unwrap();
        assert_eq!(p.optlinked_version("demo").as_deref(), Some("1.0"));
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo"), &linked)
            .unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("2.0"));
        std::fs::remove_file(&linked).unwrap();
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/1.0/bin/demo"), &linked)
            .unwrap();
        assert!(!unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
        std::fs::write(newer.join("bin/demo-newer"), b"second command").unwrap();
        let second = p.bin().join("demo-newer");
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo-newer"), &second)
            .unwrap();
        assert_eq!(linked_keg_version(&p, &admitted), None);
        std::fs::remove_file(second).unwrap();
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
        std::fs::remove_file(&linked).unwrap();
        super::super::prefix::symlink_file(
            &p.keg("demo", "1.0").join("bin/demo"),
            &p.bin().join("demo"),
        )
        .unwrap();
        assert!(!unlinked_keg(&p, &demo));
        for item in [
            mk("kegonly", Some("true"), true),
            mk("legacy", None, true),
            mk("libonly", Some("false"), false),
        ] {
            assert!(!unlinked_keg(&p, &item));
            assert_eq!(keg_bin_linked(&p, &item.name, &item.version), None);
            assert_eq!(linked_keg_version(&p, &item), None);
        }
        let empty = mk("emptybin", Some("false"), false);
        std::fs::create_dir(p.keg("emptybin", "1.0").join("bin")).unwrap();
        assert_eq!(keg_bin_linked(&p, &empty.name, &empty.version), None);
        assert_eq!(linked_keg_version(&p, &empty), None);
        let unreadable = mk("unreadable", Some("false"), false);
        std::fs::write(p.keg("unreadable", "1.0").join("bin"), b"not a directory").unwrap();
        assert_eq!(
            keg_bin_linked(&p, &unreadable.name, &unreadable.version),
            None
        );
        assert!(!unlinked_keg(&p, &unreadable));
        assert_eq!(linked_keg_version(&p, &unreadable), None);
    }

    #[test]
    fn prune_downloads_respects_age_and_dry_run() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        std::fs::create_dir_all(p.cache().join("api")).unwrap();
        std::fs::write(p.cache().join("demo-1.bottle.tar.gz"), b"x").unwrap();
        std::fs::write(p.cache().join("api/formula-demo.json"), b"{}").unwrap();
        assert_eq!(prune_downloads(&p, true, None, &[]).unwrap().0, 1);
        assert!(p.cache().join("demo-1.bottle.tar.gz").exists());
        assert_eq!(
            prune_downloads(
                &p,
                false,
                Some(std::time::Duration::from_secs(999_999_999)),
                &[]
            )
            .unwrap()
            .0,
            0
        );
        assert_eq!(
            prune_downloads(&p, false, Some(std::time::Duration::ZERO), &[])
                .unwrap()
                .0,
            1
        );
        assert!(!p.cache().join("demo-1.bottle.tar.gz").exists());
        assert!(p.cache().join("api/formula-demo.json").exists());
        let downloads: [(&str, &[u8]); 5] = [
            ("demo-2.bottle.tar.gz", b"ordinary bottle"),
            (
                "demo-extra-1.bottle.tar.gz",
                b"hyphenated name or alpha version",
            ),
            ("demo-extra-release.zip", b"arbitrary cask URL basename"),
            ("demo-extra-1-fetch", b"stray cache file"),
            ("other-1.bottle.tar.gz", b"other package"),
        ];
        for (file, contents) in downloads {
            std::fs::write(p.cache().join(file), contents).unwrap();
        }
        for (dry, max_age, scope) in [
            (false, Some(std::time::Duration::ZERO), &["demo"][..]),
            (false, None, &["demo-extra"][..]),
            (true, None, &["demo"][..]),
            (false, None, &["demo", "other"][..]),
            (false, None, &["oth"][..]),
        ] {
            assert_eq!(prune_downloads(&p, dry, max_age, scope).unwrap().0, 0);
            for (file, contents) in downloads {
                assert_eq!(std::fs::read(p.cache().join(file)).unwrap(), contents);
            }
        }
        assert_eq!(
            prune_downloads(&p, true, None, &[]).unwrap().0,
            downloads.len()
        );
        for (file, contents) in downloads {
            assert_eq!(std::fs::read(p.cache().join(file)).unwrap(), contents);
        }
        assert_eq!(
            prune_downloads(&p, false, None, &[]).unwrap().0,
            downloads.len()
        );
        for (file, _) in downloads {
            assert!(!p.cache().join(file).exists());
        }
        assert_eq!(
            std::fs::read(p.cache().join("api/formula-demo.json")).unwrap(),
            b"{}"
        );
    }

    #[test]
    fn edit_distance_measures_typo_closeness() {
        assert_eq!(edit_distance("hello", "hello"), 0);
        assert_eq!(edit_distance("helo", "hello"), 1);
        assert_eq!(edit_distance("wget", "wgwt"), 1);
        assert_eq!(edit_distance("wget", "wgt"), 1);
        assert_eq!(edit_distance("tree", "tpe"), 2);
        assert!(edit_distance("hello", "goodbye") > 2);
        assert_eq!(edit_distance("", "ab"), 2);
    }

    #[test]
    fn regex_search_matches_brew_case_and_anchoring() {
        assert_eq!(search_regex_pattern("/wget.+/"), Some("wget.+"));
        assert_eq!(search_regex_pattern("/http client/"), Some("http client"));
        assert_eq!(search_regex_pattern("//"), Some(""));
        assert_eq!(search_regex_pattern("/"), None);
        assert_eq!(search_regex_pattern("/wget"), None);
        assert_eq!(search_regex_pattern("wget/"), None);
        assert_eq!(search_regex_pattern("wget"), None);
        assert_eq!(search_regex_pattern("/a\nb/"), None);

        let query = |p: &str| SearchQuery::Regex(compile_search_regex(p).unwrap());
        assert!(query("wget").hits_any(&["wget2"]).unwrap());
        assert!(!query("WGET").hits_any(&["wget"]).unwrap());
        assert!(query("(?i)WGET").hits_any(&["wget"]).unwrap());
        assert!(query("get").hits_any(&["wget"]).unwrap());
        assert!(!query("^get").hits_any(&["wget"]).unwrap());
        assert!(query("^lib.*@\\d$").hits_any(&["libfoo@3"]).unwrap());
        assert!(query("^py").hits_any(&["py-spy"]).unwrap());
        assert!(!query("^py").hits_any(&["airspy"]).unwrap());
        assert!(query("").hits_any(&["anything"]).unwrap());
        assert!(!query("HTTP").hits_any(&["wget"]).unwrap());
        assert!(query("HTTP").hits_any(&["wget", "HTTP client"]).unwrap());

        for refused in [r"(?=http)client", r"(\w)\1", r"\p{L}+", "(a{200}){200}"] {
            assert!(compile_search_regex(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn text_search_simplifies_like_brew_and_desc_fields_select() {
        assert_eq!(simplify_search_text("Node.js C++ @3"), "nodejsc++@3");
        let q = SearchQuery::text("node js");
        assert!(q.hits_any(&["node", "Platform built on Node.js"]).unwrap());
        assert!(SearchQuery::text("alt-tab").hits_any(&["alt-tab"]).unwrap());
        assert!(!q.hits_any(&["deno", "A runtime"]).unwrap());

        let by = |s: &str| SearchQuery::text(s);
        assert!(desc_matches(&by("wget"), DescField::Name, false, "wget", "", "HTTP").unwrap());
        assert!(!desc_matches(&by("http"), DescField::Name, false, "wget", "", "HTTP").unwrap());
        assert!(
            desc_matches(
                &by("http"),
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &by("wget"),
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            desc_matches(
                &by("firefox"),
                DescField::Description,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        assert!(
            desc_matches(
                &by("browser"),
                DescField::Either,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &by("safari"),
                DescField::Either,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        let re = SearchQuery::Regex(compile_search_regex("HTTP").unwrap());
        assert!(
            desc_matches(
                &re,
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &re,
                DescField::Description,
                false,
                "wget",
                "",
                "http client"
            )
            .unwrap()
        );
    }

    fn owned_keg(prefix: &Prefix, name: &str, version: &str) {
        let dir = prefix.keg(name, version);
        std::fs::create_dir_all(&dir).unwrap();
        prefix
            .write_receipt(
                &dir,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", version),
                ],
            )
            .unwrap();
    }

    #[test]
    fn analytics_table_matches_brew_layout_at_80_columns() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(59_538), "59,538");
        assert_eq!(format_count(1_234_567), "1,234,567");
        let rows = [
            ("wget".to_string(), 59_416),
            ("wget --HEAD".to_string(), 122),
        ];
        let table = analytics_table("install", "30", &rows, 80);
        let expected = concat!(
            "==> install (30 days)\n",
            "Index | Name (with options)                                  |  Count |  Percent\n",
            "-----:|------------------------------------------------------|-------:|--------:\n",
            "1     | wget                                                 | 59,416 |   99.80%\n",
            "2     | wget --HEAD                                          |    122 |    0.20%\n",
            "Total |                                                      | 59,538 |  100.00%\n",
        );
        assert_eq!(table, expected);
        let one = analytics_table("install", "30", &rows[..1], 80);
        assert!(!one.contains("Total"), "{one}");
        assert!(one.contains("100.00%"), "{one}");
    }

    #[test]
    fn deprecate_disable_message_matches_brew_rendering() {
        use super::super::formula::DeprecateDisable;
        let dd = DeprecateDisable {
            disable_reason: Some("fails_gatekeeper_check".to_string()),
            disable_date: Some("2999-01-01".to_string()),
            ..Default::default()
        };
        assert_eq!(
            deprecate_disable_message(true, false, true, &dd).as_deref(),
            Some(
                "Disabled because it does not pass the macOS Gatekeeper check! \
                 It will be disabled on 2999-01-01."
            )
        );
        // Deprecation date shifts twelve months (leap day clamps)
        // to the synthesized disable date.
        let dd = DeprecateDisable {
            deprecation_reason: Some("unmaintained".to_string()),
            deprecation_date: Some("2000-02-29".to_string()),
            deprecation_replacement_formula: Some("newtool".to_string()),
            ..Default::default()
        };
        assert_eq!(
            deprecate_disable_message(false, true, false, &dd).as_deref(),
            Some(
                "Deprecated because it is not maintained upstream! \
                 It was disabled on 2001-02-28.\nReplacement:\n  brew install --formula newtool\n"
            )
        );
        // A free-form reason passes through; no reason, bare kind.
        let dd = DeprecateDisable {
            disable_reason: Some("is broken in mysterious ways".to_string()),
            ..Default::default()
        };
        assert_eq!(
            deprecate_disable_message(false, false, true, &dd).as_deref(),
            Some("Disabled because it is broken in mysterious ways!")
        );
        assert_eq!(
            deprecate_disable_message(true, true, false, &DeprecateDisable::default()).as_deref(),
            Some("Deprecated!")
        );
        assert!(deprecate_disable_message(false, false, false, &dd).is_none());
    }

    #[test]
    fn analytics_summary_totals_each_period_in_brew_category_order() {
        let doc = aterm_json::json!({
            "analytics": {
                "build_error": {"30d": {"wget": 23}},
                "install_on_request": {
                    "30d": {"wget": 46000, "wget --HEAD": 187},
                    "90d": {"wget": 88049},
                    "365d": {"wget": 321504}
                },
                "install": {
                    "30d": {"wget": 46322, "wget --HEAD": 122},
                    "90d": {"wget": 88791},
                    "365d": {"wget": 325445}
                }
            }
        });
        assert_eq!(
            analytics_summary_lines(Some(&doc)),
            [
                "install: 46,444 (30 days), 88,791 (90 days), 325,445 (365 days)",
                "install-on-request: 46,187 (30 days), 88,049 (90 days), 321,504 (365 days)",
                "build-error: 23 (30 days)",
            ]
        );
        assert!(analytics_summary_lines(None).is_empty());
        assert!(analytics_summary_lines(Some(&aterm_json::json!({"analytics": null}))).is_empty());
    }

    #[test]
    fn multiple_version_lines_keep_only_multi_version_formulae_sorted() {
        let row = |name: &str, versions: &[&str]| Installed {
            name: name.into(),
            version: versions
                .last()
                .map(|v| (*v).to_string())
                .unwrap_or_default(),
            kind: Kind::Formula,
            versions: versions.iter().map(|v| (*v).to_string()).collect(),
        };
        let rows = [
            row("zeta", &["1", "2"]),
            row("solo", &["3"]),
            row("alpha", &["0.9", "1.0", "1.1"]),
            row("none", &[]),
        ];
        assert_eq!(
            multiple_version_lines(&rows),
            ["alpha 0.9 1.0 1.1", "zeta 1 2"]
        );
    }

    #[test]
    fn flat_dependencies_close_over_the_full_graph_sorted() {
        let formula = |name: &str, deps: &[&str]| Formula {
            name: name.into(),
            dependencies: deps.iter().map(|name| (*name).to_string()).collect(),
            ..Formula::default()
        };
        let app = formula("app", &["zeta", "homebrew/core/mid", "zeta"]);
        let read = |name: &str| match name {
            "mid" => Some(formula("mid", &["leaf"])),
            "leaf" => Some(formula("leaf", &["app"])),
            "zeta" => Some(formula("zeta", &[])),
            _ => None,
        };
        let direct = flat_dependencies(&app, true, |_| {
            panic!("direct sets read nothing below the root")
        });
        assert_eq!(
            direct.iter().collect::<Vec<_>>(),
            [&"mid".to_string(), &"zeta".to_string()]
        );
        let all = flat_dependencies(&app, false, read);
        assert_eq!(
            all.iter().collect::<Vec<_>>(),
            [&"leaf".to_string(), &"mid".to_string(), &"zeta".to_string()]
        );
    }

    #[test]
    fn completion_caveats_follow_brews_keg_checks() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let keg = tmp.path().join("keg");
        for (dir, file) in [
            ("share/zsh/site-functions", "_demo"),
            ("share/fish/vendor_completions.d", "demo.fish"),
            ("share/fish/vendor_functions.d", "demo_fn.fish"),
            ("etc/bash_completion.d", "demo"),
            ("share/emacs/site-lisp/demo", "demo.el"),
        ] {
            let dir = keg.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let f = Formula {
            name: "demo".to_string(),
            ..Formula::default()
        };
        let root = prefix.root.display().to_string();
        assert_eq!(
            completions_and_elisp_for(&prefix, &f, &keg, &["zsh"], &|_| true),
            vec![
                format!(
                    "zsh completions have been installed to:\n  {root}/share/zsh/site-functions\n"
                ),
                format!(
                    "Emacs Lisp files have been installed to:\n  {root}/share/emacs/site-lisp/demo\n"
                ),
            ]
        );
        assert_eq!(
            completions_and_elisp_for(&prefix, &f, &keg, &["fish"], &|_| true)[0],
            format!(
                "fish completions and functions have been installed to:\n  {root}/share/fish/vendor_completions.d\n  {root}/share/fish/vendor_functions.d\n"
            )
        );
        // A shell not on PATH contributes nothing.
        assert_eq!(
            completions_and_elisp_for(&prefix, &f, &keg, &["zsh"], &|_| false).len(),
            1,
            "only the elisp line remains"
        );
        // Keg-only formulae point at the opt prefix and skip elisp.
        let keg_only = Formula {
            name: "demo".to_string(),
            keg_only: true,
            ..Formula::default()
        };
        assert_eq!(
            completions_and_elisp_for(&prefix, &keg_only, &keg, &["bash"], &|_| true),
            vec![format!(
                "Bash completion has been installed to:\n  {}/etc/bash_completion.d\n",
                prefix.opt("demo").display()
            )]
        );
    }

    #[test]
    fn strip_bottle_block_matches_brew_api_source() {
        let full = "class Hello < Formula\n  license \"GPL\"\n\n  bottle do\n    rebuild 1\n    sha256 cellar: :any, arm64: \"x\"\n  end\n\n  def install\n  end\nend\n";
        assert_eq!(
            strip_bottle_block(full),
            "class Hello < Formula\n  license \"GPL\"\n\n  def install\n  end\nend\n"
        );
        // No block, and a block with no closing end, pass through.
        assert_eq!(strip_bottle_block("class X\nend\n"), "class X\nend\n");
        let unclosed = "  bottle do\n    sha256 \"x\"\n";
        assert_eq!(strip_bottle_block(unclosed), unclosed);
        // A `pour_bottle?` line is not a bottle block.
        let pour = "  def pour_bottle?\n    true\n  end\n";
        assert_eq!(strip_bottle_block(pour), pour);
    }

    #[test]
    fn erb_url_encode_matches_ruby() {
        assert_eq!(
            erb_url_encode("a b~c@d:e#f_g-h.i"),
            "a%20b~c%40d%3Ae%23f_g-h.i"
        );
        // The GraphvizOnline fragment brew 7.0 opens for `deps --graph jq`.
        assert_eq!(
            erb_url_encode("digraph {\n  \"jq\" -> \"oniguruma\"\n}"),
            "digraph%20%7B%0A%20%20%22jq%22%20-%3E%20%22oniguruma%22%0A%7D"
        );
    }

    #[test]
    fn deps_tree_matches_brews_continuations_and_circular_marks() {
        let row = |name: &str, deps: &[&str]| super::super::api::DepRow {
            name: name.to_string(),
            full_name: name.to_string(),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
            ..Default::default()
        };
        let rows: std::collections::BTreeMap<String, super::super::api::DepRow> = [
            row("app", &["kept", "last"]),
            row("kept", &["leaf", "child"]),
            row("leaf", &["kept"]),
            row("child", &[]),
            row("last", &["leaf"]),
        ]
        .into_iter()
        .map(|r| (r.name.clone(), r))
        .collect();
        let read = |name: &str| rows.get(name).cloned();
        let never = |_: &str| false;
        let expander = DepExpander::new(
            &read,
            super::super::api::DepFilter::default(),
            false,
            &never,
        );
        let display =
            |_: &DepExpander, name: &str, _: &super::super::api::DepTags| name.to_string();
        let filter = super::super::api::DepFilter::default();
        let mut lines = Vec::new();
        let mut seen = Default::default();
        let mut circular = false;
        deps_tree_lines(
            &expander,
            &rows["app"],
            &filter,
            false,
            &never,
            false,
            true,
            false,
            &Prefix::at(std::path::Path::new("/tmp/abrew-test-nonexistent")),
            &display,
            &mut seen,
            "",
            &mut circular,
            &mut lines,
        );
        assert_eq!(
            lines,
            [
                "├── kept",
                "│   ├── leaf",
                "│   │   └── kept (CIRCULAR DEPENDENCY)",
                "│   └── child",
                "└── last",
                "    └── leaf",
                "        └── kept",
                "            ├── leaf (CIRCULAR DEPENDENCY)",
                "            └── child",
            ]
        );
        assert!(circular, "circular roots fail the command like brew");

        let mut pruned = Vec::new();
        let mut seen = Default::default();
        let mut circular = false;
        deps_tree_lines(
            &expander,
            &rows["app"],
            &filter,
            false,
            &never,
            true,
            true,
            false,
            &Prefix::at(std::path::Path::new("/tmp/abrew-test-nonexistent")),
            &display,
            &mut seen,
            "",
            &mut circular,
            &mut pruned,
        );
        assert_eq!(
            pruned,
            [
                "├── kept",
                "│   ├── leaf",
                "│   │   └── kept (CIRCULAR DEPENDENCY)",
                "│   └── child",
                "└── last",
                "    └── leaf (PRUNED)",
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn long_listing_dates_keep_each_instants_offset_and_handle_extreme_ages() {
        const NOW: i64 = 1_800_000_000;
        const OLD: i64 = NOW - 400 * 86_400;
        const BEFORE: i64 = 1_710_063_000;
        const AFTER: i64 = BEFORE + 3_600;
        assert_eq!(BEFORE.div_euclid(86_400), AFTER.div_euclid(86_400));
        let mut dated = LongLister {
            users: Default::default(),
            groups: Default::default(),
            offsets: [
                (NOW, 0),
                (OLD, 0),
                (BEFORE, -28_800),
                (AFTER, -25_200),
                (-1, 0),
                (i64::MAX, 3_600),
                (i64::MIN, -3_600),
            ]
            .into(),
            now: NOW,
        };
        assert_eq!(dated.date(NOW), "Jan 15 08:00");
        assert_eq!(dated.date(OLD), "Dec 11  2025");
        dated.now = AFTER;
        assert_eq!(dated.date(BEFORE), "Mar 10 01:30");
        assert_eq!(dated.date(AFTER), "Mar 10 03:30");

        for (now, mtime, expected) in [
            (i64::MIN, BEFORE, "Mar 10  2024"),
            (i64::MAX, -1, "Dec 31  1969"),
            (i64::MAX, i64::MAX, "Dec  4 16:30"),
            (i64::MIN, i64::MIN, "Jan 27 07:29"),
        ] {
            dated.now = now;
            assert_eq!(dated.date(mtime), expected, "now={now}, mtime={mtime}");
        }
    }

    #[test]
    fn core_shard_puts_lib_formulae_under_lib() {
        assert_eq!(core_shard("libffi"), "lib");
        assert_eq!(core_shard("lib"), "lib");
        assert_eq!(core_shard("tree"), "t");
        assert_eq!(core_shard("openssl@3"), "o");
    }

    #[test]
    fn macho_magic_and_executable_filetype() {
        assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]));
        assert!(!is_macho(b"#!/bin/sh"));
        assert!(
            !is_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]),
            "a Java class file"
        );
        // Thin LE 64-bit header: magic, cputype, cpusubtype, filetype.
        let mut exe = vec![0xcdu8; 0];
        exe.extend_from_slice(&[
            0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01, 0, 0, 0, 0, 2, 0, 0, 0,
        ]);
        assert!(macho_is_executable(&exe));
        exe[12] = 6; // MH_DYLIB
        assert!(!macho_is_executable(&exe));

        use crate::macho::fixtures::{
            ID_DYLIB, LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, thin, thin32,
        };
        let files = aterm_tempfile::tempdir().expect("owned keg directory");
        let tool = thin(
            &[
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
                dylib_command(LOAD_DYLIB, "@rpath/libfoo.dylib", true),
            ],
            true,
        );
        std::fs::write(files.path().join("tool"), &tool).unwrap();
        assert_eq!(
            linked_dylibs(&files.path().join("tool")).unwrap(),
            ["/usr/lib/libSystem.B.dylib", "@rpath/libfoo.dylib"]
        );
        let lib = thin(
            &[
                dylib_command(ID_DYLIB, "@rpath/libfoo.dylib", false),
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", false),
            ],
            false,
        );
        std::fs::write(files.path().join("libfoo"), fat(&[lib.clone(), lib], true)).unwrap();
        assert_eq!(
            linked_dylibs(&files.path().join("libfoo")).unwrap(),
            ["@rpath/libfoo.dylib", "/usr/lib/libSystem.B.dylib"]
        );
        let watch = thin32(
            &[dylib_command32(
                LOAD_DYLIB,
                "/usr/lib/libSystem.B.dylib",
                true,
            )],
            true,
        );
        std::fs::write(files.path().join("watch"), &watch).unwrap();
        assert!(is_macho(&watch[..8]));
        assert_eq!(
            linked_dylibs(&files.path().join("watch")).unwrap(),
            ["/usr/lib/libSystem.B.dylib"]
        );
        std::fs::write(
            files.path().join("libx.a"),
            fat(&[archive(), archive()], false),
        )
        .unwrap();
        assert!(
            linked_dylibs(&files.path().join("libx.a"))
                .unwrap()
                .is_empty()
        );
        std::fs::write(files.path().join("static"), thin(&[], true)).unwrap();
        assert!(
            linked_dylibs(&files.path().join("static"))
                .unwrap()
                .is_empty()
        );
        std::fs::write(files.path().join("cut"), &tool[..40]).unwrap();
        assert!(
            linked_dylibs(&files.path().join("cut"))
                .is_err_and(|e| e.starts_with("inspection failed: malformed"))
        );
        assert!(linked_dylibs(&files.path().join("absent")).is_err());

        let scratch = aterm_tempfile::tempdir().expect("owned library directory");
        let library = scratch.path().join("libok.dylib");
        std::fs::write(&library, b"owned library bytes").unwrap();
        assert!(linked_library_exists(&library));
        assert!(!linked_library_exists(scratch.path()));
        assert!(!linked_library_exists(&scratch.path().join("absent.dylib")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = scratch.path().join("libalias.dylib");
            let dangling = scratch.path().join("libgone.dylib");
            symlink("libok.dylib", &alias).unwrap();
            symlink("absent.dylib", &dangling).unwrap();
            assert!(linked_library_exists(&alias));
            assert!(!linked_library_exists(&dangling));
        }
    }

    #[test]
    fn unpin_splits_like_brew() {
        assert_eq!(unpin_outcome(true, true, false), UnpinOutcome::Remove);
        assert_eq!(unpin_outcome(true, false, false), UnpinOutcome::Remove);
        assert_eq!(
            unpin_outcome(false, true, false),
            UnpinOutcome::WarnNotPinned
        );
        assert_eq!(
            unpin_outcome(false, false, false),
            UnpinOutcome::ErrorNotInstalled
        );
        assert_eq!(unpin_outcome(false, false, true), UnpinOutcome::FailUnknown);

        let url = "https://formulae.brew.sh/api/formula/pkg404.json";
        for code in [Some(22), Some(56)] {
            let missing = fetch::parse_get_response(
                url,
                code,
                b"\n404".to_vec(),
                b"curl: (56) The requested URL returned error: 404",
            )
            .unwrap_err();
            assert!(missing.is_not_found(), "code={code:?}");
            assert_eq!(
                unpin_outcome(false, false, missing.is_not_found()),
                UnpinOutcome::FailUnknown
            );
        }
        for (code, response) in [
            (Some(6), b"\n000".as_slice()),
            (Some(28), b"\n404".as_slice()),
            (Some(22), b"\n500".as_slice()),
            (None, b"\n404".as_slice()),
            (Some(22), b"\n40".as_slice()),
            (Some(22), b"\n40x".as_slice()),
            (Some(22), b"\n404junk".as_slice()),
            (Some(22), b"\n999".as_slice()),
            (Some(22), b"".as_slice()),
        ] {
            let error = fetch::parse_get_response(
                url,
                code,
                response.to_vec(),
                b"transport failure mentioning 404",
            )
            .unwrap_err();
            assert!(!error.is_not_found(), "{code:?} {response:?}");
            assert_eq!(
                unpin_outcome(false, false, error.is_not_found()),
                UnpinOutcome::ErrorNotInstalled
            );
            assert_eq!(
                error.into_message(),
                format!("abrew: download failed ({url}): transport failure mentioning 404")
            );
        }
        assert_eq!(
            fetch::parse_get_response(url, Some(0), b"body\n404\n200".to_vec(), b"").unwrap(),
            b"body\n404"
        );
        assert_eq!(
            fetch::parse_get_response(url, Some(0), vec![0xff, b'\n', b'0', b'0', b'0'], b"")
                .unwrap(),
            [0xff]
        );
        assert!(fetch::parse_get_response(url, Some(0), b"no status".to_vec(), b"").is_err());
    }

    #[test]
    fn outdated_fails_only_for_named_operands() {
        assert!(!outdated_failed(false, false));
        assert!(!outdated_failed(false, true));
        assert!(!outdated_failed(true, false), "bare census reports, rc 0");
        assert!(outdated_failed(true, true), "named + outdated fails");
    }

    #[test]
    fn cask_shard_keys_fonts_by_the_letter_after_font() {
        assert_eq!(cask_shard("alt-tab"), "a");
        assert_eq!(cask_shard("font-monoid"), "font/font-m");
        assert_eq!(cask_shard("font-iosevka-ss09"), "font/font-i");
        assert_eq!(cask_shard("firefox"), "f");
    }

    #[test]
    #[cfg(unix)]
    fn long_listing_renders_modes_dates_and_link_targets() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("tool");
        std::fs::write(&file, b"12345").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            mode_string(&std::fs::metadata(&file).unwrap()),
            "-rwxr-xr-x"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4750)).unwrap();
        assert_eq!(
            mode_string(&std::fs::metadata(&file).unwrap()),
            "-rwsr-x---"
        );
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(mode_string(&std::fs::metadata(&dir).unwrap()), "drwxrwxrwt");
        let link = tmp.path().join("link");
        super::super::prefix::symlink_file(&file, &link).unwrap();
        let mut lister = LongLister::new();
        let line = lister.line(&link);
        let link_mode = mode_string(&std::fs::symlink_metadata(&link).unwrap());
        assert!(link_mode.starts_with('l'), "{link_mode}");
        assert!(line.starts_with(&link_mode), "{line}");
        assert!(line.contains(" -> "), "{line}");
        let line = lister.line(&file);
        assert!(line.contains("       5 "), "{line}");
    }

    #[test]
    #[cfg(unix)]
    fn bundle_dump_reserves_a_new_name_without_following_existing_links() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("Brewfile");
        write_bundle_dump(&file, b"first", false).unwrap();
        assert!(write_bundle_dump(&file, b"second", false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"first");
        write_bundle_dump(&file, b"forced", true).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"forced");
        let link = tmp.path().join("link");
        super::super::prefix::symlink_file(&file, &link).unwrap();
        assert!(write_bundle_dump(&link, b"changed", false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"forced");
        let absent = tmp.path().join("absent");
        let dangling = tmp.path().join("dangling");
        super::super::prefix::symlink_file(&absent, &dangling).unwrap();
        assert!(write_bundle_dump(&dangling, b"created", false).is_err());
        assert!(!absent.exists());
        assert_eq!(std::fs::read_link(&dangling).unwrap(), absent);
    }

    #[test]
    fn on_refuses_an_occupied_marker_before_store_or_stub_changes() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        std::fs::create_dir(prefix.disabled_path()).unwrap();
        std::fs::write(prefix.disabled_path().join("owner"), b"keep").unwrap();
        assert_eq!(cmd_on(&prefix), ExitCode::from(1));
        assert_eq!(
            std::fs::read(prefix.disabled_path().join("owner")).unwrap(),
            b"keep"
        );
        assert!(prefix.is_disabled());
    }

    /// Unix only: a pin is a symlink, which abrew refuses to make elsewhere.
    #[test]
    #[cfg(unix)]
    fn cleanup_preserves_an_older_pin_foreign_versions_and_other_packages() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        for version in ["1", "2"] {
            owned_keg(&prefix, "wget", version);
        }
        prefix.pin("wget").unwrap();
        for version in ["3", "4"] {
            owned_keg(&prefix, "wget", version);
        }
        for version in ["1", "2"] {
            owned_keg(&prefix, "curl", version);
        }
        let foreign = prefix.keg("wget", "999");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("owner"), b"keep").unwrap();
        #[cfg(unix)]
        {
            std::fs::create_dir_all(prefix.bin()).unwrap();
            std::fs::write(prefix.keg("wget", "3").join("tool"), b"x").unwrap();
            std::os::unix::fs::symlink(
                prefix.keg("wget", "3").join("tool"),
                prefix.bin().join("wget-old"),
            )
            .unwrap();
            std::fs::write(prefix.keg("wget", "4").join("tool"), b"current").unwrap();
            std::os::unix::fs::symlink(
                prefix.keg("wget", "3").join("../4/tool"),
                prefix.bin().join("wget-current"),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(prefix.bin().join("wget-current")).unwrap(),
                b"current"
            );
            std::os::unix::fs::symlink(
                "../Cellar/wget/3/tool",
                prefix.bin().join("wget-old-relative"),
            )
            .unwrap();
        }
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), true, &["wget"])
                .unwrap()
                .0,
            2
        );
        assert!(prefix.keg("wget", "1").is_dir());
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), false, &["wget"])
                .unwrap()
                .0,
            2
        );
        for version in ["1", "3"] {
            assert!(!prefix.keg("wget", version).exists());
        }
        for version in ["2", "4"] {
            assert!(prefix.keg("wget", version).is_dir());
        }
        assert_eq!(std::fs::read(foreign.join("owner")).unwrap(), b"keep");
        assert!(prefix.keg("curl", "1").is_dir());
        assert_eq!(prefix.pinned_version("wget").as_deref(), Some("2"));
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old")).is_err());
            assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old-relative")).is_err());
            assert_eq!(
                std::fs::read_link(prefix.bin().join("wget-current")).unwrap(),
                prefix.keg("wget", "3").join("../4/tool"),
            );
            assert_eq!(
                std::fs::read(prefix.keg("wget", "4").join("tool")).unwrap(),
                b"current"
            );
        }
    }

    #[test]
    fn info_json_enrichment_reports_installed_state() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        prefix.ensure_api_cache().unwrap();
        let api_dir = prefix.cache().join("api");
        std::fs::write(
            api_dir.join("formula-demo.json"),
            br#"{"name":"demo","versions":{"stable":"2"},"dependencies":["dep"]}"#,
        )
        .unwrap();
        std::fs::write(
            api_dir.join("formula-dep.json"),
            br#"{"name":"dep","versions":{"stable":"3"}}"#,
        )
        .unwrap();
        let keg = prefix.keg("demo", "1");
        std::fs::create_dir_all(&keg).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", "demo"),
                    ("version", "1"),
                    ("installed_on_request", "true"),
                    ("full_name", "demo"),
                ],
            )
            .unwrap();
        owned_keg(&prefix, "dep", "3");
        let api = Api::new(prefix.clone());
        let mut record = aterm_json::json!({"name": "demo"});
        enrich_formula_json(&prefix, &api, &mut record);
        // Brew's API-mode to_hash: requirements empty, patches untyped.
        let mut rich = aterm_json::json!({
            "name": "demo",
            "requirements": [{"name": "macos", "version": "12"}],
            "patches": [{"strip": "p1", "url": "https://x/y.diff", "type": "backport"}]
        });
        enrich_formula_json(&prefix, &api, &mut rich);
        assert_eq!(rich.get("requirements"), Some(&aterm_json::json!([])));
        assert!(
            rich.get("patches").unwrap()[0].get("type").is_none(),
            "patch type dropped like brew's API loader"
        );
        // Cask side: placeholders expand, language variations null out.
        let mut cask = aterm_json::json!({
            "token": "demo-cask",
            "version": "1",
            "language_variations": [{"languages": ["en"]}],
            "artifacts": [{"binary": ["$APPDIR/Demo.app/tool"], "target": "$HOMEBREW_PREFIX/bin/tool"}]
        });
        enrich_cask_json(&prefix, &mut cask);
        assert_eq!(
            cask.get("language_variations"),
            Some(&aterm_json::Value::Null)
        );
        let arts = cask.get("artifacts").unwrap().to_string();
        assert!(
            !arts.contains("$APPDIR") && !arts.contains("$HOMEBREW_PREFIX"),
            "{arts}"
        );
        let installed = record
            .get("installed")
            .and_then(aterm_json::Value::as_array)
            .unwrap();
        assert_eq!(installed.len(), 1);
        let row = &installed[0];
        assert_eq!(row.get("version").unwrap(), &aterm_json::json!("1"));
        assert_eq!(
            row.get("installed_on_request").unwrap(),
            &aterm_json::json!(true)
        );
        assert!(row.get("time").is_some_and(|t| !t.is_null()));
        let runtime = row
            .get("runtime_dependencies")
            .and_then(aterm_json::Value::as_array)
            .unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(
            runtime[0].get("full_name").unwrap(),
            &aterm_json::json!("dep")
        );
        assert_eq!(
            runtime[0].get("declared_directly").unwrap(),
            &aterm_json::json!(true)
        );
        assert!(record.get("linked_keg").is_some_and(|v| v.is_null()));
        assert_eq!(record.get("pinned").unwrap(), &aterm_json::json!(false));
        assert_eq!(record.get("outdated").unwrap(), &aterm_json::json!(true));
        let mut absent = aterm_json::json!({"name": "dep"});
        enrich_formula_json(&prefix, &api, &mut absent);
        assert_eq!(
            absent
                .get("installed")
                .and_then(aterm_json::Value::as_array)
                .unwrap()
                .len(),
            1,
            "dep itself is installed"
        );
    }

    #[test]
    fn disk_usage_readable_matches_brew_units() {
        assert_eq!(disk_usage_readable(0), "0B");
        assert_eq!(disk_usage_readable(999), "999B");
        assert_eq!(disk_usage_readable(1_000), "1KB");
        assert_eq!(disk_usage_readable(1_500), "1.5KB");
        assert_eq!(disk_usage_readable(193_300), "193.3KB");
        assert_eq!(disk_usage_readable(1_000_000), "1MB");
        assert_eq!(disk_usage_readable(1_050_000), "1.1MB");
        assert_eq!(disk_usage_readable(2_500_000_000), "2.5GB");
    }

    #[test]
    fn cask_bundle_versions_read_the_staged_app_plist() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cask");
        let contents = dir.join("Widget.app/Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<?xml version=\"1.0\"?><plist><dict>\
             <key>CFBundleShortVersionString</key><string>2.5 (409)</string>\
             <key>CFBundleVersion</key><string>409</string>\
             </dict></plist>",
        )
        .unwrap();
        // Brew strips the trailing " (<version>)" from the short form.
        assert_eq!(
            cask_bundle_versions(&dir),
            (Some("2.5".to_string()), Some("409".to_string()))
        );
        assert_eq!(cask_bundle_versions(tmp.path()), (None, None));
    }

    #[test]
    fn spell_correct_matches_did_you_mean() {
        let dict: Vec<String> = ["zsxd", "zstd", "firefox", "firefly", "hello"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // The oracle's own outcomes: `brew search zstd` also finds
        // zsxd, and `brew search firefx` finds firefox (exact input
        // is never its own correction).
        assert_eq!(spell_correct(&dict, "zstd"), ["zsxd"]);
        assert_eq!(spell_correct(&dict, "firefx"), ["firefox", "firefly"]);
        assert!(spell_correct(&dict, "zzzzzz").is_empty());
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(levenshtein_distance(&chars("kitten"), &chars("sitting")), 3);
        assert!(jaro_winkler_distance(&chars("zsxd"), &chars("zstd")) >= 0.834);
        let sentence = |items: &[&str]| {
            to_sentence_or(&items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
        };
        assert_eq!(sentence(&[]), "");
        assert_eq!(sentence(&["wget"]), "wget");
        assert_eq!(sentence(&["wget", "wgo"]), "wget or wgo");
        assert_eq!(
            sentence(&["wget", "wgo", "vgt", "ngt"]),
            "wget, wgo, vgt or ngt"
        );
        assert_eq!(
            cask_unavailable("nosuch"),
            "Cask 'nosuch' is unavailable: No Cask with this name exists."
        );
    }

    #[test]
    fn missing_formula_reasons_match_brew() {
        assert_eq!(
            disallowed_reason("pip").as_deref(),
            Some("pip is part of the python formula:\n  brew install python\n")
        );
        assert!(disallowed_reason("rubygems").is_some_and(|r| r.contains("brew install ruby")));
        assert!(disallowed_reason("liblzma").is_some_and(|r| r.contains("brew install xz")));
        assert!(disallowed_reason("wget").is_none());

        // Migration message shapes from a fabricated tap map.
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let tap_dir = p.taps_dir().join("me").join("homebrew-mine");
        std::fs::create_dir_all(tap_dir.join(".git")).expect("mkdir");
        std::fs::write(
            tap_dir.join("tap_migrations.json"),
            r#"{"oldthing": "homebrew/cask/newthing"}"#,
        )
        .expect("write");
        let api = Api::read_only(p.clone());
        let reason = tap_migration_reason(&p, &api, "oldthing").expect("reason");
        assert_eq!(
            reason,
            "It was migrated from me/mine to homebrew/cask/newthing.\n\
             You can access it again by running:\n  brew tap homebrew/cask\n\
             And then you can install it by running:\n  brew install --cask newthing\n"
        );
    }

    #[test]
    fn doctor_and_cleanup_see_broken_links() {
        assert!(DOCTOR_CHECKS.contains(&"check_for_broken_symlinks"));
        #[cfg(unix)]
        {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let p = Prefix::at(tmp.path());
            p.ensure().unwrap();
            let bin = p.root.join("bin");
            std::os::unix::fs::symlink("/nonexistent/abrew-r134-target", bin.join("dead"))
                .expect("symlink");
            let deep = p.root.join("share/man/man1");
            std::fs::create_dir_all(&deep).expect("mkdir");
            std::os::unix::fs::symlink("/nonexistent/abrew-r134-target", deep.join("dead.1"))
                .expect("symlink");
            std::os::unix::fs::symlink("/bin/sh", bin.join("alive")).expect("symlink");
            let broken = broken_prefix_links(&p);
            assert_eq!(broken.len(), 2, "bin and share broken links");
            assert!(broken.iter().all(|b| b.file_name().unwrap() != "alive"));
            prune_prefix_links(&p, false);
            assert!(
                broken_prefix_links(&p).is_empty(),
                "cleanup prune removed them"
            );
            assert!(bin.join("alive").exists(), "live link kept");
            assert!(bin.is_dir(), "anchor kept");
        }
    }

    #[test]
    fn tap_info_pieces_match_brew() {
        assert_eq!(pluralize_with_count("formula", 1), "1 formula");
        assert_eq!(pluralize_with_count("formula", 3), "3 formulae");
        assert_eq!(pluralize_with_count("tap", 0), "0 taps");
        assert_eq!(pluralize_with_count("command", 2), "2 commands");

        let dir = std::env::temp_dir().join(format!("abrew-r133-tap-{}", std::process::id()));
        let tap_path = dir.join("homebrew-testtap");
        for sub in ["Formula/w", "Casks", "cmd"] {
            std::fs::create_dir_all(tap_path.join(sub)).expect("mkdir");
        }
        std::fs::write(tap_path.join("Formula/w/wget.rb"), "#").expect("write");
        std::fs::write(tap_path.join("Formula/hello.rb"), "#").expect("write");
        std::fs::write(tap_path.join("Casks/firefox.rb"), "#").expect("write");
        std::fs::write(tap_path.join("cmd/brew-hello-cmd.rb"), "#").expect("write");
        let tap = tap::Tap {
            user: "me".to_string(),
            repo: "testtap".to_string(),
            path: tap_path,
        };
        assert_eq!(tap_formula_names(&tap), vec!["hello", "wget"]);
        assert_eq!(tap_cask_tokens(&tap), vec!["firefox"]);
        assert_eq!(tap_command_names(&tap), vec!["hello-cmd"]);
        assert_eq!(
            tap_contents(&tap),
            vec!["1 command", "1 cask", "2 formulae"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ls_columns_matches_bsd_ls() {
        // Oracle: `/bin/ls` of these names under a pty (this host).
        let names: Vec<String> = [
            "a",
            "bb",
            "cccccccccccccccccc",
            "ddd",
            "ee",
            "ffffffffff",
            "gg",
            "h",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            ls_columns(&names, 80),
            "a\t\t\tddd\t\t\tgg\nbb\t\t\tee\t\t\th\ncccccccccccccccccc\tffffffffff\n"
        );
        // One column when the tab-rounded width leaves room for only one.
        assert_eq!(
            ls_columns(&names, 24),
            "a\nbb\ncccccccccccccccccc\nddd\nee\nffffffffff\ngg\nh\n"
        );
        assert_eq!(ls_columns(&[], 80), "");
    }

    #[test]
    fn format_columns_matches_brews_layout() {
        let names: Vec<String> = ["a", "bb", "ccc", "dd", "e"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // Oracle: brew's Formatter.columns at width 12 (pty-captured).
        assert_eq!(format_columns(&names, 12, true), "a      dd\nbb     e\nccc");
        // Piped output is always one per line, at any width.
        assert_eq!(format_columns(&names, 80, false), "a\nbb\nccc\ndd\ne");
        // Too narrow for two columns falls back to lines.
        assert_eq!(format_columns(&names, 4, true), "a\nbb\nccc\ndd\ne");
    }

    #[test]
    fn dry_change_lines_render_install_and_upgrade_rows() {
        let fresh = |n: &str, v: &str| install::DryRow {
            name: n.into(),
            old_version: None,
            new_version: v.into(),
        };
        // Oracle sample (brew install --dry-run): one space, name then
        // the new version alone.
        assert_eq!(
            dry_change_lines(&[fresh("wget", "1.25.0_2")]),
            ["wget 1.25.0_2"]
        );
        assert_eq!(
            dry_change_lines(&[fresh("libpsl", "0.23.3")]),
            ["libpsl 0.23.3"]
        );
        let upgraded = install::DryRow {
            name: "ca-certificates".into(),
            old_version: Some("2026-09-25_1".into()),
            new_version: "2026-10-20".into(),
        };
        assert_eq!(
            dry_change_lines(&[upgraded]),
            ["ca-certificates 2026-09-25_1 -> 2026-10-20"]
        );
    }

    #[test]
    fn version_change_lines_align_like_brew() {
        let row = |n: &str, o: &str, v: &str, s: Option<&str>| {
            (n.into(), o.into(), v.into(), s.map(str::to_owned))
        };
        // Oracle sample (brew upgrade --dry-run, redirected).
        let lines = version_change_lines(&[
            row("brotli", "1.2.0", "1.2.1", Some("791.9KB")),
            row(
                "ca-certificates",
                "2026-09-25_1",
                "2026-10-20",
                Some("109.6KB"),
            ),
            row("gh", "2.102.0", "2.103.0", None),
        ]);
        assert_eq!(
            lines,
            [
                "brotli           1.2.0        -> 1.2.1       (791.9KB)",
                "ca-certificates  2026-09-25_1 -> 2026-10-20  (109.6KB)",
                "gh               2.102.0      -> 2.103.0",
            ]
        );
        // A single change joins with one space.
        assert_eq!(
            version_change_lines(&[row("xz", "5.8.3", "5.8.4", None)]),
            ["xz 5.8.3 -> 5.8.4"]
        );
    }

    #[test]
    fn to_sentence_joins_like_brew() {
        let s = |v: &[&str]| to_sentence(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&[]), "");
        assert_eq!(s(&["a"]), "a");
        assert_eq!(s(&["a", "b"]), "a and b");
        assert_eq!(s(&["a", "b", "c"]), "a, b, and c");
    }

    #[test]
    fn update_report_diffs_indexes_like_brew() {
        use super::super::api::IndexRow;
        let row = |name: &str, version: &str| IndexRow {
            name: name.into(),
            desc: format!("{name} desc"),
            version: version.into(),
            auto_updates: false,
        };
        let inst = |name: &str, version: &str| Installed {
            kind: Kind::Formula,
            name: name.into(),
            version: version.into(),
            versions: vec![version.into()],
        };
        let old = vec![row("gone", "1"), row("kept", "1"), row("old-inst", "1")];
        let new = vec![row("kept", "2"), row("fresh", "1"), row("old-inst", "1")];
        let installed = vec![inst("kept", "1"), inst("gone", "1")];
        let (added, deleted, outdated) = update_report(Some(&old), &new, &installed, true);
        assert_eq!(added, [("fresh".to_string(), "fresh desc".to_string())]);
        assert_eq!(deleted, ["gone"]);
        assert_eq!(outdated, ["kept"]);
        let (added, deleted, outdated) = update_report(None, &new, &installed, true);
        assert!(added.is_empty() && deleted.is_empty());
        assert_eq!(outdated, ["kept"]);
        let (added, deleted, _) = update_report(Some(&old), &new, &installed, false);
        assert!(added.is_empty());
        assert_eq!(deleted, ["gone"]);
        let mut auto = row("kept", "2");
        auto.auto_updates = true;
        let (_, _, outdated) = update_report(None, &[auto], &installed, true);
        assert!(outdated.is_empty());
    }

    #[test]
    fn list_files_excludes_directories_and_ds_store() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().join("keg");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("README"), b"r").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(root.join("bin/tool"), b"t").unwrap();
        std::fs::write(root.join("bin/.DS_Store"), b"junk").unwrap();
        #[cfg(unix)]
        super::super::prefix::symlink_file(&root.join("bin"), &root.join("binlink")).unwrap();
        let mut got: Vec<String> = list_files(&root)
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        got.sort();
        #[cfg(unix)]
        assert_eq!(got, ["README", "bin/tool", "binlink"]);
        #[cfg(not(unix))]
        assert_eq!(
            got,
            [
                "README".to_owned(),
                std::path::Path::new("bin")
                    .join("tool")
                    .display()
                    .to_string()
            ]
        );
    }

    #[test]
    fn artifact_lines_use_brew_summarize_and_english_names() {
        use super::super::formula::Artifact;
        assert_eq!(
            artifact_line(&Artifact::App("AltTab.app".into(), None)).as_deref(),
            Some("AltTab.app (App)")
        );
        assert_eq!(
            artifact_line(&Artifact::App("Foo.app".into(), Some("Bar.app".into()))).as_deref(),
            Some("Foo.app -> Bar.app (App)")
        );
        assert_eq!(
            artifact_line(&Artifact::Suite("SuiteDir".into(), None)).as_deref(),
            Some("SuiteDir (App Suite)")
        );
        assert_eq!(
            artifact_line(&Artifact::PrefPane("X.prefPane".into(), None)).as_deref(),
            Some("X.prefPane (Preference Pane)")
        );
        assert_eq!(
            artifact_line(&Artifact::Generic("f".into(), Some("/Library/X".into()))).as_deref(),
            Some("f -> /Library/X (Generic Artifact)")
        );
        assert_eq!(
            artifact_line(&Artifact::Wrapper(
                "demo".into(),
                "exe".into(),
                vec![],
                None
            ))
            .as_deref(),
            Some(".homebrew-command-wrappers/demo -> demo (Command Wrapper)")
        );
        assert_eq!(
            artifact_line(&Artifact::Library(
                "P.qlgenerator".into(),
                None,
                "Library/QuickLook".into()
            ))
            .as_deref(),
            Some("P.qlgenerator (Quick Look Plugin)")
        );
        assert_eq!(artifact_line(&Artifact::Other("zap".into())), None);
    }

    #[test]
    fn shell_kind_follows_brew_families() {
        assert_eq!(shell_kind("fish"), ShellKind::Fish);
        assert_eq!(shell_kind("-fish"), ShellKind::Fish);
        assert_eq!(shell_kind("/usr/local/bin/fish"), ShellKind::Fish);
        assert_eq!(shell_kind("tcsh"), ShellKind::Csh);
        assert_eq!(shell_kind("-csh"), ShellKind::Csh);
        assert_eq!(shell_kind("pwsh"), ShellKind::Pwsh);
        assert_eq!(shell_kind("pwsh-preview"), ShellKind::Pwsh);
        assert_eq!(shell_kind("powershell"), ShellKind::Pwsh);
        assert_eq!(shell_kind("zsh"), ShellKind::Posix);
        assert_eq!(shell_kind("bash"), ShellKind::Posix);
        assert_eq!(shell_kind("nushell"), ShellKind::Posix);
        assert_eq!(shell_kind(""), ShellKind::Posix);
    }

    #[cfg(unix)]
    #[test]
    fn shellenv_keeps_prefix_bytes_literal_when_evaluated() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("unexpected");
        let root = temp.path().join(format!(
            "quote'\"$(touch {})`touch {}` path",
            sentinel.display(),
            sentinel.display()
        ));
        let prefix = Prefix::at(&root);
        let script = format!("{}\nprintf '%s' \"$ABREW_PREFIX\"", shellenv(&prefix));
        let output = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, root.as_os_str().as_encoded_bytes());
        assert!(!sentinel.exists());
    }

    #[test]
    fn deletion_requires_complete_dependency_and_request_inputs() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        for (name, on_request) in [("app", "true"), ("dep", "false"), ("orphan", "false")] {
            let keg = p.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            std::fs::write(keg.join("payload"), name).unwrap();
            p.write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", "1"),
                    ("installed_on_request", on_request),
                ],
            )
            .unwrap();
        }
        let read = |name: &str| {
            let deps = if name == "app" { vec!["dep"] } else { vec![] };
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": deps}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert_eq!(autoremove_targets(&p, read).unwrap(), vec!["orphan"]);
        let incomplete = |name: &str| {
            if name == "app" {
                Err("captured unknown".into())
            } else {
                read(name)
            }
        };
        assert!(autoremove_targets(&p, incomplete).is_err());
        for value in [
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": "dep"}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": [1]}),
            aterm_json::json!({"name": "wrong", "versions": {"stable": "1"}, "revision": 0, "dependencies": []}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": ["../dep"]}),
            aterm_json::json!({"name": "app", "versions": {"stable": ""}, "revision": 0, "dependencies": []}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": "unknown", "dependencies": []}),
        ] {
            assert!(
                super::super::api::dependency_formula(
                    &value,
                    "app",
                    super::super::machine::Machine::this()
                )
                .is_err()
            );
        }
        let installed = p.installed_checked(Kind::Formula).unwrap();
        let revised = |name: &str| {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 2, "dependencies": []}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert!(
            install::dependency_books(&installed, revised).is_err(),
            "unqualified base does not prove revision2"
        );
        let mut qualified = installed.clone();
        for i in &mut qualified {
            i.version = "1_2".into();
            i.versions = vec!["1_2".into()];
        }
        assert!(
            install::dependency_books(&qualified, revised).is_ok(),
            "actual revision suffix is matched"
        );
        let receipt = p.keg("app", "1").join("INSTALL_RECEIPT.json");
        let saved = std::fs::read(&receipt).unwrap();
        std::fs::remove_file(&receipt).unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(
            &receipt,
            br#"{"installed_by":"abrew","name":"app","version":"1","installed_on_request":null}"#,
        )
        .unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(&receipt, vec![b' '; MAX_REQUEST_RECEIPT_BYTES + 1]).unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(&receipt, saved).unwrap();
        let old = p.keg("orphan", "0");
        std::fs::create_dir_all(&old).unwrap();
        p.write_receipt(
            &old,
            &[
                ("installed_by", "abrew"),
                ("name", "orphan"),
                ("version", "0"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
        let older = p
            .installed_checked(Kind::Formula)
            .unwrap()
            .into_iter()
            .find(|i| i.name == "orphan")
            .unwrap();
        assert!(requested(&p, &older).unwrap(), "old request is retained");
        assert!(
            autoremove_targets(&p, read).is_err(),
            "latest metadata cannot cover the older version"
        );
        std::fs::remove_dir_all(&old).unwrap();
        let missing = |name: &str| {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": ["absent"]}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert!(autoremove_targets(&p, missing).is_err());
        #[cfg(unix)]
        {
            p.pin("orphan").unwrap();
            assert!(autoremove_targets(&p, read).unwrap().is_empty());
            std::fs::remove_file(p.root.join("Pinned/orphan")).unwrap();
            std::fs::remove_dir(p.root.join("Pinned")).unwrap();
            std::fs::write(p.root.join("Pinned"), b"not a directory").unwrap();
            assert!(autoremove_targets(&p, read).is_err());
            std::fs::remove_file(p.root.join("Pinned")).unwrap();
            let pins_outside = tmp.path().join("pins-outside");
            std::fs::create_dir(&pins_outside).unwrap();
            std::os::unix::fs::symlink(&pins_outside, p.root.join("Pinned")).unwrap();
            assert!(autoremove_targets(&p, read).is_err());
            assert!(std::fs::read_dir(&pins_outside).unwrap().next().is_none());
            std::fs::remove_file(p.root.join("Pinned")).unwrap();
            let outside = tmp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("marker"), b"retain outside").unwrap();
            std::os::unix::fs::symlink(&outside, p.cellar().join("alias")).unwrap();
            assert!(p.installed_checked(Kind::Formula).is_err());
            assert_eq!(
                p.installed(Kind::Formula).len(),
                3,
                "legacy read-only best effort"
            );
            assert!(autoremove_targets(&p, read).is_err());
            assert_eq!(
                std::fs::read(outside.join("marker")).unwrap(),
                b"retain outside"
            );
            std::fs::remove_file(p.cellar().join("alias")).unwrap();
        }
        assert_eq!(autoremove_targets(&p, read).unwrap(), vec!["orphan"]);
        for name in ["adep", "zapp"] {
            let keg = p.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            std::fs::write(keg.join("payload"), name).unwrap();
            p.write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", "1"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        }
        let chain = |name: &str| {
            if name == "zapp" {
                super::super::api::dependency_formula(
                    &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["adep"]}),
                    name,
                    super::super::machine::Machine::this(),
                )
            } else {
                read(name)
            }
        };
        assert_eq!(
            autoremove_targets(&p, chain).unwrap(),
            ["orphan", "zapp", "adep"]
        );
        let cycle = |name: &str| {
            if name == "adep" {
                super::super::api::dependency_formula(
                    &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["zapp"]}),
                    name,
                    super::super::machine::Machine::this(),
                )
            } else {
                chain(name)
            }
        };
        assert!(autoremove_targets(&p, cycle).is_err());
        let unknown = p.keg("notes", "1");
        std::fs::create_dir_all(&unknown).unwrap();
        std::fs::write(unknown.join("owner"), b"keep").unwrap();
        assert!(autoremove_targets(&p, chain).is_err());
        assert_eq!(std::fs::read(unknown.join("owner")).unwrap(), b"keep");
        for name in ["app", "dep", "orphan", "adep", "zapp"] {
            assert_eq!(
                std::fs::read(p.keg(name, "1").join("payload")).unwrap(),
                name.as_bytes()
            );
        }
    }

    #[test]
    fn caveats_helpers_match_brew_text() {
        // KegOnlyReason#to_s: explanation wins; stock symbol texts
        // are brew's verbatim (wrapped) wordings.
        assert_eq!(
            keg_only_reason_text(
                ":shadowed_by_macos",
                "macOS provides BLAS in Accelerate.framework"
            ),
            "macOS provides BLAS in Accelerate.framework"
        );
        assert_eq!(
            keg_only_reason_text(":provided_by_macos", ""),
            "macOS already provides this software and installing another version in\nparallel can cause all kinds of trouble"
        );
        assert_eq!(
            keg_only_reason_text(":versioned_formula", ""),
            "this is an alternate version of another formula"
        );
        assert_eq!(
            keg_only_reason_text("it conflicts with x", ""),
            "it conflicts with x"
        );
        // Shellwords.escape via Utils::Shell.sh_quote.
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("/opt/x/bin"), "/opt/x/bin");
        assert_eq!(sh_quote("~/.zshrc"), "~/.zshrc");
        assert_eq!(sh_quote("a b=c$d"), "a\\ b\\=c\\$d");
        // export_value per shell.
        assert_eq!(
            export_value(Some("zsh"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
            Some("export LDFLAGS=-L/opt/x/lib")
        );
        assert_eq!(
            export_value(Some("fish"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
            Some("set -gx LDFLAGS -L/opt/x/lib")
        );
        assert_eq!(
            export_value(Some("tcsh"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
            Some("setenv LDFLAGS -L/opt/x/lib;")
        );
        assert_eq!(export_value(None, "LDFLAGS", "-L/opt/x/lib"), None);
        assert_eq!(
            prepend_path_in_profile(Some("fish"), "/opt/x/bin").as_deref(),
            Some("fish_add_path /opt/x/bin")
        );
        let zsh = prepend_path_in_profile(Some("zsh"), "/opt/x/bin").unwrap();
        assert!(
            zsh.starts_with("printf '%s\\n' export\\ PATH\\=/opt/x/bin:\\$PATH >> "),
            "{zsh}"
        );
        // Service#manual_command: env (minus PATH), placeholder
        // expansion, shell-quoted argv.
        let service = super::super::formula::ServiceInfo {
            run: vec![
                "$HOMEBREW_PREFIX/opt/apfel/bin/apfel".to_string(),
                "--serve".to_string(),
            ],
            require_root: false,
            environment_variables: vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("FOO".to_string(), "$HOMEBREW_PREFIX/var".to_string()),
            ],
        };
        assert_eq!(
            service_manual_command("/p", &service),
            "FOO=\"/p/var\" /p/opt/apfel/bin/apfel --serve"
        );
    }

    #[test]
    fn installed_section_includes_sibling_kegs() {
        let dir = crate::testdir::dir("abrew-info-siblings-");
        let p = Prefix::at(&dir.join("prefix"));
        p.ensure().unwrap();
        owned_keg(&p, "demo", "1.0");
        owned_keg(&p, "demo", "2.0");
        owned_keg(&p, "demo@1", "1.5");
        owned_keg(&p, "unrelated", "9.9");
        let doc = aterm_json::from_str::<aterm_json::Value>(
            r#"{"name":"demo","versions":{"stable":"2.0"}}"#,
        )
        .unwrap();
        let f = super::super::formula::Formula::from_api(&doc).unwrap();
        let inst = p.installed_one(Kind::Formula, "demo").unwrap();
        let api = Api::read_only(p.clone());
        // Non-verbose: newest keg per related formula, newest first.
        let rows = installed_section_rows(&p, &api, &f, Some(&inst), false);
        let names: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.full_name.as_str(), r.shown.as_str()))
            .collect();
        assert_eq!(names, [("demo", "2.0"), ("demo@1", "1.5")]);
        // Verbose: every keg.
        let rows = installed_section_rows(&p, &api, &f, Some(&inst), true);
        let names: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.full_name.as_str(), r.shown.as_str()))
            .collect();
        assert_eq!(names, [("demo", "2.0"), ("demo", "1.0"), ("demo@1", "1.5")]);
        // Not installed itself: the sibling's kegs still show.
        let rows = installed_section_rows(&p, &api, &f, None, false);
        let names: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.full_name.as_str(), r.shown.as_str()))
            .collect();
        assert_eq!(names, [("demo@1", "1.5")]);
        // A higher receipt version_scheme outranks a higher version
        // (brew's openssl@3 [scheme 1] above openssl@4 [scheme 0]).
        p.write_receipt(
            &p.keg("demo@1", "1.5"),
            &[
                ("installed_by", "abrew"),
                ("name", "demo@1"),
                ("version", "1.5"),
                ("version_scheme", "2"),
            ],
        )
        .unwrap();
        let rows = installed_section_rows(&p, &api, &f, Some(&inst), false);
        let names: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.full_name.as_str(), r.shown.as_str()))
            .collect();
        assert_eq!(names, [("demo@1", "1.5"), ("demo", "2.0")]);
    }

    #[test]
    fn caffiene_alias_works_for_list_outdated_and_uninstall() {
        let dir = crate::testdir::dir("abrew-caffiene-lifecycle-");
        let p = Prefix::at(&dir.join("prefix"));
        p.ensure().unwrap();
        let cask_dir = p.cask_dir("caffeine", "1.1.3");
        std::fs::create_dir_all(&cask_dir).unwrap();
        std::fs::write(cask_dir.join("Caffeine.app"), b"stub").unwrap();
        p.write_receipt(
            &cask_dir,
            &[
                ("installed_by", "abrew"),
                ("token", "caffeine"),
                ("version", "1.1.3"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
        p.rebuild_state().unwrap();

        let api = Api::read_only(p.clone());
        assert_eq!(
            cmd_list(&p, &api, &[String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            cmd_list(
                &p,
                &api,
                &[String::from("--versions"), String::from("caffiene")]
            ),
            ExitCode::SUCCESS
        );
        assert!(!package_definitively_unknown(
            &p, &api, "caffiene", false, true
        ));
        assert_eq!(
            cmd_unpin(&p, &api, &[String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            bundle_file(
                &p,
                &api,
                &[String::from("bundle"), String::from("check")],
                &[bundle::BrewfileEntry::Cask(String::from("caffiene"))],
                "check"
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            cmd_cleanup(&p, &api, &[String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            cmd_uninstall(
                &p,
                &api,
                &[String::from("uninstall"), String::from("caffiene")],
                &[String::from("caffiene")]
            ),
            ExitCode::SUCCESS
        );
        assert!(p.installed_one(Kind::Cask, "caffeine").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pinned_metadata_line_reads_cask_pin_timestamp_from_pinned_casks() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let cask_dir = p.cask_dir("demo", "2.4");
        std::fs::create_dir_all(&cask_dir).unwrap();
        p.write_receipt(
            &cask_dir,
            &[
                ("installed_by", "abrew"),
                ("token", "demo"),
                ("version", "2.4"),
            ],
        )
        .unwrap();
        p.pin_cask("demo").unwrap();
        let line = pinned_metadata_line(&p, "demo", true).expect("pinned cask metadata");
        assert!(line.starts_with("Pinned: 2.4 on "), "{line}");
        assert!(pinned_metadata_line(&p, "demo", false).is_none());
    }
}

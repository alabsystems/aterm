// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native `brew trust`/`untrust`: brew's global trust store
//! (`Homebrew::Trust`) over `${user config home}/trust.json`.

use std::path::{Path, PathBuf};

use super::prefix::Prefix;

/// One trust entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustType {
    Tap,
    Formula,
    Cask,
    Command,
}

impl TrustType {
    pub fn singular(self) -> &'static str {
        match self {
            TrustType::Tap => "tap",
            TrustType::Formula => "formula",
            TrustType::Cask => "cask",
            TrustType::Command => "command",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            TrustType::Tap => "taps",
            TrustType::Formula => "formulae",
            TrustType::Cask => "casks",
            TrustType::Command => "commands",
        }
    }

    fn setting_key(self) -> &'static str {
        match self {
            TrustType::Tap => "trustedtaps",
            TrustType::Formula => "trustedformulae",
            TrustType::Cask => "trustedcasks",
            TrustType::Command => "trustedcommands",
        }
    }

    pub const ALL: [TrustType; 4] = [
        TrustType::Tap,
        TrustType::Formula,
        TrustType::Cask,
        TrustType::Command,
    ];
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var_os(name)
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|v| !v.is_empty())
}

/// The trust-store file (`bin/brew`'s user-config-home rule,
/// which recomputes `HOMEBREW_USER_CONFIG_HOME`: XDG first).
pub fn trust_file_path() -> Option<PathBuf> {
    if let Some(xdg) = env_nonempty("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("homebrew/trust.json"));
    }
    if let Some(xdg) = env_nonempty("HOMEBREW_XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("homebrew/trust.json"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join(".homebrew/trust.json"))
}

type StorePairs = Vec<(String, aterm_json::Value)>;

/// Top-level object keys of a JSON document, in file order
/// (the store preserves insertion order like Ruby hashes).
fn top_level_keys(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    let mut was_string_at_depth1 = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                current.push(c);
                escaped = false;
            } else if c == '\\' {
                current.push(c);
                escaped = true;
            } else if c == '"' {
                in_string = false;
                was_string_at_depth1 = depth == 1;
            } else {
                current.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                current.clear();
            }
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ':' if depth == 1 && was_string_at_depth1 => {
                if let Ok(key) =
                    aterm_json::from_slice::<aterm_json::Value>(format!("\"{current}\"").as_bytes())
                    && let Some(key) = key.as_str()
                {
                    keys.push(key.to_owned());
                }
                was_string_at_depth1 = false;
            }
            _ => {}
        }
    }
    keys
}

fn read_store_ordered() -> StorePairs {
    let Some(path) = trust_file_path() else {
        return Vec::new();
    };
    let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, 1024 * 1024) else {
        return Vec::new();
    };
    let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut pairs: StorePairs = Vec::new();
    for key in top_level_keys(&bytes) {
        if let Some(v) = map.get(&key)
            && !pairs.iter().any(|(k, _)| *k == key)
        {
            pairs.push((key, v.clone()));
        }
    }
    for (key, v) in map {
        if !pairs.iter().any(|(k, _)| k == key) {
            pairs.push((key.clone(), v.clone()));
        }
    }
    pairs
}

pub fn read_store() -> aterm_json::Value {
    aterm_json::Value::Object(read_store_ordered().into_iter().collect())
}

/// brew `Trust.trusted_entries`.
pub fn trusted_entries(kind: TrustType) -> Vec<String> {
    let store = read_store();
    store
        .get(kind.setting_key())
        .and_then(aterm_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(aterm_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn pretty_json(value: &aterm_json::Value, indent: usize, out: &mut String) {
    match value {
        aterm_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{\n\n");
                out.push_str(&" ".repeat(indent));
                out.push('}');
                return;
            }
            out.push_str("{\n");
            let inner = indent + 2;
            for (i, (k, v)) in map.iter().enumerate() {
                out.push_str(&" ".repeat(inner));
                out.push_str(
                    &aterm_json::to_string(&aterm_json::Value::String(k.clone()))
                        .unwrap_or_default(),
                );
                out.push_str(": ");
                pretty_json(v, inner, out);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
        aterm_json::Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[\n\n");
                out.push_str(&" ".repeat(indent));
                out.push(']');
                return;
            }
            out.push_str("[\n");
            let inner = indent + 2;
            for (i, v) in items.iter().enumerate() {
                out.push_str(&" ".repeat(inner));
                pretty_json(v, inner, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push(']');
        }
        other => out.push_str(&aterm_json::to_string(other).unwrap_or_default()),
    }
}

fn pretty_pairs(pairs: &StorePairs) -> String {
    if pairs.is_empty() {
        return "{\n\n}".to_owned();
    }
    let mut out = String::from("{\n");
    for (i, (key, value)) in pairs.iter().enumerate() {
        out.push_str("  ");
        out.push_str(
            &aterm_json::to_string(&aterm_json::Value::String(key.clone())).unwrap_or_default(),
        );
        out.push_str(": ");
        pretty_json(value, 2, &mut out);
        if i + 1 < pairs.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push('}');
    out
}

/// Ruby `JSON.pretty_generate` for store-shaped values.
pub fn pretty_store(value: &aterm_json::Value) -> String {
    let mut out = String::new();
    pretty_json(value, 0, &mut out);
    out
}

#[cfg(unix)]
fn group_world_writable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o022 != 0
}

#[cfg(not(unix))]
fn group_world_writable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// brew's `write_trust_store` guards + file write, under the
/// store lock. An empty store removes the file.
fn write_store(pairs: &StorePairs) -> Result<(), String> {
    let path =
        trust_file_path().ok_or_else(|| "abrew: cannot resolve the trust store path".to_owned())?;
    let dir = path
        .parent()
        .ok_or_else(|| "abrew: cannot resolve the trust store path".to_owned())?
        .to_path_buf();
    let insecure = |what: &str| {
        format!(
            "Refusing to write insecure trust store: {what} {}",
            path.display()
        )
    };
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(insecure("symlink directory"));
            }
            if group_world_writable(&meta) {
                return Err(insecure("group or world writable directory"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{
                    DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _,
                };
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&dir)
                    .map_err(|e| format!("abrew: cannot create {}: {e}", dir.display()))?;
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&dir)
                    .and_then(|f| f.set_permissions(std::fs::Permissions::from_mode(0o700)))
                    .map_err(|e| format!("abrew: cannot chmod {}: {e}", dir.display()))?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("abrew: cannot create {}: {e}", dir.display()))?;
        }
        Err(e) => return Err(format!("abrew: cannot inspect {}: {e}", dir.display())),
    }
    let empty = pairs
        .iter()
        .all(|(_, v)| v.as_array().is_some_and(Vec::is_empty));
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(insecure("symlink"));
            }
            if !meta.is_file() {
                return Err(insecure("not a regular file"));
            }
            if group_world_writable(&meta) {
                return Err(insecure("group or world writable"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("abrew: cannot inspect {}: {e}", path.display())),
    }
    if empty {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("abrew: cannot remove {}: {e}", path.display()))?;
        }
        return Ok(());
    }
    let text = format!("{}\n", pretty_pairs(pairs));
    let tmp = dir.join(format!(".trust.json.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let wrote = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(text.as_bytes())
    })();
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("abrew: cannot write {}: {e}", tmp.display()));
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("abrew: cannot write {}: {e}", path.display())
    })
}

/// Run `f` under the trust store's exclusive-creation lease, removed after the write ends.
fn with_store_lock<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let path =
        trust_file_path().ok_or_else(|| "abrew: cannot resolve the trust store path".to_owned())?;
    if let Some(dir) = path.parent()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("abrew: cannot create {}: {e}", dir.display()))?;
    }
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock_file = acquire_store_lock(
        &lock_path,
        std::time::Instant::now(),
        crate::platform::open_create_new_write,
        std::thread::sleep,
    )?;
    let result = f();
    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);
    result
}

const STORE_LOCK_HANG: std::time::Duration = std::time::Duration::from_secs(60);

fn acquire_store_lock(
    path: &Path,
    started: std::time::Instant,
    mut create: impl FnMut(&Path, u32) -> std::io::Result<std::fs::File>,
    mut pause: impl FnMut(std::time::Duration),
) -> Result<std::fs::File, String> {
    loop {
        match create(path, 0o600) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if started.elapsed() >= STORE_LOCK_HANG {
                    return Err("abrew: cannot lock the trust store".to_owned());
                }
                pause(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(format!("abrew: cannot lock the trust store: {error}")),
        }
    }
}

fn store_entries_mut(pairs: &mut StorePairs, kind: TrustType) -> &mut Vec<aterm_json::Value> {
    let key = kind.setting_key();
    if !pairs.iter().any(|(k, _)| k == key) {
        pairs.push((key.to_owned(), aterm_json::Value::Array(Vec::new())));
    }
    let entry = &mut pairs
        .iter_mut()
        .find(|(k, _)| k == key)
        .expect("just inserted")
        .1;
    if !matches!(entry, aterm_json::Value::Array(_)) {
        *entry = aterm_json::Value::Array(Vec::new());
    }
    match entry {
        aterm_json::Value::Array(items) => items,
        _ => unreachable!(),
    }
}

/// brew `Trust.trust!`: true when newly added.
pub fn trust_add(kind: TrustType, name: &str) -> Result<bool, String> {
    let name = name.to_lowercase();
    with_store_lock(|| {
        let mut store = read_store_ordered();
        let items = store_entries_mut(&mut store, kind);
        if items.iter().any(|v| v.as_str() == Some(name.as_str())) {
            return Ok(false);
        }
        items.push(aterm_json::Value::String(name.clone()));
        items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        write_store(&store)?;
        Ok(true)
    })
}

/// brew `Trust.untrust!`: true when anything was removed.
pub fn trust_remove(prefix: &Prefix, kind: TrustType, name: &str) -> Result<bool, String> {
    let name = name.to_lowercase();
    let mut to_delete = vec![name.clone()];
    if kind != TrustType::Tap
        && let Some((tap, item)) = split_full_name(&name)
        && let Ok(tap) = TapRef::fetch(prefix, &tap)
        && tap.uses_custom_remote()
    {
        to_delete.push(item_trust_name(kind, &tap, &item, false, None));
    }
    with_store_lock(|| {
        let mut store = read_store_ordered();
        let mut removed = false;
        {
            let items = store_entries_mut(&mut store, kind);
            for entry in &to_delete {
                let before = items.len();
                items.retain(|v| v.as_str() != Some(entry.as_str()));
                removed |= items.len() != before;
            }
            items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
        if !removed {
            return Ok(false);
        }
        store
            .retain(|(k, v)| !(k == kind.setting_key() && v.as_array().is_some_and(Vec::is_empty)));
        write_store(&store)?;
        Ok(true)
    })
}

/// brew `Trust.replace!`.
pub fn trust_replace(entries: &[(TrustType, String)]) -> Result<(), String> {
    with_store_lock(|| {
        let mut store: StorePairs = Vec::new();
        for (kind, name) in entries {
            store_entries_mut(&mut store, *kind)
                .push(aterm_json::Value::String(name.to_lowercase()));
        }
        for (_, items) in &mut store {
            if let aterm_json::Value::Array(items) = items {
                items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                items.dedup_by(|a, b| a.as_str() == b.as_str());
            }
        }
        write_store(&store)
    })
}

/// A tap for trust purposes (brew `Tap.fetch`).
pub struct TapRef {
    user: String,
    repo: String,
    path: Option<PathBuf>,
    remote: Option<String>,
}

fn valid_tap_component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".."
}

/// brew `Tap.normalize_remote`.
pub fn normalize_remote(remote: &str) -> String {
    let mut r = remote.trim().to_lowercase();
    if let Some(path) = github_remote_path(&r) {
        r = format!("https://github.com/{path}");
    }
    if let Some(host) = remote_host(&r)
        && ["codeberg.org", "github.com", "gitlab.com"].contains(&host.as_str())
    {
        let trimmed = r.trim_end_matches('/');
        r = trimmed.strip_suffix(".git").unwrap_or(trimmed).to_owned();
    }
    r
}

/// The path after a GitHub remote prefix (`<scheme>://[user@]github.com/`
/// or SCP-style `[user@]github.com:`), when present.
fn github_remote_path(remote: &str) -> Option<String> {
    if let Some(idx) = remote.find("://") {
        let scheme = &remote[..idx];
        if scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            || !scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return None;
        }
        let rest = &remote[idx + 3..];
        let rest = match rest.find('@') {
            Some(at) if !rest[..at].contains('/') => &rest[at + 1..],
            _ => rest,
        };
        return rest.strip_prefix("github.com/").map(str::to_owned);
    }
    let rest = match remote.find('@') {
        Some(at) if !remote[..at].contains('/') => &remote[at + 1..],
        _ => remote,
    };
    rest.strip_prefix("github.com:").map(str::to_owned)
}

/// The host segment of a remote (brew `REMOTE_HOST_REGEX`).
fn remote_host(remote: &str) -> Option<String> {
    let mut rest = remote;
    if let Some(idx) = rest.find("://") {
        let scheme = &rest[..idx];
        if !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            rest = &rest[idx + 3..];
        }
    }
    if let Some(at) = rest.find('@')
        && !rest[..at].contains('/')
    {
        rest = &rest[at + 1..];
    }
    let host: String = rest
        .chars()
        .take_while(|c| *c != '/' && *c != ':')
        .collect();
    if host.is_empty() { None } else { Some(host) }
}

/// brew `Tap.same_remote?`.
pub fn same_remote(left: &str, right: &str) -> bool {
    let left = normalize_remote(left);
    !left.is_empty() && left == normalize_remote(right)
}

/// brew `Tap.remote_reference?`.
pub fn remote_reference(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with('.')
        || s.starts_with('~')
        || s.find(':').is_some_and(|i| !s[..i].contains('/'))
}

/// brew `Tap.remote_to_reference`: "user/repo" when the
/// remote is the tap's default, else the normalised remote.
pub fn remote_to_reference(prefix: &Prefix, remote: &str) -> Option<String> {
    let normalised = normalize_remote(remote);
    let tail: Vec<&str> = normalised
        .rsplit(['/', ':'])
        .filter(|s| !s.is_empty())
        .take(2)
        .collect();
    if tail.len() == 2 {
        let rr = format!("{}/{}", tail[1], tail[0]);
        if let Ok(tap) = TapRef::fetch(prefix, &rr)
            && same_remote(&normalised, &tap.default_remote())
        {
            return Some(tap.name());
        }
    }
    Some(normalised)
}

/// Split `user/repo/name` into tap and item parts.
fn split_full_name(name: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = name.splitn(3, '/').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((format!("{}/{}", parts[0], parts[1]), parts[2].to_owned()))
}

impl TapRef {
    /// brew `Tap.fetch(name)` (UsageError text on bad names).
    pub fn fetch(prefix: &Prefix, name: &str) -> Result<TapRef, String> {
        let Some((user, repo)) = name.split_once('/') else {
            return Err(format!("Invalid tap name: '{name}'"));
        };
        if user.contains('/') || !valid_tap_component(user) || !valid_tap_component(repo) {
            return Err(format!("Invalid tap name: '{name}'"));
        }
        let user = if user == "homebrew" {
            "Homebrew"
        } else if user == "linuxbrew" {
            "Linuxbrew"
        } else {
            user
        }
        .to_owned();
        let repo = repo
            .strip_prefix("homebrew-")
            .or_else(|| repo.strip_prefix("linuxbrew-"))
            .unwrap_or(repo)
            .to_owned();
        if repo.is_empty() {
            return Err(format!("Invalid tap name: '{name}'"));
        }
        Ok(Self::from_parts(prefix, user, repo))
    }

    fn from_parts(prefix: &Prefix, user: String, repo: String) -> Self {
        // brew capitalises the official users in Tap.fetch, and
        // every Tap (path-derived ones too) carries that case.
        let user = match user.as_str() {
            "homebrew" => "Homebrew".to_owned(),
            "linuxbrew" => "Linuxbrew".to_owned(),
            _ => user,
        };
        let installed = super::tap::list(prefix)
            .into_iter()
            .find(|t| t.user.eq_ignore_ascii_case(&user) && t.repo.eq_ignore_ascii_case(&repo));
        let (path, remote) = match installed {
            Some(tap) => {
                let remote = super::tap::remote_url(&tap);
                (Some(tap.path.clone()), remote)
            }
            None => (None, None),
        };
        TapRef {
            user,
            repo,
            path,
            remote,
        }
    }

    pub fn from_tap(prefix: &Prefix, tap: &super::tap::Tap) -> Self {
        Self::from_parts(prefix, tap.user.clone(), tap.repo.clone())
    }

    /// brew `Tap#name`: downcased `user/repo`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.user, self.repo).to_lowercase()
    }

    /// brew `Tap#full_name`.
    fn full_name(&self) -> String {
        let prefix_word = if self.user == "Linuxbrew" {
            "linuxbrew"
        } else {
            "homebrew"
        };
        format!("{}/{prefix_word}-{}", self.user, self.repo)
    }

    pub fn official(&self) -> bool {
        self.user == "Homebrew"
    }

    fn default_remote(&self) -> String {
        format!("https://github.com/{}", self.full_name())
    }

    /// brew `Tap#reference`.
    fn reference(&self, remote_override: Option<&str>) -> String {
        let remote = remote_override.or(self.remote.as_deref());
        match remote {
            Some(r) if !same_remote(r, &self.default_remote()) => r.to_owned(),
            _ => self.name(),
        }
    }

    fn uses_custom_remote(&self) -> bool {
        self.remote
            .as_deref()
            .is_some_and(|r| !same_remote(r, &self.default_remote()))
    }

    fn canonical_remote(&self) -> bool {
        match self.remote.as_deref() {
            None => true,
            Some(r) => r.trim().is_empty() || same_remote(r, &self.default_remote()),
        }
    }

    pub fn implicitly_trusted(&self) -> bool {
        self.official() && self.canonical_remote()
    }

    fn matches_reference(&self, reference: &str, remote: Option<&str>) -> bool {
        let remote = remote.or(self.remote.as_deref());
        if remote_reference(reference) {
            return remote.is_some_and(|r| same_remote(reference, r));
        }
        let custom = remote.is_some_and(|r| !same_remote(r, &self.default_remote()));
        !custom && self.name() == reference.to_lowercase()
    }

    fn explicitly_trusted(&self) -> bool {
        trusted_entries(TrustType::Tap).iter().any(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return false;
            }
            let normalised = if remote_reference(entry) {
                normalize_remote(entry)
            } else {
                entry.to_lowercase()
            };
            !normalised.is_empty() && self.matches_reference(&normalised, self.remote.as_deref())
        })
    }

    pub fn trusted(&self) -> bool {
        self.implicitly_trusted() || self.explicitly_trusted()
    }

    fn formula_dir(&self) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        for candidate in ["Formula", "HomebrewFormula"] {
            let dir = path.join(candidate);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        Some(path.clone())
    }

    fn rb_basenames(dir: &Path, recursive: bool) -> Vec<String> {
        fn walk(dir: &Path, recursive: bool, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let ft = entry.file_type().ok();
                let is_dir = ft
                    .map(|ft| ft.is_dir() || (ft.is_symlink() && entry.path().is_dir()))
                    .unwrap_or(false);
                if recursive && is_dir {
                    walk(&entry.path(), recursive, out);
                } else {
                    let name = entry.file_name();
                    let name_path = Path::new(&name);
                    if name_path.extension().is_some_and(|e| e == "rb")
                        && let Some(stem) = name_path.file_stem().and_then(|s| s.to_str())
                    {
                        out.push(stem.to_owned());
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, recursive, &mut out);
        out
    }

    pub fn formula_names(&self) -> Vec<String> {
        let Some(dir) = self.formula_dir() else {
            return Vec::new();
        };
        let recursive = self.path.as_ref().is_some_and(|p| dir != *p);
        Self::rb_basenames(&dir, recursive)
    }

    pub fn cask_tokens(&self) -> Vec<String> {
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let dir = path.join("Casks");
        if !dir.is_dir() {
            return Vec::new();
        }
        Self::rb_basenames(&dir, true)
    }

    pub fn command_names(&self) -> Vec<String> {
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let dir = path.join("cmd");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                name.to_str()?.strip_prefix("brew-").map(|cmd| {
                    cmd.strip_suffix(".rb")
                        .or_else(|| cmd.strip_suffix(".sh"))
                        .unwrap_or(cmd)
                        .to_owned()
                })
            })
            .collect()
    }

    /// brew `Tap#alias_table`: alias → canonical name.
    fn alias_table(&self, api: &super::api::Api) -> Vec<(String, String)> {
        if self.name() == "homebrew/core" {
            let mut out = Vec::new();
            if let Ok(rows) = api.search_index(false) {
                for row in rows {
                    for alias in &row.aliases {
                        out.push((alias.clone(), row.name.clone()));
                    }
                }
            }
            return out;
        }
        if self.name() == "homebrew/cask" {
            return Vec::new();
        }
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let aliases = path.join("Aliases");
        let Ok(entries) = std::fs::read_dir(&aliases) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let alias = entry.file_name().to_string_lossy().into_owned();
            let target = std::fs::canonicalize(&entry_path).unwrap_or(entry_path);
            let canonical = target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| alias.clone());
            // Non-core alias tables key on the full name.
            out.push((format!("{}/{alias}", self.name()), canonical));
        }
        out
    }

    fn renames(&self, api: &super::api::Api) -> Vec<(String, String)> {
        if self.name() == "homebrew/core" {
            let mut out = Vec::new();
            if let Ok(rows) = api.index_rows_fresh(false) {
                for row in rows {
                    for old in row.oldnames {
                        out.push((old, row.name.clone()));
                    }
                }
            }
            return out;
        }
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let Ok(bytes) = crate::metadata_io::read_bounded_regular(
            &path.join("formula_renames.json"),
            1024 * 1024,
        ) else {
            return Vec::new();
        };
        let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
            return Vec::new();
        };
        value
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// brew `Tap.with_formula_name`: the tap and downcased
/// token of a fully-qualified `user/repo/item` name.
pub fn with_formula_name(prefix: &Prefix, name: &str) -> Option<(TapRef, String)> {
    let (tap_name, item) = split_full_name(name)?;
    if !item
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.' | '@'))
    {
        return None;
    }
    let tap = TapRef::fetch(prefix, &tap_name).ok()?;
    Some((tap, item.to_lowercase()))
}

/// brew `Trust.tap_formula_name` canonicalization.
fn tap_formula_name(api: &super::api::Api, tap: &TapRef, name: &str) -> String {
    let name = name.to_lowercase();
    let full = format!("{}/{name}", tap.name());
    let key = if matches!(tap.name().as_str(), "homebrew/core" | "homebrew/cask") {
        name.clone()
    } else {
        full.clone()
    };
    if let Some((_, canonical)) = tap
        .alias_table(api)
        .into_iter()
        .find(|(alias, _)| *alias == key)
    {
        return canonical;
    }
    if let Some((_, canonical)) = tap.renames(api).into_iter().find(|(old, _)| *old == name) {
        return canonical;
    }
    name
}

/// brew `Trust.item_trust_name`.
fn item_trust_name(
    kind: TrustType,
    tap: &TapRef,
    item: &str,
    include_existing: bool,
    tap_remote: Option<&str>,
) -> String {
    let item = item.to_lowercase();
    let full_name = format!("{}/{item}", tap.name());
    if include_existing && trusted_entries(kind).contains(&full_name) {
        return full_name;
    }
    let reference = tap.reference(tap_remote);
    if reference == tap.name() {
        full_name
    } else {
        format!("{}/{item}", reference.to_lowercase())
    }
}

/// brew `Trust.fully_qualified_package_name`.
fn fully_qualified(prefix: &Prefix, name: &str, noun: &str) -> Result<(TapRef, String), String> {
    with_formula_name(prefix, name).ok_or_else(|| {
        format!("Error: Invalid usage: {noun} must be fully-qualified as <user>/<tap>/<name>.")
    })
}

/// brew `Trust.trust_name`.
fn trust_name(
    prefix: &Prefix,
    api: &super::api::Api,
    kind: TrustType,
    name: &str,
    include_existing: bool,
    tap_remote: Option<&str>,
) -> Result<String, String> {
    match kind {
        TrustType::Tap => {
            if remote_reference(name) {
                return remote_to_reference(prefix, name).ok_or_else(|| {
                    format!("Error: Invalid usage: Invalid tap remote URL: {name}")
                });
            }
            let tap =
                TapRef::fetch(prefix, name).map_err(|e| format!("Error: Invalid usage: {e}"))?;
            Ok(tap.reference(tap_remote).to_lowercase())
        }
        TrustType::Formula => {
            let (tap, formula_name) = fully_qualified(prefix, name, "Formulae")?;
            if include_existing {
                let existing = item_trust_name(kind, &tap, &formula_name, true, tap_remote);
                if trusted_entries(kind).contains(&existing) {
                    return Ok(existing);
                }
            }
            let mut formula_name = formula_name;
            if tap_remote.is_none()
                || tap_remote
                    .is_some_and(|r| tap.remote.as_deref().is_some_and(|now| same_remote(r, now)))
            {
                let canonical = tap_formula_name(api, &tap, &formula_name);
                if !include_existing && canonical != formula_name {
                    return Err(format!(
                        "Error: Invalid usage: {name} resolves to {}/{canonical}.\n\
                         Trust the canonical formula explicitly:\n  \
                         brew trust --formula {}/{canonical}",
                        tap.name(),
                        tap.name()
                    ));
                }
                formula_name = canonical;
            }
            Ok(item_trust_name(
                kind,
                &tap,
                &formula_name,
                include_existing,
                tap_remote,
            ))
        }
        TrustType::Cask => {
            let (tap, token) = fully_qualified(prefix, name, "Casks")?;
            Ok(item_trust_name(
                kind,
                &tap,
                &token,
                include_existing,
                tap_remote,
            ))
        }
        TrustType::Command => {
            let (tap, token) = fully_qualified(prefix, name, "Commands")?;
            Ok(item_trust_name(
                kind,
                &tap,
                &token,
                include_existing,
                tap_remote,
            ))
        }
    }
}

/// brew `Trust.infer_target` + candidate collection.
fn infer_target(
    prefix: &Prefix,
    api: &super::api::Api,
    name: &str,
    include_existing: bool,
) -> Result<(TrustType, String), String> {
    if name.matches('/').count() == 1 || remote_reference(name) {
        return Ok((
            TrustType::Tap,
            trust_name(prefix, api, TrustType::Tap, name, include_existing, None)?,
        ));
    }
    fetch_error_for_infer(name)?;
    let Some((tap, token)) = with_formula_name(prefix, name) else {
        return Err(
            "Error: Invalid usage: Trust targets must be fully-qualified tap, \
                    formula, cask or command names."
                .to_owned(),
        );
    };
    let mut candidates: Vec<TrustType> = Vec::new();
    let core_formula = tap.name() == "homebrew/core"
        && api
            .search_index(false)
            .is_ok_and(|rows| rows.iter().any(|r| r.name == token));
    let core_cask = tap.name() == "homebrew/cask"
        && api
            .search_index(true)
            .is_ok_and(|rows| rows.iter().any(|r| r.name == token));
    if core_formula
        || tap.formula_names().contains(&token)
        || (tap.name() != "homebrew/cask" && tap_formula_name(api, &tap, &token) != token)
    {
        candidates.push(TrustType::Formula);
    }
    if core_cask || tap.cask_tokens().contains(&token) {
        candidates.push(TrustType::Cask);
    }
    if tap.command_names().contains(&token) {
        candidates.push(TrustType::Command);
    }
    if include_existing {
        let full_name = format!("{}/{token}", tap.name());
        for kind in [TrustType::Formula, TrustType::Cask, TrustType::Command] {
            if !candidates.contains(&kind) && trusted(prefix, kind, &full_name) {
                candidates.push(kind);
            }
        }
    }
    let mut resolved: Vec<(TrustType, String)> = Vec::new();
    for kind in candidates {
        resolved.push((
            kind,
            trust_name(prefix, api, kind, name, include_existing, None)?,
        ));
    }
    match resolved.len() {
        0 => Err(format!(
            "Error: Invalid usage: No formula, cask or command found for {name}."
        )),
        1 => Ok(resolved.into_iter().next().expect("one candidate")),
        _ => Err(format!(
            "Error: Invalid usage: Ambiguous trust target {name}. \
             Use `--formula`, `--cask` or `--command`."
        )),
    }
}

fn fetch_error_for_infer(name: &str) -> Result<(), String> {
    // brew pre-validates the tap in infer_target so invalid
    // names take the tap error, not the generic one.
    if remote_reference(name) {
        return Ok(());
    }
    if let Some((tap_name, _)) = split_full_name(name) {
        validate_tap_name(&tap_name)?;
    } else if let Some((user, repo)) = name.split_once('/')
        && !repo.contains('/')
    {
        validate_tap_name(&format!("{user}/{repo}"))?;
    }
    Ok(())
}

fn validate_tap_name(name: &str) -> Result<(), String> {
    let Some((user, repo)) = name.split_once('/') else {
        return Ok(());
    };
    if !valid_tap_component(user) || !valid_tap_component(repo) {
        return Err(format!("Error: Invalid usage: Invalid tap name: '{name}'"));
    }
    Ok(())
}

/// brew `Trust.target`.
pub fn target(
    prefix: &Prefix,
    api: &super::api::Api,
    name: &str,
    kind: Option<TrustType>,
    include_existing: bool,
    tap_remote: Option<&str>,
) -> Result<(TrustType, String), String> {
    if let Some(kind) = kind {
        return Ok((
            kind,
            trust_name(prefix, api, kind, name, include_existing, tap_remote)?,
        ));
    }
    infer_target(prefix, api, name, include_existing)
}

/// brew `Trust.trusted?`.
pub fn trusted(prefix: &Prefix, kind: TrustType, name: &str) -> bool {
    let name = name.to_lowercase();
    let entries = trusted_entries(kind);
    if entries.contains(&name) {
        return true;
    }
    if kind == TrustType::Tap {
        if remote_reference(&name) {
            return false;
        }
        return TapRef::fetch(prefix, &name).is_ok_and(|tap| tap.explicitly_trusted());
    }
    let Some((tap_name, item)) = split_full_name(&name) else {
        return false;
    };
    let Ok(tap) = TapRef::fetch(prefix, &tap_name) else {
        return false;
    };
    if tap.trusted() {
        return true;
    }
    if entries.contains(&item_trust_name(kind, &tap, &item, false, None)) {
        return true;
    }
    if !tap.uses_custom_remote() {
        return false;
    }
    let Some(remote) = tap.remote.as_deref() else {
        return false;
    };
    entries.iter().any(|entry| {
        entry
            .strip_suffix(&format!("/{item}"))
            .is_some_and(|head| same_remote(head, remote))
    })
}

/// brew `Bundle::Trust.entries`: Brewfile `trusted:` options
/// as trust-store entries, resolved through [`target`].
pub fn bundle_trust_entries(
    prefix: &Prefix,
    api: &super::api::Api,
    entries: &[super::bundle::CleanupEntry],
) -> Result<Vec<(TrustType, String)>, String> {
    use super::bundle::TrustedOpt;
    let mut tap_remotes: Vec<(String, String)> = Vec::new();
    for entry in entries.iter().filter(|e| e.kind == "tap") {
        if let Some(clone) = entry.clone_target.as_deref().filter(|c| !c.is_empty()) {
            tap_remotes.push((entry.name.to_lowercase(), clone.to_owned()));
        }
    }
    let mut out: Vec<(TrustType, String)> = Vec::new();
    for entry in entries {
        let Some(trusted) = &entry.trusted else {
            continue;
        };
        let mut targets: Vec<(TrustType, String, Option<String>)> = Vec::new();
        match entry.kind {
            "tap" => {
                let tap_remote = entry.clone_target.clone().filter(|c| !c.is_empty());
                match trusted {
                    TrustedOpt::True => {
                        targets.push((TrustType::Tap, entry.name.clone(), tap_remote));
                    }
                    TrustedOpt::Items(pairs) => {
                        const ALLOWED: [(&str, TrustType); 6] = [
                            ("formula", TrustType::Formula),
                            ("formulae", TrustType::Formula),
                            ("cask", TrustType::Cask),
                            ("casks", TrustType::Cask),
                            ("command", TrustType::Command),
                            ("commands", TrustType::Command),
                        ];
                        let unsupported: Vec<&str> = pairs
                            .iter()
                            .map(|(k, _)| k.as_str())
                            .filter(|k| !ALLOWED.iter().any(|(a, _)| a == k))
                            .collect();
                        if !unsupported.is_empty() {
                            return Err(format!(
                                "Error: Invalid usage: Unsupported trusted keys: {}",
                                unsupported.join(", ")
                            ));
                        }
                        for kind in [TrustType::Formula, TrustType::Cask, TrustType::Command] {
                            for (key, key_kind) in ALLOWED {
                                if key_kind != kind {
                                    continue;
                                }
                                for (k, items) in pairs.iter().filter(|(k, _)| k == key) {
                                    let _ = k;
                                    for item in items {
                                        let item_name = super::formula::Formula::short_name(item);
                                        if item_name.is_empty() {
                                            continue;
                                        }
                                        targets.push((
                                            kind,
                                            format!("{}/{item_name}", entry.name),
                                            tap_remote.clone(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    TrustedOpt::Other => {}
                }
            }
            "brew" | "cask" => {
                if *trusted != TrustedOpt::True {
                    continue;
                }
                let full_name = &entry.raw;
                if with_formula_name(prefix, full_name).is_none() {
                    continue;
                }
                let kind = if entry.kind == "brew" {
                    TrustType::Formula
                } else {
                    TrustType::Cask
                };
                let tap_remote = split_full_name(full_name).and_then(|(tap_name, _)| {
                    let canonical = super::bundle::sanitize_tap_display(&tap_name);
                    tap_remotes
                        .iter()
                        .find(|(n, _)| *n == canonical)
                        .map(|(_, r)| r.clone())
                });
                targets.push((kind, full_name.clone(), tap_remote));
            }
            _ => {}
        }
        for (kind, name, tap_remote) in targets {
            let resolved = target(prefix, api, &name, Some(kind), false, tap_remote.as_deref())?;
            if !out.contains(&resolved) {
                out.push(resolved);
            }
        }
    }
    Ok(out)
}

/// brew `Trust.wholly_untrusted_taps`: untrusted taps with no trusted
/// formula, cask or command entry under their prefix.
pub fn wholly_untrusted_taps(prefix: &Prefix) -> Vec<TapRef> {
    untrusted_taps(prefix)
        .into_iter()
        .filter(|tap| {
            let mut prefixes = vec![tap.reference(None).to_lowercase()];
            if tap.uses_custom_remote() {
                prefixes.push(tap.name());
            }
            ![TrustType::Formula, TrustType::Cask, TrustType::Command]
                .iter()
                .any(|kind| {
                    trusted_entries(*kind)
                        .iter()
                        .any(|entry| prefixes.iter().any(|p| entry.starts_with(&format!("{p}/"))))
                })
        })
        .collect()
}

/// brew `Trust.untrusted_taps`.
pub fn untrusted_taps(prefix: &Prefix) -> Vec<TapRef> {
    let mut taps: Vec<TapRef> = super::tap::list(prefix)
        .iter()
        .map(|t| TapRef::from_tap(prefix, t))
        .filter(|t| !t.official() && !t.trusted())
        .collect();
    taps.sort_by_key(|t| t.name());
    taps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_store_writer_preserves_exclusive_creation_custody() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.lock");
        let mut attempts = 0;
        let acquired = acquire_store_lock(
            &path,
            std::time::Instant::now() - std::time::Duration::from_secs(3),
            |path, mode| {
                attempts += 1;
                if attempts <= 201 {
                    Err(std::io::ErrorKind::AlreadyExists.into())
                } else {
                    crate::platform::open_create_new_write(path, mode)
                }
            },
            |_| {},
        );
        let acquired = acquired.expect("the preceding store writer releases its lease");
        assert_eq!(attempts, 202);
        assert!(crate::platform::open_create_new_write(&path, 0o600).is_err());
        drop(acquired);
    }

    #[test]
    fn a_store_lock_hang_or_conclusive_io_error_does_not_write() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.lock");
        let mut attempts = 0;
        assert!(
            acquire_store_lock(
                &path,
                std::time::Instant::now() - std::time::Duration::from_secs(61),
                |_, _| {
                    attempts += 1;
                    Err(std::io::ErrorKind::AlreadyExists.into())
                },
                |_| panic!("an exhausted hang detector must not poll again"),
            )
            .is_err()
        );
        assert_eq!(attempts, 1);
        assert!(
            acquire_store_lock(
                &path,
                std::time::Instant::now(),
                |_, _| Err(std::io::ErrorKind::PermissionDenied.into()),
                |_| panic!("a conclusive error must not be retried"),
            )
            .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn normalize_remote_matches_brew_rules() {
        assert_eq!(
            normalize_remote("git@github.com:Someone/Homebrew-Tools.git"),
            "https://github.com/someone/homebrew-tools"
        );
        assert_eq!(
            normalize_remote("https://github.com/Someone/Tools/"),
            "https://github.com/someone/tools"
        );
        // `.git` is only insignificant on known hosts.
        assert_eq!(
            normalize_remote("https://example.com/Someone/Tools.git"),
            "https://example.com/someone/tools.git"
        );
        assert!(same_remote(
            "git@github.com:someone/tools",
            "https://github.com/someone/tools.git"
        ));
        assert!(!same_remote(
            "https://example.com/someone/tools",
            "https://example.com/someone/tools.git"
        ));
        assert!(remote_reference("https://example.com/x"));
        assert!(remote_reference("/tmp/x"));
        assert!(!remote_reference("someone/tools"));
    }

    #[test]
    fn store_order_and_pretty_follow_insertion() {
        let pairs: StorePairs = vec![
            (
                "trustedtaps".into(),
                aterm_json::Value::Array(vec![aterm_json::Value::String("a/b".into())]),
            ),
            (
                "trustedformulae".into(),
                aterm_json::Value::Array(vec![aterm_json::Value::String("a/b/c".into())]),
            ),
        ];
        assert_eq!(
            pretty_pairs(&pairs),
            "{\n  \"trustedtaps\": [\n    \"a/b\"\n  ],\n  \"trustedformulae\": [\n    \"a/b/c\"\n  ]\n}"
        );
        assert_eq!(
            top_level_keys(b"{\"trustedcasks\":[],\"trustedtaps\":[\"x/y\"]}"),
            ["trustedcasks", "trustedtaps"]
        );
    }
}

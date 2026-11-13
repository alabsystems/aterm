// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm doctor`'s `spotlight:` row.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const QUERY: Duration = Duration::from_secs(10);

const RUST_DOCS: &str = "share/doc/rust/html/";

/// init's Rust in Spotlight group, whose items remove rust-docs: `aterm_machines::init_spotlight::ID`,
/// which this crate reaches on macOS alone.
const INIT_SPOTLIGHT: &str = "init.spotlight";

/// What Spotlight's index holds under the home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spotlit {
    /// Each folder holding indexed crate files, `~`-relative, with its count, most first.
    pub builds: Vec<(String, usize)>,
    /// The folders of [`Spotlit::builds`] that are an x.py checkout's `build/`, in its order,
    /// each with what its checkout's bootstrap does with it ([`Bootstrap`]).
    pub xpy: Vec<(String, Bootstrap)>,
    /// rustup's home, `~`-relative, and the `*.html` indexed in it.
    pub rustup: Option<(String, Pages)>,
    /// Why bare doctor leaves the indexed `rust-docs` pages where they are, when it does
    /// ([`kept_why`]): the person's System State plan leaves Rust in Spotlight unchecked, or the
    /// plan could not be read, and a choice doctor cannot see is never overridden.
    pub docs_kept: Option<String>,
}

impl Spotlit {
    /// Whether Spotlight indexes `rust-docs` pages the person did not choose to keep: bare
    /// `aterm doctor` then sets the docs aside (macOS's `aterm_machines::init::Doctor::rust_docs`).
    pub(crate) fn docs_indexed(&self) -> bool {
        self.docs_kept.is_none() && self.has_docs()
    }

    /// Whether Spotlight indexes any `rust-docs` page.
    fn has_docs(&self) -> bool {
        self.rustup
            .as_ref()
            .is_some_and(|(_, pages)| !pages.docs.is_empty())
    }

    /// These readings with the person's choice on indexed `rust-docs` read through `kept`, which
    /// is asked only when there are some ([`kept_by_choice`]).
    fn with_choice(mut self, kept: impl FnOnce() -> Option<String>) -> Self {
        self.docs_kept = if self.has_docs() { kept() } else { None };
        self
    }
}

/// Whether System State's records say the person chose to leave Rust in Spotlight as it is:
/// they cancelled the first-launch review (every group then reads Leave as is), or the plan in
/// force is theirs, not one aterm chose by itself, and leaves Rust in Spotlight unchecked.
#[cfg(any(target_os = "macos", test))]
const fn declines(cancelled: bool, provisional: bool, spotlight_checked: bool) -> bool {
    cancelled || (!provisional && !spotlight_checked)
}

/// Why the indexed `rust-docs` stay, from the setup records' read of whether the review was
/// cancelled, whether the plan is provisional and whether it checks Rust in Spotlight
/// ([`declines`]); `None` when bare doctor sets them aside. Records that cannot be read keep
/// them: doctor never overrides a choice it cannot see.
#[cfg(any(target_os = "macos", test))]
fn kept_why(read: Result<(bool, bool, bool), String>) -> Option<String> {
    match read {
        Ok((cancelled, provisional, spotlight_checked)) => {
            declines(cancelled, provisional, spotlight_checked).then(|| {
                "kept, as your System State plan leaves Rust in Spotlight unchecked".to_owned()
            })
        }
        Err(why) => Some(format!(
            "kept, as your System State plan could not be read ({why})"
        )),
    }
}

/// [`kept_why`] over this Mac's setup records.
#[cfg(target_os = "macos")]
fn kept_by_choice() -> Option<String> {
    kept_why(
        crate::macos_doctor::initial_setup_snapshot()
            .map(|snapshot| {
                (
                    snapshot.cancelled,
                    snapshot.provisional,
                    snapshot
                        .plan
                        .selection
                        .contains(aterm_types::macos_setup::Setting::Spotlight),
                )
            })
            .map_err(|why| why.to_string()),
    )
}

/// No setup records off macOS.
#[cfg(not(target_os = "macos"))]
const fn kept_by_choice() -> Option<String> {
    None
}

/// What becomes of the indexed `build/` an x.py checkout's bootstrap writes ([`bootstrap_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bootstrap {
    /// The checkout holds Trust's noindex-layout module (`aterm_toolchain::xpy::NOINDEX_LAYOUT`):
    /// its next x.py build moves `build/` to `build.noindex`, which Spotlight skips.
    Moves,
    /// Its bootstrap does not make that move, and the checkout supports the layout made by
    /// hand (`aterm_toolchain::xpy::supports_noindex_layout`: Trust since 2026-09-25):
    /// [`BY_HAND`], which first checks the checkout's build lock is free, takes `build/` out of
    /// the index.
    ByHand,
    /// Something named `build.noindex` (a folder, a file, a link, one that leads nowhere) is
    /// already beside the indexed `build/`: a move there would fail, or nest one tree in the
    /// other, and which of the two the checkout keeps (a `bootstrap.toml` `build-dir` can name
    /// either) is not read here, so the row says what it saw and gives no command.
    Blocked,
    /// Its checkout does not support the layout (an upstream rustc checkout, Trust before
    /// 2026-09-25), where a move by hand leaves `build.noindex` untracked by git and
    /// `x.py clean --all` removing only the link; so the row gives none, and `build/` stays
    /// indexed while it is there.
    Stays,
}

/// The `*.html` Spotlight indexes in rustup's home, each list most first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pages {
    /// `rust-docs` pages (a toolchain's `share/doc/rust/html`), per toolchain.
    pub docs: Vec<(String, usize)>,
    /// Every other page.
    pub other: Vec<(String, usize)>,
}

/// Spotlight's count, or why it was not taken; `None` off macOS (no row).
pub(crate) fn facts() -> Option<Result<Spotlit, String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if cfg!(test) || aterm_uds::test_binary::scratch_root().is_some() {
        return Some(Err("a test binary".into()));
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Some(Err("no home folder".into()));
    };
    let rustup = rustup_dir(&home);
    let kept = atpkg::probe_cache::file("spotlight")
        .and_then(|file| Some((file, day_key(&home, &rustup, atpkg::probe_cache::today()?)?)));
    Some(
        measure_kept(kept.as_ref(), || {
            measure(&home, &rustup, &|dir, pattern| mdfind(dir, pattern))
        })
        .map(|lit| lit.with_choice(kept_by_choice)),
    )
}

/// rustup's home: `$RUSTUP_HOME`, else `~/.rustup`.
fn rustup_dir(home: &Path) -> PathBuf {
    std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".rustup"), Into::into)
}

/// What a clean verdict depends on besides the index itself: the day, the home, rustup's home
/// and each toolchain's `rust-docs` folder (its time, or its absence), so a toolchain or a
/// `rust-docs` install, removal or set-aside measures again at once. A build written under the
/// home since the last measure waits for the next day.
fn day_key(home: &Path, rustup: &Path, day: u64) -> Option<String> {
    let mut parts = vec![
        Some(day.to_string()),
        Some(home.display().to_string()),
        Some(rustup.display().to_string()),
    ];
    let toolchains = rustup.join("toolchains");
    parts.push(Some(stamp(&toolchains)));
    if let Ok(entries) = std::fs::read_dir(&toolchains) {
        let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let docs = toolchains.join(&name).join("share/doc/rust");
            parts.push(Some(format!(
                "{} {} {}",
                name.to_string_lossy(),
                stamp(&docs),
                stamp(&docs.join("html"))
            )));
        }
    }
    atpkg::probe_cache::key(parts)
}

/// A folder's modification time in nanoseconds, or `absent`.
fn stamp(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(|| "absent".to_owned(), |at| at.as_nanos().to_string())
}

/// `measure`, or the clean verdict `kept` (its file and [`day_key`]) holds: only a clean one is
/// kept, so a verdict that names something to fix is measured every run and the fix shows at
/// once.
fn measure_kept(
    kept: Option<&(PathBuf, String)>,
    measure: impl FnOnce() -> Result<Spotlit, String>,
) -> Result<Spotlit, String> {
    if let Some((file, key)) = kept
        && let Some(lit) = atpkg::probe_cache::get(file, key).and_then(|v| decode(&v))
    {
        return Ok(lit);
    }
    let measured = measure();
    if let (Some((file, key)), Ok(lit)) = (kept, &measured)
        && lit.builds.is_empty()
        && lit
            .rustup
            .as_ref()
            .is_none_or(|(_, pages)| pages.docs.is_empty())
        && let Some(encoded) = encode(lit)
    {
        atpkg::probe_cache::put(file, key, &encoded);
    }
    measured
}

/// Separates fields in a kept clean verdict.
const FIELD: char = '\u{1c}';

/// A clean verdict as one line: `clean`, then rustup's home and each other page source with its
/// count; `None` if a field is empty or holds a delimiter or line break.
fn encode(lit: &Spotlit) -> Option<String> {
    let mut out = String::from("clean");
    if let Some((home, pages)) = &lit.rustup {
        if home.is_empty() || home.contains([FIELD, '\n', '\r', '\u{1e}']) {
            return None;
        }
        out.push(FIELD);
        out.push_str(home);
        for (source, n) in &pages.other {
            if source.is_empty() || source.contains([FIELD, '\n', '\r', '\u{1e}']) {
                return None;
            }
            out.push(FIELD);
            out.push_str(source);
            out.push(FIELD);
            out.push_str(&n.to_string());
        }
    }
    Some(out)
}

fn decode(kept: &str) -> Option<Spotlit> {
    let mut fields = kept.split(FIELD);
    if fields.next()? != "clean" {
        return None;
    }
    let rustup = match fields.next() {
        None => None,
        Some(home) => {
            if home.is_empty() {
                return None;
            }
            let mut other = Vec::new();
            while let Some(source) = fields.next() {
                if source.is_empty() {
                    return None;
                }
                let count = fields
                    .next()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
                    .parse()
                    .ok()?;
                other.push((source.to_owned(), count));
            }
            Some((
                home.to_owned(),
                Pages {
                    docs: Vec::new(),
                    other,
                },
            ))
        }
    };
    Some(Spotlit {
        docs_kept: None,
        builds: Vec::new(),
        xpy: Vec::new(),
        rustup,
    })
}

/// One Spotlight query: the NUL-separated paths named `pattern` under a folder.
type Query<'a> = dyn Fn(&Path, &str) -> Result<String, String> + Sync + 'a;

/// The two queries, crate files under the home and HTML pages in rustup's home, run at once (one
/// after the other where no thread starts: [`crate::beside`]).
fn measure(home: &Path, rustup: &Path, query: &Query<'_>) -> Result<Spotlit, String> {
    let real_rustup = std::fs::canonicalize(rustup)
        .ok()
        .filter(|real| real.is_dir());
    let (crates, listed) = std::thread::scope(|scope| {
        let html = real_rustup.as_deref().map(|real| {
            crate::beside::start(scope, "aterm-spotlight-html", move || query(real, "*.html"))
        });
        let crates = query(home, "*.rmeta");
        let listed = html.map(crate::beside::Beside::answer);
        (crates, listed)
    });
    let crates = crates?;
    // `mdfind` prints each path as the volume spells it, which a home reached through a link
    // spells otherwise: both spellings are the home's.
    let homes: Vec<PathBuf> = std::iter::once(home.to_path_buf())
        .chain(std::fs::canonicalize(home).ok().filter(|real| real != home))
        .collect();
    let mut builds = Tally::default();
    // Keyed by each crate file's own folder: a build root is read off the path, which stops at
    // the first folder named `target` or `build`, and so above a checkout that lies under one.
    let mut placed: HashMap<String, String> = HashMap::new();
    let mut bootstraps: HashMap<String, Bootstrap> = HashMap::new();
    for path in crates.split('\0').filter(|p| !p.is_empty()) {
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let folder = placed.entry(dir.to_owned()).or_insert_with_key(|dir| {
            let (folder, repo) = counted_under(&homes, dir, path);
            if let Some(repo) = repo {
                bootstraps
                    .entry(folder.clone())
                    .or_insert_with(|| bootstrap_of(&repo));
            }
            folder
        });
        builds.add(folder.clone());
    }
    let builds = builds.most_first();
    let xpy = builds
        .iter()
        .filter_map(|(folder, _)| Some((folder.clone(), *bootstraps.get(folder)?)))
        .collect();
    let rustup_home = match (real_rustup, listed) {
        (Some(real), Some(listed)) => {
            let listed = listed?;
            let mut manifests = HashMap::new();
            let found = pages(
                &real.to_string_lossy(),
                listed.split('\0').filter(|p| !p.is_empty()),
                |toolchain, rest| {
                    installed_by(
                        manifests.entry(toolchain.to_owned()).or_insert_with(|| {
                            read_manifests(&real.join("toolchains").join(toolchain))
                        }),
                        rest,
                    )
                },
                |folder| tilde(home, folder),
            );
            (found != Pages::default()).then(|| (tilde(home, &rustup.to_string_lossy()), found))
        }
        _ => None,
    };
    Ok(Spotlit {
        docs_kept: None,
        builds,
        xpy,
        rustup: rustup_home,
    })
}

/// The folder the crate file `path`, in the folder `dir`, is counted under, `~`-relative, and,
/// for an x.py checkout's `build/`, the checkout: a file inside that `build/` (in a stage tree,
/// a stage sysroot) is counted under the `build/` itself, which is what a move would take
/// (`aterm_toolchain::xpy`), found from the file's own folder up, so a checkout under a folder
/// named `build` or `target` is still met. Any other file is counted under its [`build_root`],
/// and so is one under no spelling of the home or in a folder of it aterm never looks into
/// (`aterm_toolchain::checkout::never_looked_at`: on macOS the privacy-guarded folders, where
/// even a look can put a dialog on the person's screen), where nothing is read.
fn counted_under(homes: &[PathBuf], dir: &str, path: &str) -> (String, Option<PathBuf>) {
    use aterm_toolchain::{checkout, xpy};
    let root = || tilde_in(homes, &build_root(path));
    let dir = Path::new(dir);
    let Some(home) = homes.iter().find(|home| dir.starts_with(home)) else {
        return (root(), None);
    };
    if checkout::never_looked_at(dir, Some(home)).is_some() {
        return (root(), None);
    }
    match xpy::build_holding(dir) {
        Some((build, xpy::BUILD)) => (
            tilde_in(homes, &build.to_string_lossy()),
            build.parent().map(Path::to_path_buf),
        ),
        _ => (root(), None),
    }
}

/// What becomes of the x.py checkout `repo`'s indexed `build/`, read once per checkout. Only a
/// clean "not found" for `build.noindex` leaves the way clear: anything there, or a look that
/// fails, is [`Bootstrap::Blocked`].
fn bootstrap_of(repo: &Path) -> Bootstrap {
    use aterm_toolchain::xpy;
    let beside = std::fs::symlink_metadata(repo.join(xpy::BUILD_NOINDEX));
    if !beside.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        Bootstrap::Blocked
    } else if xpy::moves_build_aside(repo) {
        Bootstrap::Moves
    } else if xpy::supports_noindex_layout(repo) {
        Bootstrap::ByHand
    } else {
        Bootstrap::Stays
    }
}

/// Counts per name, in first-seen order until [`Tally::most_first`].
#[derive(Default)]
struct Tally(Vec<(String, usize)>);

impl Tally {
    fn add(&mut self, name: String) {
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, k)) => *k = k.saturating_add(1),
            None => self.0.push((name, 1)),
        }
    }

    fn most_first(mut self) -> Vec<(String, usize)> {
        self.0
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.0
    }
}

/// Sorts Spotlight's `*.html` paths under rustup's home `root`.
pub(crate) fn pages<'a>(
    root: &str,
    paths: impl IntoIterator<Item = &'a str>,
    mut listed: impl FnMut(&str, &str) -> Option<String>,
    shown: impl Fn(&str) -> String,
) -> Pages {
    let mut docs = Tally::default();
    let mut other = Tally::default();
    for path in paths {
        let in_toolchain = path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix("/toolchains/"))
            .and_then(|rest| rest.split_once('/'));
        if let Some((toolchain, rest)) = in_toolchain {
            if rest.starts_with(RUST_DOCS) {
                docs.add(toolchain.to_owned());
                continue;
            }
            if let Some(component) = listed(toolchain, rest) {
                other.add(format!("the {component} component"));
                continue;
            }
        }
        let folder = path.rsplit_once('/').map_or(path, |(dir, _)| dir);
        other.add(shown(folder));
    }
    Pages {
        docs: docs.most_first(),
        other: other.most_first(),
    }
}

fn read_manifests(toolchain: &Path) -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir(toolchain.join("lib/rustlib")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let component = name.strip_prefix("manifest-")?.to_owned();
            let text = manifest_text(&entry.path())?;
            Some((component, text))
        })
        .collect()
}

fn manifest_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    const LIMIT: u64 = 16 * 1024 * 1024;

    let (file, metadata) = aterm_uds::regular_file::open(path).ok()?;
    if metadata.len() > LIMIT {
        return None;
    }
    let mut text = String::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .saturating_add(1),
    );
    file.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= LIMIT).then_some(text)
}

/// The component whose manifest lists `rest`, as a `file:` entry or under a `dir:` one.
pub(crate) fn installed_by(manifests: &[(String, String)], rest: &str) -> Option<String> {
    manifests.iter().find_map(|(component, text)| {
        text.lines()
            .any(|line| {
                line.strip_prefix("file:") == Some(rest)
                    || line.strip_prefix("dir:").is_some_and(|dir| {
                        rest.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
                    })
            })
            .then(|| component.clone())
    })
}

/// The paths named `pattern` under `dir` (NUL-separated): `mdfind -count` first, and the list
/// only when the count is not zero, so a clean index costs no listing.
fn mdfind(dir: &Path, pattern: &str) -> Result<String, String> {
    count_then_list(
        || mdfind_run(dir, pattern, true),
        || mdfind_run(dir, pattern, false),
    )
}

/// `list`'s answer, or nothing when `count` says there is nothing to list; a count that reads
/// as no number is refused, never taken for one, and so is an empty list after a count of some:
/// `mdfind` prints nothing and exits 0 when its server cannot be reached, and that is no
/// measurement of the index.
fn count_then_list(
    count: impl FnOnce() -> Result<String, String>,
    list: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let said = count()?;
    let said = said.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let n: u64 = said
        .parse()
        .map_err(|_| format!("mdfind gave no count ({said:?})"))?;
    if n == 0 {
        return Ok(String::new());
    }
    let listed = list()?;
    if listed.is_empty() {
        return Err(format!("mdfind counted {n} but listed none"));
    }
    Ok(listed)
}

fn mdfind_run(dir: &Path, pattern: &str, count: bool) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/mdfind");
    if count {
        cmd.arg("-count");
    }
    cmd.arg("-onlyin").arg(dir).arg("-0");
    cmd.arg(format!("kMDItemFSName == \"{pattern}\""));
    let now = Instant::now();
    let deadline = now
        .checked_add(QUERY)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 86_400));
    let listed = listing(&mut cmd, deadline, LISTING)?;
    if count {
        Ok(listed)
    } else {
        whole_list(listed)
    }
}

/// The most a Spotlight listing is read to: the listing reader's hard maximum, 64 MiB, some
/// 450,000 paths.
const LISTING: usize = aterm_uds::deadline::MAX_COMPLETE_LISTING_BYTES;

/// `listed`, a `-0` list, when it ends as one does: `mdfind -0` ends every path with a NUL, so
/// one that does not was cut short, and is refused rather than counted with a cut path. An
/// empty one is [`count_then_list`]'s to judge against its count.
fn whole_list(listed: String) -> Result<String, String> {
    if listed.is_empty() || listed.ends_with('\0') {
        Ok(listed)
    } else {
        Err("mdfind's list ended mid-path".to_owned())
    }
}

/// What `cmd` (an `mdfind`) printed by `deadline`, every byte of it, read through a pipe to its
/// end (`aterm_uds::deadline::output_until_complete_listing`). A listing of every crate
/// file, or of every page of a few toolchains' docs, runs to tens of MiB: a reader that kept a
/// bounded tail would count part of the index, and one that had it written to a file would
/// count what a full disk let through, since `mdfind` does not check its writes and still
/// exits 0. One past `most` bytes is refused rather than counted short; past `deadline` the
/// reader kills `cmd`'s process group and refuses.
fn listing(
    cmd: &mut aterm_uds::spawnfd::Command,
    deadline: Instant,
    most: usize,
) -> Result<String, String> {
    use aterm_uds::deadline::{CompleteOutputError, RunError};
    let out = aterm_uds::deadline::output_until_complete_listing(cmd, deadline, most).map_err(
        |e| match e {
            CompleteOutputError::StdoutTooLarge { limit } => format!(
                "Spotlight lists more than {} MiB of paths, more than is read",
                limit >> 20
            ),
            CompleteOutputError::Run(RunError::TimedOut) => {
                format!("mdfind did not answer within {}s", QUERY.as_secs())
            }
            CompleteOutputError::Run(RunError::Spawn(e)) => format!("mdfind did not start: {e}"),
            e => format!("mdfind did not answer: {e}"),
        },
    )?;
    if !out.status.success() {
        let why = &out.stderr[..out.stderr.len().min(4096)];
        return Err(format!(
            "mdfind failed: {}",
            String::from_utf8_lossy(why).trim()
        ));
    }
    Ok(String::from_utf8(out.stdout)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// The folder a crate file's build lives in, as a person would delete or rename it.
pub(crate) fn build_root(path: &str) -> String {
    if let Some((sysroot, _)) = path.split_once("/lib/rustlib/") {
        return sysroot.to_owned();
    }
    let parts: Vec<&str> = path.split('/').collect();
    for name in ["target", "build"] {
        if let Some(i) = parts[..parts.len().saturating_sub(1)]
            .iter()
            .position(|c| *c == name)
        {
            return parts[..=i].join("/");
        }
    }
    if let Some(i) = parts.iter().rposition(|c| *c == "deps")
        && i >= 2
    {
        return parts[..i - 1].join("/");
    }
    parts[..parts.len().saturating_sub(1)].join("/")
}

/// `path` with `home`, spelled with or without a trailing `/`, as `~`; a home of `/` is no
/// home to shorten.
fn tilde(home: &Path, path: &str) -> String {
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_owned();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// `path` `~`-relative by the first spelling of the home it lies under ([`tilde`]).
fn tilde_in(homes: &[PathBuf], path: &str) -> String {
    homes
        .iter()
        .map(|home| tilde(home, path))
        .find(|shown| shown != path)
        .unwrap_or_else(|| path.to_owned())
}

fn largest(counts: &[(String, usize)], more: &str) -> String {
    let mut top: Vec<String> = counts
        .iter()
        .take(3)
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    if counts.len() > 3 {
        top.push(format!("{} more {more}", counts.len() - 3));
    }
    top.join(", ")
}

fn total(counts: &[(String, usize)]) -> usize {
    counts
        .iter()
        .fold(0usize, |acc, (_, n)| acc.saturating_add(*n))
}

/// `folders` named, at most three and how many more: `a`, `a and b`, `a, b and c`,
/// `a, b, c and 2 more`.
fn named(folders: &[&str]) -> String {
    match folders {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [head @ .., last] if folders.len() <= 3 => format!("{} and {last}", head.join(", ")),
        _ => format!("{} and {} more", folders[..3].join(", "), folders.len() - 3),
    }
}

/// The move by hand that takes an x.py checkout's `build/` out of Spotlight, run in the checkout
/// (Trust's `docs/BUILD_AND_INSTALL.md`, "Keeping Spotlight out of build/"). It moves nothing
/// unless Trust's build lock says free (`status` exits 0 only then: 3 for held, and for free
/// with something running outside the lock) and nothing named `build.noindex` is there, not
/// even a link that leads nowhere, which `test -e` alone would miss.
const BY_HAND: &str = "scripts/trust_build_lock.sh status && test ! -e build.noindex && test ! -L \
                       build.noindex && mv build build.noindex && ln -s build.noindex build";

/// One line of [`xpy_lines`]: `one` of a single folder, `many` of the folders named.
fn said(
    folders: &[&str],
    one: impl Fn(&str) -> String,
    many: impl Fn(&str) -> String,
) -> Option<String> {
    match folders {
        [] => None,
        [folder] => Some(one(folder)),
        _ => Some(many(&named(folders))),
    }
}

/// What the row says of the indexed x.py `build/` folders, a line for each [`Bootstrap`] they
/// are: whose checkout's next x.py build moves them out of Spotlight's way, which the move by
/// hand ([`BY_HAND`]) takes out, which have a `build.noindex` beside them already, and which
/// stay.
fn xpy_lines(lit: &Spotlit) -> Vec<String> {
    let of = |kind: Bootstrap| -> Vec<&str> {
        lit.xpy
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(folder, _)| folder.as_str())
            .collect()
    };
    [
        said(
            &of(Bootstrap::Moves),
            |one| {
                format!(
                    "{one} is an x.py checkout's build tree; its next x.py build moves it to \
                     build.noindex, which Spotlight skips"
                )
            },
            |many| {
                format!(
                    "{many} are x.py checkouts' build trees; each checkout's next x.py build \
                     moves its tree to build.noindex, which Spotlight skips"
                )
            },
        ),
        said(
            &of(Bootstrap::ByHand),
            |one| {
                format!(
                    "{one} is an x.py checkout's build tree, and its bootstrap does not move it \
                     to build.noindex; `{BY_HAND}` in the checkout takes it out of the index, \
                     and moves nothing while its build lock is held"
                )
            },
            |many| {
                format!(
                    "{many} are x.py checkouts' build trees, and their bootstraps do not move \
                     them to build.noindex; `{BY_HAND}` in a checkout takes its tree out of the \
                     index, and moves nothing while its build lock is held"
                )
            },
        ),
        said(
            &of(Bootstrap::Blocked),
            |one| {
                format!(
                    "{one} is an x.py checkout's build tree, and {one}.noindex is already there \
                     beside it; a move there would fail or nest one in the other, so {one} stays \
                     indexed while both are there"
                )
            },
            |many| {
                format!(
                    "{many} are x.py checkouts' build trees, each with a build.noindex already \
                     there beside it; a move there would fail or nest one in the other, so each \
                     stays indexed while both are there"
                )
            },
        ),
        said(
            &of(Bootstrap::Stays),
            |one| {
                format!(
                    "{one} is an x.py checkout's build tree, and its checkout does not support \
                     Trust's build.noindex layout (an upstream rustc checkout, or Trust before \
                     2026-09-25), so it stays indexed while it is there"
                )
            },
            |many| {
                format!(
                    "{many} are x.py checkouts' build trees, and their checkouts do not support \
                     Trust's build.noindex layout (an upstream rustc checkout, or Trust before \
                     2026-09-25), so each stays indexed while it is there"
                )
            },
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The row's fact and whether it is clean, pure over the count.
pub(crate) fn row(measured: &Result<Spotlit, String>) -> (bool, String) {
    let lit = match measured {
        Err(why) => return (false, format!("not measured ({why})")),
        Ok(lit) => lit,
    };
    let none = Pages::default();
    let (home, pages) = lit
        .rustup
        .as_ref()
        .map_or(("~/.rustup", &none), |(home, pages)| (home.as_str(), pages));
    let clean = lit.builds.is_empty() && pages.docs.is_empty();
    let mut lines = Vec::new();
    if clean {
        lines.push("Spotlight indexes no Rust build output or rust-docs under ~".to_owned());
    }
    if !lit.builds.is_empty() {
        lines.push(format!(
            "Spotlight indexes {} crate builds (.rmeta) under ~: {}",
            total(&lit.builds),
            largest(&lit.builds, "folders")
        ));
        lines.extend(xpy_lines(lit));
        let others = lit
            .builds
            .iter()
            .filter(|(folder, _)| !lit.xpy.iter().any(|(x, _)| x == folder))
            .count();
        if others > 0 {
            let (written, them) = match (lit.xpy.is_empty(), others) {
                (true, _) => ("written", "them"),
                (false, 1) => ("the other folder was written", "it"),
                (false, _) => ("the other folders were written", "them"),
            };
            lines.push(format!(
                "{written} where the shims' rule did not place {them} (a direct targo, another \
                 repository's build, an installed toolchain); a folder leaves the index when \
                 it is deleted or named *.noindex"
            ));
        }
    }
    if !pages.docs.is_empty() {
        lines.push(format!(
            "{} rust-docs pages indexed in {home} ({}): {}",
            total(&pages.docs),
            largest(&pages.docs, "toolchains"),
            lit.docs_kept.as_ref().map_or_else(
                || "bare `aterm doctor` keeps them out of Spotlight".to_owned(),
                |why| format!(
                    "{why}; `{}` removes them",
                    aterm_converge::Remedy::Init {
                        only: INIT_SPOTLIGHT.to_owned()
                    }
                    .command()
                    .unwrap_or_default()
                )
            )
        ));
    }
    if !pages.other.is_empty() {
        lines.push(format!(
            "{} other HTML files indexed in {home}, not rust-docs: {}",
            total(&pages.other),
            largest(&pages.other, "sources")
        ));
    }
    (clean, lines.join("\n"))
}

#[cfg(test)]
#[path = "spotlight_tests.rs"]
mod tests;

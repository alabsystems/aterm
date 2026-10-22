// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Writing a machine's secrets to disk (docs/DESIGN-secrets-2026-10-20.md §7, Sync).
//!
//! A file name lands at `~/.secrets/<name>`, a `~/a/b` name at that path under home. Every
//! write is atomic at 0600; `~/.secrets` is narrowed to 0700. The [`Manifest`] remembers what
//! the vault wrote, so a file someone else wrote or edited is moved to
//! `~/.secrets/.replaced/<flattened name>.<unix secs>` before it is overwritten, and a file
//! is removed only while it still holds exactly what the vault wrote. Reports carry names,
//! never values.
//!
//! A symlink at a target is never followed: it is moved aside (the link itself, not what it
//! names) and a regular file takes its place. Nor is a symlinked directory written through:
//! an entry behind one (`~/.secrets` itself, or a directory between home and a `~/a/b/c`
//! target) fails, naming it, and the rest proceed. A target equal to the new value is
//! adopted silently, its mode narrowed to 0600 if it lets other users in. A file the vault
//! did not write stays the machine's own when adopted: it is kept out of the manifest, so
//! the vault never removes it, and a later value moves it aside like any file of its own.
//!
//! An ` if-absent` entry ([`Entry::if_absent`]) fills a gap: where it finds a file this
//! vault never wrote, or one lands there before its write (a gap is filled by a link that
//! refuses a taken name, never a rename over one), it leaves the file alone and out of the
//! manifest, so no later sync replaces or removes it. A file the vault wrote there itself
//! takes each new value while it still holds what the vault wrote; once anything else
//! changes it (git writes its config, gh its hosts.yml, the person their own) or a link or
//! a directory takes its place, it is the machine's own: left as it is, nothing moved
//! aside, and out of the manifest from then on. Deleted, it is a gap again.
//!
//! An anchored ` if-absent` entry ([`Entry::anchor`]) is the vault's on a machine only while
//! its anchor is: the anchor's file holds what the vault wrote, or is a gap this pass fills
//! ([`standings`]). Its gap is filled only then, and a file the vault wrote for it before
//! takes new values only then; once the anchor is the machine's own, that file is taken back
//! as a dropped entry's is (removed while it still holds what the vault wrote), so a file the
//! vault filled beside the machine's own before the entry was anchored is not kept there. So
//! a set of files that belong together (a release key and the profile naming it) lands whole
//! on a machine with none of them and never stays beside one of the machine's own.
//!
//! Whether two names are one file is decided by (device, inode), never by spelling: `Token`
//! and `token` are one file on a case-insensitive volume (APFS's default), where the second
//! name to land on it is refused, and two files on a case-sensitive one.

use crate::fsx::{self, Kind};
use crate::text::{self, Entry};
use crate::{Error, SecretBytes};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What the vault wrote: entry name → lowercase hex SHA-256 of the bytes written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Entry name → digest.
    pub files: BTreeMap<String, String>,
}

/// What one materialization did, by entry name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Written (new or changed).
    pub written: Vec<String>,
    /// Already held the value.
    pub unchanged: Vec<String>,
    /// Moved aside before being overwritten: name and where it went.
    pub moved_aside: Vec<(String, PathBuf)>,
    /// Removed because the vault dropped them, or because they are anchored ` if-absent`
    /// entries whose anchor is the machine's own.
    pub removed: Vec<String>,
    /// Left in place: dropped (or taken back) by the vault but changed since it wrote them,
    /// or not looked at (reached only through a symlinked or refused directory, or the file
    /// an entry behind one names; these stay in the manifest), or ` if-absent` entries whose
    /// target is the machine's own (a file the vault never wrote or that changed since it
    /// did, a link, a directory) or whose anchor is (a gap left unfilled), or cannot be
    /// looked at (a file the vault wrote, not written and still in the manifest).
    pub left: Vec<String>,
    /// Not written: the name and why (never the value).
    pub failed: Vec<(String, String)>,
}

/// The directory under `~/.secrets` that moved-aside files go to.
pub const REPLACED_DIR: &str = ".replaced";

/// The most numbered suffixes tried for a moved-aside name that is taken.
const MAX_ASIDE_SUFFIX: u32 = 1000;

/// The longest file name a Unix volume takes, in bytes (`NAME_MAX`).
const NAME_MAX: usize = 255;

/// The hex digits of the full name's digest a shortened moved-aside name carries.
const ASIDE_DIGEST: usize = 16;

/// Lowercase hex SHA-256 of `bytes`: a manifest digest.
pub fn digest_hex(bytes: &[u8]) -> String {
    let digest = aterm_digest::Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 15)]));
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Where entry `name` lands under `home`. Refuses an invalid name
/// ([`crate::text::valid_entry_name`]).
pub fn target_path(home: &Path, name: &str) -> crate::Result<PathBuf> {
    if !text::valid_entry_name(name) {
        return Err(Error::new(
            "an invalid entry name: a file name [A-Za-z0-9][A-Za-z0-9._-]{0,127} or a ~/ home path",
        ));
    }
    Ok(match name.strip_prefix("~/") {
        Some(rel) => rel
            .split('/')
            .fold(home.to_path_buf(), |path, component| path.join(component)),
        None => home.join(".secrets").join(name),
    })
}

/// The name a moved-aside copy of entry `name` gets (before any `.1`, `.2` suffix): the name
/// with each `/` spelled `%2F`, then `.<now>`.
///
/// A name that would leave no room within [`NAME_MAX`] for the longest numbered suffix is
/// cut, and `~` and the first 16 hex digits of the whole name's SHA-256 follow the cut, so
/// two long names that share their start still get two names. (An entry name holds `~`
/// only as its first byte, so a name that is not cut never meets one that is.)
pub fn aside_name(name: &str, now: u64) -> String {
    let flat = name.replace('/', "%2F");
    let stamp = format!(".{now}");
    let suffix = format!(".{}", MAX_ASIDE_SUFFIX - 1).len();
    let room = NAME_MAX - suffix - stamp.len();
    if flat.len() <= room {
        return format!("{flat}{stamp}");
    }
    let digest = digest_hex(name.as_bytes());
    let mut keep = room - 1 - ASIDE_DIGEST;
    // Entry names are ASCII; any other text is cut at a character boundary.
    while !flat.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}~{}{stamp}", &flat[..keep], &digest[..ASIDE_DIGEST])
}

/// Make the files under `home` match `entries` (already merged for this machine), given what
/// the vault wrote last time (`held`). Returns the new manifest and the report. `now` names
/// moved-aside copies. Missing directories of a home path are created 0700.
///
/// An entry that cannot be placed is reported failed with a reason naming the path (never
/// the value), and the rest proceed: a directory or other non-file at its target, a
/// symlinked or refused directory on its way (`~/.secrets` included), or a file another
/// entry of this run already placed or found (two names for one file). So the `Err` arm is
/// never taken today; it is kept for a refusal that would stop a whole sync.
///
/// The new manifest holds every entry written, or found holding its value where the vault
/// wrote it before (a file the vault never wrote that holds the value is adopted but stays
/// the machine's own, out of the manifest), and keeps `held`'s digest for an
/// entry that failed (so a later sync still knows what the vault wrote there); an
/// ` if-absent` entry left alone is not in it, nor is one whose file changed since the
/// vault wrote it. A name the vault dropped, or an anchored entry it takes back, leaves the
/// manifest once its file is removed or found changed; one that could not be looked at
/// (behind a symlinked or refused directory, the file such an entry names, an error) keeps
/// its digest, so the sync that can reach it still removes it, as does an anchored entry
/// whose anchor could not be. An entry whose anchor is not another ` if-absent` entry of
/// `entries` fails.
pub fn apply(
    home: &Path,
    entries: &[Entry],
    held: &Manifest,
    now: u64,
) -> crate::Result<(Manifest, Report)> {
    apply_resuming(
        home,
        entries,
        held,
        &Pending::new(),
        now,
        &mut |_, _| Ok(()),
    )
}

/// What a pass is to write, or what a pass cut short was writing: entry name → digest of the
/// value ([`apply_resuming`]).
pub type Pending = BTreeMap<String, String>;

/// [`apply`], resuming a pass cut short and saving ahead for this one.
///
/// A name of `resumed` (what the pass cut short was writing) whose file holds exactly that
/// value, private and reached without a symlinked directory or a refused `~/.secrets`, is
/// what the cut pass wrote: the vault's, as if `held` named it, so it takes the next value,
/// anchors as the vault's file, and is removed when the vault drops it or takes it back with
/// its anchor. Bytes a person put there since are judged the same way: they are the value the
/// vault held when the pass pended it.
///
/// Then, before any entry is written (only `~/.secrets` is made private first), `ahead` gets
/// that manifest and what this pass is to write: each entry whose value is not what the vault
/// last wrote under its name, at a target it can write (reached as above; a gap, a file, or
/// for a plain entry a link it moves aside), but an ` if-absent` one that does not stand as
/// the vault's, which it leaves alone. The same standings decide what it writes, and a plain
/// entry whose target changed since the look fails rather than be written unannounced, so it
/// writes only what `ahead` was told of and what the manifest it got already records. An
/// error of `ahead` stops the pass before any entry is written.
pub fn apply_resuming(
    home: &Path,
    entries: &[Entry],
    held: &Manifest,
    resumed: &Pending,
    now: u64,
    ahead: &mut dyn FnMut(&Manifest, &Pending) -> crate::Result<()>,
) -> crate::Result<(Manifest, Report)> {
    let secrets = home.join(".secrets");
    let places = Places::new(home, &secrets, now);
    let mut resumed_held = held.clone();
    for (name, digest) in resumed {
        if held.files.get(name) == Some(digest) || !places.reachable(name) {
            continue;
        }
        let wrote = target_path(home, name)
            .ok()
            .and_then(|target| fsx::read_private(&target, text::MAX_VALUE).ok().flatten())
            .is_some_and(|bytes| digest_hex(bytes.as_slice()) == *digest);
        if wrote {
            resumed_held.files.insert(name.clone(), digest.clone());
        }
    }
    let held = &resumed_held;
    let names: BTreeSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let history = history(home, &names, held);
    let by_name: BTreeMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    let standings = standings(&places, entries, held, &history);
    let pending = to_write(&places, entries, held, &history, &standings);
    ahead(held, &pending)?;
    let mut manifest = Manifest::default();
    let mut report = Report::default();
    // (device, inode) → the entry that placed or found that file this run.
    let mut claimed: BTreeMap<(u64, u64), String> = BTreeMap::new();
    // The files entries behind a symlinked or refused directory name (looked at through
    // it, never written): no entry's to claim, and never wiped through another name.
    let mut spared: BTreeSet<(u64, u64)> = BTreeSet::new();
    // Anchored entries whose anchor is the machine's own, with what the vault wrote for
    // each: taken back with the dropped names, once every entry has claimed its file.
    let mut withdrawn: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let mut held_digest = held.files.get(&entry.name);
        let placed = match target_path(home, &entry.name) {
            Err(error) => Err(error.0),
            // A vault text refuses this; a set merged from lockers is held to it here, so
            // nothing an anchor guards is ever written beside an anchor nobody can judge.
            Ok(_) if !text::anchor_received(entry, |name| by_name.get(name).copied()) => {
                Err(format!(
                    "its anchor `{}` is not an if-absent entry this machine receives",
                    entry.anchor.as_deref().unwrap_or_default()
                ))
            }
            Ok(target) => match places.prepare(&entry.name) {
                // Refused before any look at the target: nothing seen through a link
                // decides what this entry does.
                Err(why) => {
                    spared.extend(fsx::file_id(&target));
                    Err(why)
                }
                Ok(()) => {
                    if held_digest.is_none() {
                        held_digest =
                            fsx::file_id(&target).and_then(|id| history.get(&id).copied());
                    }
                    let digest = digest_hex(entry.value.as_slice());
                    // A plain entry `ahead` was not told of, whose target a write would now
                    // reach: a directory at it, or a symlinked one on its way, gave way since
                    // the look, and a cut before the save would leave what it wrote nobody's.
                    let unannounced = !entry.if_absent
                        && !pending.contains_key(&entry.name)
                        && held_digest != Some(&digest)
                        && matches!(
                            fsx::kind(&target),
                            Ok(Kind::Missing | Kind::File | Kind::Symlink)
                        );
                    // An ` if-absent` entry goes by its own standing, its anchors counted:
                    // the one `ahead` was told of.
                    let standing = standings
                        .get(entry.name.as_str())
                        .copied()
                        .unwrap_or(Standing::Unseen);
                    let placed = if unannounced && adoptable(&target, entry, held_digest) {
                        // The machine's own file already holds the value: nothing is
                        // written, so `ahead` was rightly not told of it.
                        match fsx::file_id(&target).and_then(|id| claimed.get(&id)) {
                            Some(other) => {
                                Err(format!("lands on the same file as entry `{other}`"))
                            }
                            None => narrow(&target).map(|()| Placed::Adopted),
                        }
                    } else if unannounced {
                        Err(format!(
                            "{}: changed while this pass looked at it; the next pass places it",
                            target.display()
                        ))
                    } else {
                        place(
                            &places,
                            entry,
                            &target,
                            held_digest,
                            standing,
                            &claimed,
                            &mut report,
                        )
                    };
                    if let Some(id) = fsx::file_id(&target) {
                        claimed.entry(id).or_insert_with(|| entry.name.clone());
                    }
                    placed
                }
            },
        };
        match placed {
            Ok(Placed::Written) => {
                manifest
                    .files
                    .insert(entry.name.clone(), digest_hex(entry.value.as_slice()));
                report.written.push(entry.name.clone());
            }
            Ok(Placed::Unchanged) => {
                manifest
                    .files
                    .insert(entry.name.clone(), digest_hex(entry.value.as_slice()));
                report.unchanged.push(entry.name.clone());
            }
            Ok(Placed::Adopted) => report.unchanged.push(entry.name.clone()),
            Ok(Placed::Left) => report.left.push(entry.name.clone()),
            Ok(Placed::Withdrawn) => {
                withdrawn.extend(held_digest.map(|digest| (entry.name.clone(), digest.clone())));
            }
            Ok(Placed::Held) => {
                if let Some(digest) = held_digest {
                    manifest.files.insert(entry.name.clone(), digest.clone());
                }
                report.left.push(entry.name.clone());
            }
            Err(why) => {
                if let Some(digest) = held_digest {
                    manifest.files.insert(entry.name.clone(), digest.clone());
                }
                report.failed.push((entry.name.clone(), why));
            }
        }
    }
    // What the vault takes back: each name it dropped, and each anchored entry it wrote
    // whose anchor is the machine's own.
    let dropped = held
        .files
        .iter()
        .filter(|(name, _)| !names.contains(name.as_str()));
    for (name, digest) in dropped.chain(withdrawn.iter().map(|(name, digest)| (name, digest))) {
        let Ok(target) = target_path(home, name) else {
            continue;
        };
        let id = fsx::file_id(&target);
        if id.is_some_and(|id| claimed.get(&id).is_some_and(|by| by != name)) {
            // Another entry lands on this file now: it is that entry's.
            continue;
        }
        if !places.reachable(name) || id.is_some_and(|id| spared.contains(&id)) {
            // Reached only through a symlinked or refused directory, or the file an entry
            // behind one names: never touched, and still what the vault wrote, so a sync
            // that reaches it directly still removes it.
            manifest.files.insert(name.clone(), digest.clone());
            report.left.push(name.clone());
            continue;
        }
        match fsx::kind(&target) {
            Ok(Kind::Missing) => {}
            Ok(Kind::File) => {
                if file_digest(&target).as_deref() == Some(digest.as_str()) {
                    match fsx::wipe_and_remove(&target) {
                        Ok(()) => report.removed.push(name.clone()),
                        Err(error) => {
                            manifest.files.insert(name.clone(), digest.clone());
                            report.failed.push((name.clone(), error.0));
                        }
                    }
                } else {
                    report.left.push(name.clone());
                }
            }
            Ok(Kind::Symlink | Kind::Dir | Kind::Other) => report.left.push(name.clone()),
            Err(error) => {
                manifest.files.insert(name.clone(), digest.clone());
                report.failed.push((name.clone(), error.0));
            }
        }
    }
    Ok((manifest, report))
}

/// What [`apply_resuming`] is to write, as it tells `ahead`, judged by `standings`.
fn to_write(
    places: &Places<'_>,
    entries: &[Entry],
    held: &Manifest,
    history: &BTreeMap<(u64, u64), &String>,
    standings: &BTreeMap<&str, Standing>,
) -> Pending {
    entries
        .iter()
        .filter(|entry| {
            let Ok(target) = target_path(places.home, &entry.name) else {
                return false;
            };
            if entry.if_absent {
                standings.get(entry.name.as_str()) == Some(&Standing::Vaults)
            } else {
                places.reachable(&entry.name)
                    && matches!(
                        fsx::kind(&target),
                        Ok(Kind::Missing | Kind::File | Kind::Symlink)
                    )
            }
        })
        .filter_map(|entry| {
            let digest = digest_hex(entry.value.as_slice());
            let target = target_path(places.home, &entry.name).ok();
            let wrote = held.files.get(&entry.name).or_else(|| {
                target
                    .as_deref()
                    .and_then(fsx::file_id)
                    .and_then(|id| history.get(&id).copied())
            });
            // A file of the machine's own that already holds the value is adopted, never
            // written: told to `ahead`, a pass cut short would count it the vault's.
            let adopted = target
                .as_deref()
                .is_some_and(|target| adoptable(target, entry, wrote));
            (wrote != Some(&digest) && !adopted).then(|| (entry.name.clone(), digest))
        })
        .collect()
}

/// What the vault wrote under a name of `held` that is none of `names` now, by the file it
/// wrote: an entry respelled by case lands on that file on a case-insensitive volume, and
/// keeps its history there.
fn history<'m>(
    home: &Path,
    names: &BTreeSet<&str>,
    held: &'m Manifest,
) -> BTreeMap<(u64, u64), &'m String> {
    let mut history = BTreeMap::new();
    for (name, digest) in &held.files {
        if names.contains(name.as_str()) {
            continue;
        }
        if let Some(id) = target_path(home, name)
            .ok()
            .and_then(|target| fsx::file_id(&target))
        {
            history.entry(id).or_insert(digest);
        }
    }
    history
}

/// Where entries land, and whether `~/.secrets` may be written to.
struct Places<'a> {
    home: &'a Path,
    secrets: &'a Path,
    /// `~/.secrets` made private, or why it was refused (a symlink, not a directory, another
    /// user's): the reason every entry that lands there, and every move aside, fails with.
    secrets_ok: Result<(), String>,
    /// What moved-aside copies are named by ([`aside_name`]).
    now: u64,
}

impl<'a> Places<'a> {
    /// The places under `home`, with `~/.secrets` (`secrets`) made private.
    fn new(home: &'a Path, secrets: &'a Path, now: u64) -> Places<'a> {
        Places {
            home,
            secrets,
            secrets_ok: fsx::ensure_private_dir(secrets).map_err(|error| error.0),
            now,
        }
    }

    /// Make entry `name`'s directory ready to write in: `~/.secrets` for a file name, the
    /// directories between home and the file for a home path ([`make_parents`]).
    fn prepare(&self, name: &str) -> Result<(), String> {
        match name.strip_prefix("~/") {
            Some(rel) => make_parents(self.home, rel),
            None => self.secrets_ok.clone(),
        }
    }

    /// Whether entry `name`'s file is reached without a symlinked directory or a refused
    /// `~/.secrets` on the way.
    fn reachable(&self, name: &str) -> bool {
        match name.strip_prefix("~/") {
            Some(rel) => symlinked_dir(self.home, rel).is_none(),
            None => self.secrets_ok.is_ok(),
        }
    }
}

/// What placing one entry did.
enum Placed {
    Written,
    Unchanged,
    /// A plain entry whose target, a file the vault never wrote, already held its value:
    /// narrowed and kept, and still the machine's own (out of the manifest), so dropping the
    /// entry leaves it and a new value moves it aside first.
    Adopted,
    /// An ` if-absent` entry whose target, or anchor, is the machine's own.
    Left,
    /// An anchored ` if-absent` entry whose file the vault wrote and whose anchor is the
    /// machine's own: taken back, as a dropped entry is.
    Withdrawn,
    /// An anchored ` if-absent` entry whose file the vault wrote and whose anchor could not
    /// be looked at: left as it is, and still the vault's.
    Held,
}

/// How an ` if-absent` entry's file stands, with whatever it is anchored to ([`standings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// The vault's: a gap, or a file still holding what the vault wrote. As an anchor, it
    /// lets the entries anchored to it fill their gaps and update what the vault wrote.
    Vaults,
    /// The machine's own: a file the vault never wrote or that changed since, a link, a
    /// directory. As an anchor, it keeps the vault's entries anchored to it away.
    Own,
    /// Not looked at (seen only through a symlinked or refused directory, or an error):
    /// as an anchor, it lets nothing be written and takes nothing back.
    Unseen,
}

/// What an existing regular file holds, against the new value and the manifest.
enum Judged {
    /// The new value already.
    Same,
    /// What the vault last wrote (or nothing any more): overwrite it.
    Ours,
    /// Something the vault did not write, or edited since, or unreadable: move it aside.
    Foreign,
}

/// Place one entry at `target`, whose directory [`Places::prepare`] made ready; `Err` is
/// the reason it was not written (a path and an OS error at most, never a value). A move
/// aside is reported as it happens, even if the write then fails. `standing` is how an
/// ` if-absent` entry stands, its anchors counted ([`standings`]).
fn place(
    places: &Places<'_>,
    entry: &Entry,
    target: &Path,
    held: Option<&String>,
    standing: Standing,
    claimed: &BTreeMap<(u64, u64), String>,
    report: &mut Report,
) -> Result<Placed, String> {
    if let Some(other) = fsx::file_id(target).and_then(|id| claimed.get(&id)) {
        return Err(format!("lands on the same file as entry `{other}`"));
    }
    let value = entry.value.as_slice();
    let kind = fsx::kind(target).map_err(|error| error.0)?;
    if entry.if_absent {
        return fill(target, kind, value, held, standing);
    }
    let aside = match kind {
        Kind::Missing => false,
        Kind::Dir => return Err(format!("{}: a directory is in the way", target.display())),
        Kind::Other => return Err(format!("{}: not a regular file", target.display())),
        Kind::Symlink => true,
        Kind::File => match judge(target, value, held) {
            Judged::Same => {
                narrow(target)?;
                // Found holding the value where the vault never wrote: the machine's own.
                return Ok(if held.is_some() {
                    Placed::Unchanged
                } else {
                    Placed::Adopted
                });
            }
            Judged::Ours => false,
            Judged::Foreign => true,
        },
    };
    if aside {
        let to = move_aside(places, &entry.name, target)?;
        report.moved_aside.push((entry.name.clone(), to));
    }
    fsx::write_atomic(target, value, 0o600).map_err(|error| error.0)?;
    Ok(Placed::Written)
}

/// Place an ` if-absent` entry at `target`, which holds `kind`, given how it stands
/// ([`standings`]: its own file, then its anchors). While it stands as the vault's, a gap is
/// filled and a file that still holds what the vault wrote there (`held`) takes the new
/// value. Once its anchor is the machine's own, a gap stays one and such a file is
/// [`Placed::Withdrawn`]; while the anchor cannot be looked at, neither is written.
/// Anything else is this machine's own, never replaced, moved or recorded: a file the vault
/// never wrote, one changed since it wrote it (git and gh rewrite their own files, and a
/// person edits theirs), a link, a directory, and a gap that was a file when the standings
/// were judged.
fn fill(
    target: &Path,
    kind: Kind,
    value: &[u8],
    held: Option<&String>,
    standing: Standing,
) -> Result<Placed, String> {
    match kind {
        Kind::Missing => {
            // A gap is filled only while it is one: whatever lands there before the write
            // is this machine's own.
            let filled = standing == Standing::Vaults
                && fsx::write_new(target, value, 0o600).map_err(|error| error.0)?;
            Ok(if filled {
                Placed::Written
            } else {
                Placed::Left
            })
        }
        Kind::File if held.is_some() => match (judge(target, value, held), standing) {
            // Changed since the vault wrote it: the machine's own, whatever its anchor.
            (Judged::Foreign, _) => Ok(Placed::Left),
            (Judged::Same, Standing::Vaults) => {
                narrow(target)?;
                Ok(Placed::Unchanged)
            }
            (Judged::Ours, Standing::Vaults) => {
                fsx::write_atomic(target, value, 0o600).map_err(|error| error.0)?;
                Ok(Placed::Written)
            }
            (Judged::Same | Judged::Ours, Standing::Own) => Ok(Placed::Withdrawn),
            (Judged::Same | Judged::Ours, Standing::Unseen) => Ok(Placed::Held),
        },
        Kind::File | Kind::Symlink | Kind::Dir | Kind::Other => Ok(Placed::Left),
    }
}

/// How each ` if-absent` entry of `entries` stands, its anchors counted: an anchored entry
/// goes by its anchor's standing ([`apply`]). Judged from the disk and `held` alone, before
/// anything is written, so no verdict depends on the order the pass writes in.
///
/// Each entry starts from its own file: a gap, or a file still holding what the vault wrote
/// (`held`, or the `history` of a name respelled by case), is the vault's; a file the vault
/// never wrote or that changed since, a link or a directory is the machine's own; and one
/// seen only through a symlinked or refused directory is unseen, since nothing seen through
/// a link decides what another entry does. Then each entry that is the vault's but whose
/// anchor is not takes its anchor's standing, until none changes. So two entries anchored
/// to each other both fill a machine that has neither, and neither fills or stays beside
/// the other's own file; an entry anchored to a gap that its own anchor keeps unfilled
/// stays unfilled too.
fn standings<'a>(
    places: &Places<'_>,
    entries: &'a [Entry],
    held: &Manifest,
    history: &BTreeMap<(u64, u64), &String>,
) -> BTreeMap<&'a str, Standing> {
    let mut standings: BTreeMap<&str, Standing> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.if_absent) {
        let standing = match target_path(places.home, &entry.name) {
            Ok(target) if places.reachable(&entry.name) => match fsx::kind(&target) {
                Ok(Kind::Missing) => Standing::Vaults,
                Ok(Kind::File) => {
                    let wrote = held
                        .files
                        .get(&entry.name)
                        .or_else(|| fsx::file_id(&target).and_then(|id| history.get(&id).copied()));
                    // A file the vault wrote that already holds the entry's value is the
                    // vault's too, as `fill` judges it: a pass cut short after it wrote the
                    // files but before it saved their digests leaves the new value under the
                    // old digest.
                    if wrote.is_some_and(|digest| {
                        file_digest(&target).as_deref() == Some(digest.as_str())
                    }) || (wrote.is_some() && holds(&target, entry.value.as_slice()))
                    {
                        Standing::Vaults
                    } else {
                        Standing::Own
                    }
                }
                Ok(Kind::Symlink | Kind::Dir | Kind::Other) => Standing::Own,
                Err(_) => Standing::Unseen,
            },
            _ => Standing::Unseen,
        };
        standings.insert(&entry.name, standing);
    }
    let anchors: Vec<(&str, &str)> = entries
        .iter()
        .filter(|entry| entry.if_absent)
        .filter_map(|entry| Some((entry.name.as_str(), entry.anchor.as_deref()?)))
        .collect();
    // A standing other than the vault's never changes, so each entry falls at most once,
    // and with its anchor's final standing.
    loop {
        let fallen: Vec<(&str, Standing)> = anchors
            .iter()
            .filter(|(name, _)| standings.get(name) == Some(&Standing::Vaults))
            .filter_map(|&(name, anchor)| {
                let of = standings.get(anchor).copied().unwrap_or(Standing::Unseen);
                (of != Standing::Vaults).then_some((name, of))
            })
            .collect();
        if fallen.is_empty() {
            return standings;
        }
        for (name, of) in fallen {
            standings.insert(name, of);
        }
    }
}

/// Whether a plain entry's target is a regular file the vault never wrote (`wrote` is
/// `None`) that already holds the entry's value: adopted, never written.
fn adoptable(target: &Path, entry: &Entry, wrote: Option<&String>) -> bool {
    !entry.if_absent
        && wrote.is_none()
        && matches!(fsx::kind(target), Ok(Kind::File))
        && holds(target, entry.value.as_slice())
}

/// Whether the regular file at `target` holds `value`.
fn holds(target: &Path, value: &[u8]) -> bool {
    fsx::read_public(target, text::MAX_VALUE)
        .ok()
        .flatten()
        .is_some_and(|bytes| aterm_digest::ct_eq(SecretBytes::new(bytes).as_slice(), value))
}

/// Judge the regular file at `target`.
fn judge(target: &Path, value: &[u8], held: Option<&String>) -> Judged {
    let len = std::fs::symlink_metadata(target).map_or(u64::MAX, |meta| meta.len());
    if len > u64::try_from(text::MAX_VALUE).unwrap_or(u64::MAX) {
        // Larger than any value the vault writes: neither the new value nor ours.
        return Judged::Foreign;
    }
    match fsx::read_public(target, text::MAX_VALUE) {
        Ok(Some(bytes)) => {
            let current = SecretBytes::new(bytes);
            if aterm_digest::ct_eq(current.as_slice(), value) {
                Judged::Same
            } else if held.is_some_and(|digest| *digest == digest_hex(current.as_slice())) {
                Judged::Ours
            } else {
                Judged::Foreign
            }
        }
        // Gone since the look: nothing to keep.
        Ok(None) => Judged::Ours,
        Err(_) => Judged::Foreign,
    }
}

/// The digest of the regular file at `target`, if it can be read and is not larger than a
/// value.
fn file_digest(target: &Path) -> Option<String> {
    let bytes = fsx::read_public(target, text::MAX_VALUE).ok()??;
    let bytes = SecretBytes::new(bytes);
    Some(digest_hex(bytes.as_slice()))
}

/// Narrow an adopted file this user owns to 0600 when group or other bits are set.
fn narrow(target: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(target)
        .map_err(|error| format!("{}: {error}", target.display()))?;
    if meta.uid() == fsx::euid() && meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("{}: {error}", target.display()))?;
    }
    Ok(())
}

/// The directories between home and home path `rel`'s file: `a/b/c` → `~/a`, `~/a/b`.
fn parents(home: &Path, rel: &str) -> Vec<PathBuf> {
    let components: Vec<&str> = rel.split('/').collect();
    let mut dir = home.to_path_buf();
    components[..components.len().saturating_sub(1)]
        .iter()
        .map(|component| {
            dir.push(component);
            dir.clone()
        })
        .collect()
}

/// Create the missing directories between home and home path `rel`'s file at 0700. An
/// existing directory is used as it is and never chmodded; a symlink in a directory's place
/// is refused, never followed, as is anything else that is not a directory.
fn make_parents(home: &Path, rel: &str) -> Result<(), String> {
    for dir in parents(home, rel) {
        match fsx::kind(&dir).map_err(|error| error.0)? {
            Kind::Dir => {}
            Kind::Missing => match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                // Made meanwhile: a directory will do, a link to one will not.
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && fsx::kind(&dir).is_ok_and(|kind| kind == Kind::Dir) => {}
                Err(error) => return Err(format!("{}: {error}", dir.display())),
            },
            Kind::Symlink => {
                return Err(format!(
                    "{}: is a symlink; refusing to write through it",
                    dir.display()
                ));
            }
            Kind::File | Kind::Other => {
                return Err(format!("{}: not a directory", dir.display()));
            }
        }
    }
    Ok(())
}

/// The first directory between home and home path `rel`'s file that is a symlink, if any.
/// The look stops at one that is missing or not a directory: nothing past it is reached.
fn symlinked_dir(home: &Path, rel: &str) -> Option<PathBuf> {
    for dir in parents(home, rel) {
        match fsx::kind(&dir) {
            Ok(Kind::Dir) => {}
            Ok(Kind::Symlink) => return Some(dir),
            Ok(Kind::Missing | Kind::File | Kind::Other) | Err(_) => return None,
        }
    }
    None
}

/// Move `target` to `~/.secrets/.replaced/<`[`aside_name`]`>` (or that with `.1`, `.2`, …
/// when taken), never replacing anything there. A symlink moves as itself.
fn move_aside(places: &Places<'_>, name: &str, target: &Path) -> Result<PathBuf, String> {
    places.secrets_ok.clone()?;
    let dir = places.secrets.join(REPLACED_DIR);
    fsx::ensure_private_dir(&dir).map_err(|error| error.0)?;
    let base = aside_name(name, places.now);
    for k in 0..MAX_ASIDE_SUFFIX {
        let to = if k == 0 {
            dir.join(&base)
        } else {
            dir.join(format!("{base}.{k}"))
        };
        if fsx::kind(&to).map_err(|error| error.0)? != Kind::Missing {
            continue;
        }
        let moving = fsx::file_id(target);
        match fsx::rename_noreplace(target, &to) {
            Ok(()) => return Ok(to),
            // Another file took the name between the look and the link: try the next one.
            Err(_) if fsx::file_id(&to).is_some_and(|id| Some(id) != moving) => {}
            Err(error) => return Err(error.0),
        }
    }
    Err(format!(
        "{}: no free name for a moved-aside copy",
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Audience;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("tempdir")
    }

    fn entry(name: &str, value: &[u8]) -> Entry {
        Entry {
            name: name.to_string(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).expect("meta").mode() & 0o7777
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).expect("read")
    }

    fn plant(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::write(path, bytes).expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn target_paths() {
        let home = Path::new("/h");
        assert_eq!(
            target_path(home, "gh_token").expect("ok"),
            Path::new("/h/.secrets/gh_token")
        );
        assert_eq!(
            target_path(home, "~/.aterm/release-credentials.toml").expect("ok"),
            Path::new("/h/.aterm/release-credentials.toml")
        );
        assert_eq!(target_path(home, "~/x").expect("ok"), Path::new("/h/x"));
        for bad in [
            "",
            "../x",
            "a/b",
            "~/../x",
            "~/a/../../x",
            "/etc/passwd",
            "~/.secrets/x",
            ".x",
            "~/",
        ] {
            assert!(target_path(home, bad).is_err(), "{bad}");
        }
        assert_eq!(aside_name("~/.aterm/x.toml", 9), "~%2F.aterm%2Fx.toml.9");
    }

    #[test]
    fn digests_are_lowercase_hex_sha256() {
        assert_eq!(
            digest_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_first_sync_writes_0600_files_and_narrows_the_directory() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o755)).expect("widen");
        let entries = [entry("a", b"one"), entry("b", b""), entry("c", &[0, 1, 2])];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
        assert_eq!(mode(&secrets), 0o700);
        assert_eq!(report.written, ["a", "b", "c"]);
        assert_eq!(
            report,
            Report {
                written: report.written.clone(),
                ..Report::default()
            }
        );
        for e in &entries {
            let path = secrets.join(&e.name);
            assert_eq!(read(&path), e.value.as_slice());
            assert_eq!(mode(&path), 0o600);
            assert_eq!(manifest.files[&e.name], digest_hex(e.value.as_slice()));
        }
        let (again, report) = apply(home.path(), &entries, &manifest, 2).expect("again");
        assert_eq!(again, manifest);
        assert_eq!(report.unchanged, ["a", "b", "c"]);
        assert!(report.written.is_empty() && report.moved_aside.is_empty());
    }

    #[test]
    fn a_pre_existing_different_file_is_moved_aside_with_its_content() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("gh_token"), b"the old hand-made token", 0o644);
        let (manifest, report) = apply(
            home.path(),
            &[entry("gh_token", b"vault token")],
            &Manifest::default(),
            1700,
        )
        .expect("applied");
        let aside = secrets.join(".replaced").join("gh_token.1700");
        assert_eq!(
            report.moved_aside,
            [("gh_token".to_string(), aside.clone())]
        );
        assert_eq!(report.written, ["gh_token"]);
        assert_eq!(read(&aside), b"the old hand-made token");
        assert_eq!(mode(&secrets.join(".replaced")), 0o700);
        assert_eq!(read(&secrets.join("gh_token")), b"vault token");
        assert_eq!(mode(&secrets.join("gh_token")), 0o600);
        assert_eq!(manifest.files["gh_token"], digest_hex(b"vault token"));
    }

    #[test]
    fn an_equal_file_is_adopted_and_narrowed() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("t"), b"same", 0o644);
        let (manifest, report) =
            apply(home.path(), &[entry("t", b"same")], &Manifest::default(), 1).expect("applied");
        assert_eq!(report.unchanged, ["t"]);
        assert!(report.written.is_empty() && report.moved_aside.is_empty());
        assert!(
            !manifest.files.contains_key("t"),
            "an adopted file stays the machine's own"
        );
        assert_eq!(mode(&secrets.join("t")), 0o600);
        assert!(!secrets.join(".replaced").exists());
        // Every later pass adopts it again, and fails nothing.
        let (manifest, report) =
            apply(home.path(), &[entry("t", b"same")], &manifest, 2).expect("again");
        assert_eq!(report.unchanged, ["t"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn dropping_an_adopted_entry_leaves_the_machines_own_file() {
        // The audit's case: `set ~/.aws/credentials --file ~/.aws/credentials`, then unset.
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("token"), b"hand-made", 0o600);
        let entries = [entry("token", b"hand-made"), entry("other", b"o")];
        let (held, _) = apply(home.path(), &entries, &Manifest::default(), 1).expect("first");
        let (manifest, report) = apply(home.path(), &entries[1..], &held, 2).expect("dropped");
        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert_eq!(read(&secrets.join("token")), b"hand-made");
        assert!(!manifest.files.contains_key("token"));
        // NEGATIVE CONTROL: what the vault wrote itself is still removed when dropped.
        let (_, report) = apply(home.path(), &[], &manifest, 3).expect("dropped");
        assert_eq!(report.removed, ["other"]);
        assert!(!secrets.join("other").exists());
        assert_eq!(read(&secrets.join("token")), b"hand-made");
    }

    #[test]
    fn a_new_value_moves_an_adopted_file_aside_first() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("token"), b"hand-made", 0o600);
        let (held, _) = apply(
            home.path(),
            &[entry("token", b"hand-made")],
            &Manifest::default(),
            1,
        )
        .expect("adopted");
        let (manifest, report) =
            apply(home.path(), &[entry("token", b"rotated")], &held, 2).expect("changed");
        let [(name, aside)] = report.moved_aside.as_slice() else {
            panic!("moved aside: {:?}", report.moved_aside);
        };
        assert_eq!(name, "token");
        assert_eq!(read(aside), b"hand-made");
        assert_eq!(read(&secrets.join("token")), b"rotated");
        assert_eq!(manifest.files["token"], digest_hex(b"rotated"));
    }

    #[test]
    fn an_adoptable_file_is_never_announced_as_a_write() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("token"), b"hand-made", 0o600);
        let mut told = Vec::new();
        let (_, report) = apply_resuming(
            home.path(),
            &[entry("token", b"hand-made"), entry("new", b"n")],
            &Manifest::default(),
            &Pending::new(),
            1,
            &mut |_, pending| {
                told.extend(pending.keys().cloned());
                Ok(())
            },
        )
        .expect("applied");
        assert_eq!(
            told,
            ["new"],
            "a cut pass must not count the adopted file the vault's"
        );
        assert_eq!(report.unchanged, ["token"]);
        assert_eq!(report.written, ["new"]);
    }

    #[test]
    fn a_change_overwrites_what_the_vault_wrote_but_saves_a_local_edit() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        let (held, _) = apply(
            home.path(),
            &[entry("a", b"v1"), entry("b", b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("first");
        plant(&secrets.join("b"), b"edited by hand", 0o600);
        let (manifest, report) = apply(
            home.path(),
            &[entry("a", b"v2"), entry("b", b"v2")],
            &held,
            2,
        )
        .expect("second");
        assert_eq!(report.written, ["a", "b"]);
        assert_eq!(
            report.moved_aside,
            [("b".to_string(), secrets.join(".replaced").join("b.2"))]
        );
        assert_eq!(
            read(&secrets.join(".replaced").join("b.2")),
            b"edited by hand"
        );
        assert_eq!(read(&secrets.join("a")), b"v2");
        assert_eq!(read(&secrets.join("b")), b"v2");
        assert_eq!(manifest.files["a"], digest_hex(b"v2"));
    }

    #[test]
    fn a_taken_aside_name_gets_a_numbered_suffix() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("t"), b"first", 0o600);
        apply(home.path(), &[entry("t", b"v")], &Manifest::default(), 5).expect("first");
        plant(&secrets.join("t"), b"second", 0o600);
        let (_, report) =
            apply(home.path(), &[entry("t", b"w")], &Manifest::default(), 5).expect("second");
        let replaced = secrets.join(".replaced");
        assert_eq!(
            report.moved_aside,
            [("t".to_string(), replaced.join("t.5.1"))]
        );
        assert_eq!(read(&replaced.join("t.5")), b"first");
        assert_eq!(read(&replaced.join("t.5.1")), b"second");
    }

    #[test]
    fn a_dropped_entry_is_removed_only_while_unchanged() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        let entries = [
            entry("keep", b"k"),
            entry("drop", b"d"),
            entry("edited", b"e"),
            entry("gone", b"g"),
        ];
        let (held, _) = apply(home.path(), &entries, &Manifest::default(), 1).expect("first");
        plant(&secrets.join("edited"), b"local edit", 0o600);
        std::fs::remove_file(secrets.join("gone")).expect("rm");
        let (manifest, report) = apply(home.path(), &entries[..1], &held, 2).expect("second");
        assert_eq!(report.removed, ["drop"]);
        assert_eq!(report.left, ["edited"]);
        assert_eq!(report.unchanged, ["keep"]);
        assert!(!secrets.join("drop").exists());
        assert_eq!(read(&secrets.join("edited")), b"local edit");
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["keep"]);
        let (_, report) = apply(home.path(), &entries[..1], &manifest, 3).expect("third");
        assert!(report.left.is_empty() && report.removed.is_empty());
        assert_eq!(
            read(&secrets.join("edited")),
            b"local edit",
            "a file the vault let go of stays"
        );
    }

    #[test]
    fn a_file_the_vault_never_wrote_is_never_removed() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        plant(&secrets.join("mine"), b"hand-made", 0o600);
        let (_, report) = apply(home.path(), &[], &Manifest::default(), 1).expect("applied");
        assert_eq!(report, Report::default());
        assert_eq!(read(&secrets.join("mine")), b"hand-made");
    }

    #[test]
    fn a_home_path_target_gets_private_parents() {
        let home = scratch();
        let name = "~/.aterm/deep/x.toml";
        let (manifest, report) = apply(
            home.path(),
            &[entry(name, b"k = 1\n")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        assert_eq!(report.written, [name]);
        let target = home.path().join(".aterm/deep/x.toml");
        assert_eq!(read(&target), b"k = 1\n");
        assert_eq!(mode(&target), 0o600);
        assert_eq!(mode(&home.path().join(".aterm")), 0o700);
        assert_eq!(mode(&home.path().join(".aterm/deep")), 0o700);
        assert_eq!(manifest.files[name], digest_hex(b"k = 1\n"));

        let other = scratch();
        let existing = other.path().join(".aterm");
        std::fs::create_dir(&existing).expect("dir");
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        plant(&existing.join("x.toml"), b"old", 0o644);
        let (_, report) = apply(
            other.path(),
            &[entry("~/.aterm/x.toml", b"new")],
            &Manifest::default(),
            3,
        )
        .expect("applied");
        assert_eq!(mode(&existing), 0o755, "an existing parent keeps its mode");
        let aside = other
            .path()
            .join(".secrets/.replaced/~%2F.aterm%2Fx.toml.3");
        assert_eq!(
            report.moved_aside,
            [("~/.aterm/x.toml".to_string(), aside.clone())]
        );
        assert_eq!(read(&aside), b"old");
        assert_eq!(read(&existing.join("x.toml")), b"new");
    }

    #[test]
    fn a_symlink_at_the_target_is_replaced_never_followed() {
        let home = scratch();
        let outside = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        let elsewhere = outside.path().join("elsewhere");
        plant(&elsewhere, b"not the vault's", 0o644);
        std::os::unix::fs::symlink(&elsewhere, secrets.join("t")).expect("link");
        let (_, report) = apply(
            home.path(),
            &[entry("t", b"vault value")],
            &Manifest::default(),
            4,
        )
        .expect("applied");
        assert_eq!(read(&elsewhere), b"not the vault's");
        assert_eq!(mode(&elsewhere), 0o644);
        let target = secrets.join("t");
        assert_eq!(fsx::kind(&target).expect("kind"), Kind::File);
        assert_eq!(read(&target), b"vault value");
        assert_eq!(mode(&target), 0o600);
        let aside = secrets.join(".replaced/t.4");
        assert_eq!(report.moved_aside, [("t".to_string(), aside.clone())]);
        assert_eq!(fsx::kind(&aside).expect("kind"), Kind::Symlink);
        assert_eq!(std::fs::read_link(&aside).expect("link"), elsewhere);

        // A link whose name the vault dropped is left alone.
        let held = Manifest {
            files: BTreeMap::from([("t".to_string(), digest_hex(b"vault value"))]),
        };
        std::fs::remove_file(&target).expect("rm");
        std::os::unix::fs::symlink(&elsewhere, &target).expect("link");
        let (manifest, report) = apply(home.path(), &[], &held, 5).expect("applied");
        assert_eq!(report.left, ["t"]);
        assert!(manifest.files.is_empty());
        assert_eq!(fsx::kind(&target).expect("kind"), Kind::Symlink);
        assert_eq!(read(&elsewhere), b"not the vault's");
    }

    #[test]
    fn a_directory_in_the_way_fails_that_entry_only() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir_all(secrets.join("dir")).expect("dir");
        std::fs::write(home.path().join("blocker"), b"x").expect("file");
        let held = Manifest {
            files: BTreeMap::from([("dir".to_string(), digest_hex(b"old"))]),
        };
        let entries = [
            entry("dir", b"v"),
            entry("ok", b"v"),
            entry("~/blocker/x", b"v"),
        ];
        let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
        assert_eq!(report.written, ["ok"]);
        let failed: Vec<&str> = report
            .failed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(failed, ["dir", "~/blocker/x"]);
        assert!(
            report.failed[0].1.contains("directory"),
            "{}",
            report.failed[0].1
        );
        assert!(
            report.failed[1].1.contains("not a directory"),
            "{}",
            report.failed[1].1
        );
        assert!(secrets.join("dir").is_dir());
        assert_eq!(
            manifest.files["dir"],
            digest_hex(b"old"),
            "a failed entry keeps what the vault wrote"
        );
        assert!(!manifest.files.contains_key("~/blocker/x"));
    }

    /// Whether `dir`'s volume folds case (APFS's default does; most Linux volumes do not).
    fn folds_case(dir: &Path) -> bool {
        let probe = dir.join("CaseProbe");
        std::fs::write(&probe, b"").expect("probe");
        let folds = fsx::file_id(&dir.join("caseprobe")) == fsx::file_id(&probe);
        std::fs::remove_file(&probe).expect("rm probe");
        folds
    }

    fn failed_names(report: &Report) -> Vec<&str> {
        report
            .failed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    #[test]
    fn two_names_for_one_file_are_refused_where_the_volume_makes_them_one() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        let (manifest, report) = apply(
            home.path(),
            &[entry("Tok", b"1"), entry("tok", b"2")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        if folds_case(&secrets) {
            assert_eq!(report.written, ["Tok"]);
            assert_eq!(failed_names(&report), ["tok"]);
            assert!(report.failed[0].1.contains("`Tok`"), "{:?}", report.failed);
            assert_eq!(read(&secrets.join("Tok")), b"1");
            assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["Tok"]);
        } else {
            assert_eq!(report.written, ["Tok", "tok"]);
            assert!(report.failed.is_empty());
            assert_eq!(read(&secrets.join("Tok")), b"1");
            assert_eq!(read(&secrets.join("tok")), b"2");
        }
    }

    #[test]
    fn a_second_name_for_a_placed_file_is_refused_whatever_its_spelling() {
        // A hard link is two spellings of one file on every volume.
        let home = scratch();
        let first = home.path().join("first");
        plant(&first, b"v", 0o600);
        std::fs::hard_link(&first, home.path().join("second")).expect("link");
        let (manifest, report) = apply(
            home.path(),
            &[entry("~/first", b"v"), entry("~/second", b"w")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        assert_eq!(report.unchanged, ["~/first"]);
        assert_eq!(failed_names(&report), ["~/second"]);
        assert!(
            report.failed[0].1.contains("`~/first`"),
            "{:?}",
            report.failed
        );
        assert!(report.moved_aside.is_empty());
        assert_eq!(read(&first), b"v", "never written through the second name");
        // Found holding its value, `~/first` is adopted: still the machine's own.
        assert!(manifest.files.is_empty(), "{:?}", manifest.files);
    }

    #[test]
    fn a_dropped_name_for_another_entrys_file_is_never_wiped() {
        // `~/old` and `~/new` are one file; the vault renamed the entry. Wiping `~/old`
        // would zero the bytes `~/new` holds.
        let home = scratch();
        let old = home.path().join("old");
        plant(&old, b"v", 0o600);
        let new = home.path().join("new");
        std::fs::hard_link(&old, &new).expect("link");
        let held = Manifest {
            files: BTreeMap::from([("~/old".to_string(), digest_hex(b"v"))]),
        };
        let (manifest, report) =
            apply(home.path(), &[entry("~/new", b"v")], &held, 2).expect("applied");
        assert_eq!(report.unchanged, ["~/new"]);
        assert!(report.removed.is_empty() && report.left.is_empty());
        assert_eq!(read(&new), b"v");
        assert_eq!(read(&old), b"v");
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["~/new"]);

        // A hard link no entry names is no copy: a dropped file's bytes go from every name
        // (another user who can search the directory may have made one to keep them).
        let kept = home.path().join("kept");
        std::fs::hard_link(&new, &kept).expect("link");
        let (_, report) = apply(home.path(), &[], &manifest, 3).expect("dropped");
        assert_eq!(report.removed, ["~/new"]);
        assert!(!new.exists());
        assert_eq!(read(&kept), [0u8]);
    }

    #[test]
    fn an_entry_respelled_by_case_keeps_what_the_vault_wrote() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        let (held, _) = apply(
            home.path(),
            &[entry("Token", b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("first");
        let (manifest, report) =
            apply(home.path(), &[entry("token", b"v2")], &held, 2).expect("respelled");
        assert_eq!(report.written, ["token"]);
        assert!(report.failed.is_empty());
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["token"]);
        assert_eq!(read(&secrets.join("token")), b"v2");
        if folds_case(&secrets) {
            // One file: the vault's own bytes, overwritten in place, nothing moved aside.
            assert!(report.moved_aside.is_empty(), "{:?}", report.moved_aside);
            assert!(report.removed.is_empty());
        } else {
            // Two files: the old one, still as the vault wrote it, goes.
            assert_eq!(report.removed, ["Token"]);
            assert!(!secrets.join("Token").exists());
        }
    }

    fn gap(name: &str, value: &[u8]) -> Entry {
        Entry {
            if_absent: true,
            ..entry(name, value)
        }
    }

    #[test]
    fn an_if_absent_entry_fills_a_gap_but_never_touches_a_machines_own_file() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        let aterm = home.path().join(".aterm");
        std::fs::create_dir(&aterm).expect("dir");
        plant(&aterm.join("machine.key"), b"this machine's own", 0o644);
        plant(&secrets.join("same"), b"v", 0o644);
        let elsewhere = home.path().join("elsewhere");
        plant(&elsewhere, b"linked", 0o600);
        std::os::unix::fs::symlink(&elsewhere, secrets.join("link")).expect("link");
        std::fs::create_dir(secrets.join("dir")).expect("dir");
        let entries = [
            gap("~/.aterm/machine.key", b"the vault's"),
            gap("~/.aterm/missing.key", b"filled"),
            gap("same", b"v"),
            gap("link", b"v"),
            gap("dir", b"v"),
        ];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 3).expect("applied");
        assert_eq!(report.written, ["~/.aterm/missing.key"]);
        assert_eq!(report.left, ["~/.aterm/machine.key", "same", "link", "dir"]);
        assert!(report.moved_aside.is_empty() && report.failed.is_empty());
        assert_eq!(read(&aterm.join("machine.key")), b"this machine's own");
        assert_eq!(mode(&aterm.join("machine.key")), 0o644, "not even narrowed");
        assert_eq!(
            mode(&secrets.join("same")),
            0o644,
            "an equal file is not adopted"
        );
        assert_eq!(
            fsx::kind(&secrets.join("link")).expect("kind"),
            Kind::Symlink
        );
        assert_eq!(read(&elsewhere), b"linked");
        assert_eq!(read(&aterm.join("missing.key")), b"filled");
        assert_eq!(mode(&aterm.join("missing.key")), 0o600);
        assert_eq!(
            manifest.files.keys().collect::<Vec<_>>(),
            ["~/.aterm/missing.key"],
            "a file left alone is not the vault's"
        );
        assert!(!secrets.join(REPLACED_DIR).exists());

        // The same sync again changes nothing; the vault dropping every entry removes only
        // the file it wrote.
        let (again, report) = apply(home.path(), &entries, &manifest, 4).expect("again");
        assert_eq!(again, manifest);
        assert_eq!(report.unchanged, ["~/.aterm/missing.key"]);
        assert_eq!(report.left.len(), 4);
        let (last, report) = apply(home.path(), &[], &again, 5).expect("dropped");
        assert_eq!(report.removed, ["~/.aterm/missing.key"]);
        assert!(report.left.is_empty() && report.failed.is_empty());
        assert!(last.files.is_empty());
        assert_eq!(read(&aterm.join("machine.key")), b"this machine's own");
        assert_eq!(read(&secrets.join("same")), b"v");
    }

    #[test]
    fn an_if_absent_file_the_vault_wrote_is_the_machines_own_once_changed() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        let config = home.path().join(".config/git/config");
        let version = |v: &[u8]| {
            [
                gap("a", v),
                gap("~/.config/git/config", v),
                gap("c", v),
                gap("d", v),
            ]
        };
        let (held, report) =
            apply(home.path(), &version(b"v1"), &Manifest::default(), 1).expect("first");
        assert_eq!(report.written, ["a", "~/.config/git/config", "c", "d"]);
        // The config is edited where the vault put it (`git config --global` with no
        // ~/.gitconfig, `gh auth setup-git`), `c` is deleted (a gap again), `d` becomes a
        // link.
        plant(&config, b"edited since", 0o644);
        std::fs::remove_file(secrets.join("c")).expect("rm");
        let elsewhere = home.path().join("elsewhere");
        plant(&elsewhere, b"linked", 0o600);
        std::fs::remove_file(secrets.join("d")).expect("rm");
        std::os::unix::fs::symlink(&elsewhere, secrets.join("d")).expect("link");
        let (manifest, report) = apply(home.path(), &version(b"v2"), &held, 2).expect("second");
        assert_eq!(report.written, ["a", "c"]);
        assert_eq!(report.left, ["~/.config/git/config", "d"]);
        assert!(report.moved_aside.is_empty() && report.failed.is_empty());
        assert!(!secrets.join(REPLACED_DIR).exists(), "nothing moved aside");
        assert_eq!(read(&secrets.join("a")), b"v2");
        assert_eq!(read(&secrets.join("c")), b"v2");
        assert_eq!(read(&config), b"edited since");
        assert_eq!(mode(&config), 0o644, "not even narrowed");
        assert_eq!(fsx::kind(&secrets.join("d")).expect("kind"), Kind::Symlink);
        assert_eq!(read(&elsewhere), b"linked");
        assert_eq!(
            manifest.files.keys().collect::<Vec<_>>(),
            ["a", "c"],
            "a changed file is the machine's own now"
        );

        // Every later version leaves them alone and still updates the vault's own.
        let (manifest, report) = apply(home.path(), &version(b"v3"), &manifest, 3).expect("third");
        assert_eq!(report.written, ["a", "c"]);
        assert_eq!(report.left, ["~/.config/git/config", "d"]);
        assert_eq!(read(&config), b"edited since");
        let (rest, report) = apply(home.path(), &[], &manifest, 4).expect("unset");
        assert_eq!(report.removed, ["a", "c"]);
        assert!(report.left.is_empty() && report.failed.is_empty());
        assert!(rest.files.is_empty());
        assert_eq!(read(&config), b"edited since");
        assert_eq!(fsx::kind(&secrets.join("d")).expect("kind"), Kind::Symlink);
    }

    #[test]
    fn an_if_absent_entry_cannot_claim_a_file_another_entry_placed() {
        let home = scratch();
        let first = home.path().join("first");
        plant(&first, b"v", 0o600);
        std::fs::hard_link(&first, home.path().join("second")).expect("link");
        let (_, report) = apply(
            home.path(),
            &[entry("~/first", b"v"), gap("~/second", b"w")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        assert_eq!(failed_names(&report), ["~/second"]);
        assert!(report.left.is_empty());
    }

    fn anchored(name: &str, value: &[u8], anchor: &str) -> Entry {
        Entry {
            anchor: Some(anchor.to_string()),
            ..gap(name, value)
        }
    }

    const KEY: &str = "~/.aterm/machine.key";
    const TOML: &str = "~/.aterm/machine.toml";
    const PROFILE: &str = "~/.aterm/release-credentials.toml";

    /// The release identity as the vault carries it, sorted by name as a merge leaves it:
    /// a ring, the key anchored to machine.toml, machine.toml to the profile and the profile
    /// to the key, so any one of them the machine's own keeps the other two away.
    fn identity(value: &[u8]) -> [Entry; 3] {
        [
            anchored(KEY, value, TOML),
            anchored(TOML, value, PROFILE),
            anchored(PROFILE, value, KEY),
        ]
    }

    #[test]
    fn a_machines_own_machine_toml_keeps_the_vaults_key_and_profile_away() {
        // What a failed provision leaves when the operator removes only the key it made.
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        std::fs::create_dir(home.path().join(".aterm")).expect("dir");
        plant(&at(TOML), b"its own id", 0o600);
        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(report.written.is_empty() && report.failed.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!at(KEY).exists() && !at(PROFILE).exists());
        assert_eq!(read(&at(TOML)), b"its own id");
    }

    #[test]
    fn a_pass_cut_short_after_its_writes_keeps_the_anchored_set() {
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        let (held, _) =
            apply(home.path(), &identity(b"v1"), &Manifest::default(), 1).expect("first");
        // The next version's profile is written, and the pass ends before the manifest that
        // records it is saved.
        plant(&at(PROFILE), b"v2", 0o600);
        let (manifest, report) = apply(home.path(), &identity(b"v2"), &held, 2).expect("again");
        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert!(report.left.is_empty() && report.failed.is_empty());
        for name in [KEY, TOML, PROFILE] {
            assert_eq!(read(&at(name)), b"v2", "{name}");
            assert_eq!(manifest.files[name], digest_hex(b"v2"), "{name}");
        }
    }

    #[test]
    fn an_anchored_set_fills_a_machine_that_has_none_of_it_whole() {
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
        assert_eq!(report.written, [KEY, TOML, PROFILE]);
        assert!(report.left.is_empty() && report.failed.is_empty());
        for name in [KEY, TOML, PROFILE] {
            assert_eq!(read(&at(name)), b"shared", "{name}");
            assert_eq!(manifest.files[name], digest_hex(b"shared"), "{name}");
        }

        // The set the vault wrote takes the next version, and a part deleted is filled
        // again beside the rest.
        std::fs::remove_file(at(PROFILE)).expect("rm");
        let (manifest, report) =
            apply(home.path(), &identity(b"next"), &manifest, 2).expect("next");
        assert_eq!(report.written, [KEY, TOML, PROFILE]);
        assert_eq!(read(&at(PROFILE)), b"next");
        assert_eq!(manifest.files.len(), 3);

        // The machine makes its own key: the vault's profile is never filled beside it, and
        // the machine.toml the vault wrote beside the vault's key is taken back.
        plant(&at(KEY), b"its own key", 0o600);
        std::fs::remove_file(at(PROFILE)).expect("rm");
        let (manifest, report) = apply(home.path(), &identity(b"next"), &manifest, 3).expect("own");
        assert_eq!(report.removed, [TOML]);
        assert_eq!(report.left, [KEY, PROFILE]);
        assert!(report.written.is_empty() && report.unchanged.is_empty());
        assert!(report.moved_aside.is_empty() && report.failed.is_empty());
        assert_eq!(read(&at(KEY)), b"its own key");
        assert!(!at(PROFILE).exists());
        assert!(!at(TOML).exists());
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn a_profile_the_vault_filled_beside_a_machines_own_key_is_taken_back() {
        // An older vault filled the profile alone, as a plain if-absent entry, on a Mac with
        // its own key and machine.toml: the manifest records it. Anchored now, it goes, while
        // it still holds what the vault wrote.
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        std::fs::create_dir(home.path().join(".aterm")).expect("dir");
        plant(&at(KEY), b"its own key", 0o600);
        plant(&at(TOML), b"its own id", 0o600);
        let (held, report) = apply(
            home.path(),
            &[gap(KEY, b"v1"), gap(TOML, b"v1"), gap(PROFILE, b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("unanchored");
        assert_eq!(report.written, [PROFILE], "what the defect did");
        assert_eq!(held.files.keys().collect::<Vec<_>>(), [PROFILE]);

        let (manifest, report) = apply(home.path(), &identity(b"v2"), &held, 2).expect("anchored");
        assert_eq!(report.removed, [PROFILE]);
        assert_eq!(report.left, [KEY, TOML]);
        assert!(report.written.is_empty() && report.unchanged.is_empty());
        assert!(report.moved_aside.is_empty() && report.failed.is_empty());
        assert!(!at(PROFILE).exists());
        assert_eq!(read(&at(KEY)), b"its own key");
        assert_eq!(read(&at(TOML)), b"its own id");
        assert!(manifest.files.is_empty());
        assert!(!home.path().join(".secrets").join(REPLACED_DIR).exists());

        // Every later version leaves the gap unfilled.
        let (manifest, report) = apply(home.path(), &identity(b"v3"), &manifest, 3).expect("again");
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(report.written.is_empty() && report.removed.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!at(PROFILE).exists());

        // A profile the person changed since the vault wrote it is theirs: it stays.
        let other = scratch();
        let at = |name: &str| target_path(other.path(), name).expect("valid");
        std::fs::create_dir(other.path().join(".aterm")).expect("dir");
        plant(&at(KEY), b"its own key", 0o600);
        let (held, _) = apply(
            other.path(),
            &[gap(KEY, b"v1"), gap(PROFILE, b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("unanchored");
        plant(&at(PROFILE), b"edited since", 0o600);
        let (manifest, report) = apply(other.path(), &identity(b"v2"), &held, 2).expect("anchored");
        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert_eq!(read(&at(PROFILE)), b"edited since");
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn a_key_and_machine_toml_the_vault_filled_beside_a_machines_own_profile_are_taken_back() {
        // machine.toml's anchor is the machine's own profile, and the key's is machine.toml:
        // the ring falls whole.
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        std::fs::create_dir(home.path().join(".aterm")).expect("dir");
        plant(&at(PROFILE), b"its own profile", 0o600);
        let (held, report) = apply(
            home.path(),
            &[gap(KEY, b"v1"), gap(TOML, b"v1"), gap(PROFILE, b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("unanchored");
        assert_eq!(report.written, [KEY, TOML], "what the defect did");
        let (manifest, report) = apply(home.path(), &identity(b"v1"), &held, 2).expect("anchored");
        assert_eq!(report.removed, [KEY, TOML]);
        assert_eq!(report.left, [PROFILE]);
        assert!(report.unchanged.is_empty() && report.failed.is_empty());
        assert!(!at(KEY).exists() && !at(TOML).exists());
        assert_eq!(read(&at(PROFILE)), b"its own profile");
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn a_file_the_vault_wrote_beside_an_anchor_it_cannot_look_at_stays_the_vaults() {
        // The anchor lies behind a symlinked directory: the anchored file is neither updated
        // nor taken back, and keeps its digest for a sync that can judge it.
        let home = scratch();
        let outside = scratch();
        let anchor = "~/link/key";
        let (held, report) = apply(
            home.path(),
            &[gap("profile", b"v1")],
            &Manifest::default(),
            1,
        )
        .expect("first");
        assert_eq!(report.written, ["profile"]);
        std::os::unix::fs::symlink(outside.path(), home.path().join("link")).expect("link");
        let entries = [gap(anchor, b"v2"), anchored("profile", b"v2", anchor)];
        let (manifest, report) = apply(home.path(), &entries, &held, 2).expect("unseen");
        assert_eq!(failed_names(&report), [anchor]);
        assert_eq!(report.left, ["profile"]);
        assert!(report.written.is_empty() && report.removed.is_empty());
        assert_eq!(read(&home.path().join(".secrets/profile")), b"v1");
        assert_eq!(manifest, held, "still what the vault wrote");
    }

    #[test]
    fn a_machines_own_key_keeps_the_vaults_profile_and_machine_toml_away() {
        let home = scratch();
        let aterm = home.path().join(".aterm");
        std::fs::create_dir(&aterm).expect("dir");
        plant(&aterm.join("machine.key"), b"its own key", 0o600);
        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(report.written.is_empty() && report.failed.is_empty());
        assert!(manifest.files.is_empty());
        assert_eq!(read(&aterm.join("machine.key")), b"its own key");
        assert!(!aterm.join("machine.toml").exists());
        assert!(!aterm.join("release-credentials.toml").exists());

        // With its own machine.toml too, and every sync after.
        plant(&aterm.join("machine.toml"), b"its own id", 0o600);
        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &manifest, 2).expect("again");
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(manifest.files.is_empty());
        assert!(!aterm.join("release-credentials.toml").exists());
    }

    #[test]
    fn a_machines_own_profile_keeps_the_vaults_key_away() {
        let home = scratch();
        let aterm = home.path().join(".aterm");
        std::fs::create_dir(&aterm).expect("dir");
        plant(
            &aterm.join("release-credentials.toml"),
            b"its own profile",
            0o600,
        );
        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
        // The key's anchor is a gap, machine.toml, but one its own anchor keeps from filling.
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(report.written.is_empty() && report.failed.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!aterm.join("machine.key").exists());
        assert!(!aterm.join("machine.toml").exists());
        assert_eq!(
            read(&aterm.join("release-credentials.toml")),
            b"its own profile"
        );
    }

    #[test]
    fn an_anchor_the_machine_cannot_judge_fills_nothing() {
        let home = scratch();
        let outside = scratch();
        std::os::unix::fs::symlink(outside.path(), home.path().join("link")).expect("link");
        let entries = [
            anchored("a", b"v", "missing"),
            entry("plain", b"v"),
            anchored("b", b"v", "plain"),
            gap("~/link/key", b"v"),
            anchored("c", b"v", "~/link/key"),
        ];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
        assert_eq!(report.written, ["plain"]);
        assert_eq!(failed_names(&report), ["a", "b", "~/link/key"]);
        assert!(
            report.failed[0].1.contains("anchor `missing`"),
            "{:?}",
            report.failed
        );
        assert!(
            report.failed[1].1.contains("anchor `plain`"),
            "{:?}",
            report.failed
        );
        // Seen only through a link, an anchor is not the vault's.
        assert_eq!(report.left, ["c"]);
        let secrets = home.path().join(".secrets");
        for name in ["a", "b", "c"] {
            assert!(!secrets.join(name).exists(), "{name}");
        }
        assert!(!outside.path().join("key").exists());
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["plain"]);
    }

    #[test]
    fn a_symlinked_directory_on_the_way_is_refused_never_followed() {
        let home = scratch();
        let outside = scratch();
        std::os::unix::fs::symlink(outside.path(), home.path().join(".aterm")).expect("link");
        let real = home.path().join("real");
        std::fs::create_dir_all(real.join("deep")).expect("dir");
        std::fs::create_dir(home.path().join("a")).expect("dir");
        std::os::unix::fs::symlink(real.join("deep"), home.path().join("a/b")).expect("link");
        let entries = [
            entry("~/.aterm/release.toml", b"v"),
            entry("~/a/b/c/x", b"v"),
            entry("~/a/fine", b"v"),
        ];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
        assert_eq!(report.written, ["~/a/fine"]);
        assert_eq!(
            failed_names(&report),
            ["~/.aterm/release.toml", "~/a/b/c/x"]
        );
        let aterm = home.path().join(".aterm").display().to_string();
        assert!(report.failed[0].1.contains(&aterm), "{:?}", report.failed);
        assert!(
            report.failed[0].1.contains("symlink"),
            "{:?}",
            report.failed
        );
        let b = home.path().join("a/b").display().to_string();
        assert!(report.failed[1].1.contains(&b), "{:?}", report.failed);
        assert!(!outside.path().join("release.toml").exists());
        assert!(
            !real.join("deep/c").exists(),
            "no directory made through the link"
        );
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["~/a/fine"]);

        // A dropped name behind a symlinked directory (one an older build wrote through) is
        // never removed through it, and stays the vault's until a sync reaches it directly.
        plant(&outside.path().join("old.toml"), b"o", 0o600);
        let held = Manifest {
            files: BTreeMap::from([("~/.aterm/old.toml".to_string(), digest_hex(b"o"))]),
        };
        let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
        assert_eq!(report.left, ["~/.aterm/old.toml"]);
        assert!(report.removed.is_empty() && report.failed.is_empty());
        assert_eq!(manifest, held, "still what the vault wrote");
        assert_eq!(read(&outside.path().join("old.toml")), b"o");
        let (again, report) = apply(home.path(), &[], &manifest, 3).expect("again");
        assert_eq!(report.left, ["~/.aterm/old.toml"]);
        assert_eq!(again, held, "kept however many syncs meet the link");

        // The person replaces the link with the directory it named: the file goes.
        std::fs::remove_file(home.path().join(".aterm")).expect("unlink");
        std::fs::create_dir(home.path().join(".aterm")).expect("dir");
        plant(&home.path().join(".aterm/old.toml"), b"o", 0o600);
        let (last, report) = apply(home.path(), &[], &again, 4).expect("reached");
        assert_eq!(report.removed, ["~/.aterm/old.toml"]);
        assert!(report.left.is_empty());
        assert!(last.files.is_empty());
        assert!(!home.path().join(".aterm/old.toml").exists());
    }

    #[test]
    fn a_symlinked_secrets_dir_fails_its_entries_and_spares_the_rest() {
        let home = scratch();
        let real = home.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        let secrets = home.path().join(".secrets");
        std::os::unix::fs::symlink(&real, &secrets).expect("link");
        plant(&real.join("dropped"), b"d", 0o600);
        plant(&home.path().join("foreign"), b"hand-made", 0o600);
        let held = Manifest {
            files: BTreeMap::from([
                ("a".to_string(), digest_hex(b"old")),
                ("dropped".to_string(), digest_hex(b"d")),
            ]),
        };
        let entries = [
            entry("a", b"v"),
            entry("~/fine", b"v"),
            entry("~/foreign", b"v"),
        ];
        let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
        assert_eq!(report.written, ["~/fine"]);
        assert_eq!(failed_names(&report), ["a", "~/foreign"]);
        for (_, why) in &report.failed {
            assert!(
                why.contains(&secrets.display().to_string()) && why.contains("symlink"),
                "{why}"
            );
        }
        assert_eq!(report.left, ["dropped"]);
        assert!(report.moved_aside.is_empty() && report.removed.is_empty());
        assert!(!real.join("a").exists());
        assert!(!real.join(REPLACED_DIR).exists());
        assert_eq!(read(&real.join("dropped")), b"d");
        assert_eq!(
            read(&home.path().join("foreign")),
            b"hand-made",
            "not overwritten when it cannot be moved aside"
        );
        assert_eq!(
            manifest.files.get("a"),
            Some(&digest_hex(b"old")),
            "a failed entry keeps what the vault wrote"
        );
        assert_eq!(
            manifest.files.get("dropped"),
            Some(&digest_hex(b"d")),
            "a dropped name it could not reach stays the vault's"
        );
        assert_eq!(fsx::kind(&secrets).expect("kind"), Kind::Symlink);

        // The folder moved back in place of the link: the dropped file, still as the vault
        // wrote it, goes; the rest are written.
        std::fs::remove_file(&secrets).expect("unlink");
        std::fs::rename(&real, &secrets).expect("move back");
        let (last, report) = apply(home.path(), &entries, &manifest, 2).expect("reached");
        assert_eq!(report.removed, ["dropped"]);
        assert!(report.failed.is_empty() && report.left.is_empty());
        assert!(!secrets.join("dropped").exists());
        assert_eq!(read(&secrets.join("a")), b"v");
        assert!(!last.files.contains_key("dropped"));
    }

    #[test]
    fn an_entry_behind_a_symlinked_directory_is_refused_before_any_look() {
        let home = scratch();
        let real = home.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        std::os::unix::fs::symlink(&real, home.path().join(".secrets")).expect("link");
        plant(&real.join("gh_token"), b"theirs", 0o600);
        let deep = home.path().join("deep");
        std::fs::create_dir(&deep).expect("dir");
        plant(&deep.join("c"), b"theirs", 0o600);
        std::fs::create_dir(home.path().join("a")).expect("dir");
        std::os::unix::fs::symlink(&deep, home.path().join("a/b")).expect("link");
        let target = home.path().join("target");
        std::fs::create_dir(&target).expect("dir");
        plant(&target.join("x"), b"v", 0o600);
        std::os::unix::fs::symlink(&target, home.path().join("link")).expect("link");
        let entries = [
            gap("gh_token", b"vault"),
            gap("~/a/b/c", b"vault"),
            entry("~/link/x", b"v"),
            entry("~/target/x", b"v"),
        ];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
        // An existing file seen through the link is no reason to leave an if-absent entry:
        // it fails, naming the link, like any entry there.
        assert_eq!(failed_names(&report), ["gh_token", "~/a/b/c", "~/link/x"]);
        for ((_, why), link) in report.failed.iter().zip([".secrets", "a/b", "link"]) {
            let link = home.path().join(link).display().to_string();
            assert!(why.contains(&link) && why.contains("symlink"), "{why}");
        }
        assert!(report.left.is_empty(), "{:?}", report.left);
        // What the refused `~/link/x` saw through the link is not claimed by it.
        assert_eq!(report.unchanged, ["~/target/x"]);
        // Adopted, it stays the machine's own; nothing else is the vault's either.
        assert!(manifest.files.is_empty(), "{:?}", manifest.files);
        assert_eq!(read(&real.join("gh_token")), b"theirs");
        assert_eq!(read(&deep.join("c")), b"theirs");
    }

    #[test]
    fn a_dropped_name_for_a_file_a_refused_entry_names_is_kept() {
        // `~/link/x` names the file the vault wrote as `~/real/x`, through a symlinked
        // directory: removing `~/real/x` would wipe the file that entry names, through
        // another name.
        let home = scratch();
        let real = home.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        plant(&real.join("x"), b"v", 0o600);
        std::os::unix::fs::symlink(&real, home.path().join("link")).expect("link");
        let held = Manifest {
            files: BTreeMap::from([("~/real/x".to_string(), digest_hex(b"v"))]),
        };
        let (manifest, report) =
            apply(home.path(), &[entry("~/link/x", b"w")], &held, 2).expect("applied");
        assert_eq!(failed_names(&report), ["~/link/x"]);
        assert_eq!(report.left, ["~/real/x"]);
        assert!(report.removed.is_empty());
        assert_eq!(read(&real.join("x")), b"v");
        assert_eq!(manifest, held);
    }

    #[test]
    fn an_invalid_name_fails_that_entry_only() {
        let home = scratch();
        let (_, report) = apply(
            home.path(),
            &[entry("../escape", b"v"), entry("ok", b"v")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        assert_eq!(failed_names(&report), ["../escape"]);
        assert_eq!(report.written, ["ok"]);
        assert!(!home.path().join("escape").exists());
    }

    #[test]
    fn moved_aside_names_stay_within_name_max_and_apart() {
        assert_eq!(aside_name("gh_token", 1700), "gh_token.1700");
        let longest_file_name = "a".repeat(128);
        assert_eq!(
            aside_name(&longest_file_name, u64::MAX),
            format!("{longest_file_name}.{}", u64::MAX)
        );
        let deep = |last: &str| format!("~/{}/{last}", vec!["c".repeat(100); 6].join("/"));
        let (one, two) = (deep("one.toml"), deep("two.toml"));
        for now in [0, 1_792_530_000, u64::MAX] {
            let a = aside_name(&one, now);
            let b = aside_name(&two, now);
            assert!(a.len() + ".999".len() <= NAME_MAX, "{}", a.len());
            assert_ne!(a, b, "a shared start still gives two names");
            assert_eq!(a, aside_name(&one, now), "deterministic");
            assert!(a.starts_with("~%2Fcccc"), "{a}");
            assert!(a.ends_with(&format!(".{now}")), "{a}");
        }

        // On disk: a long home path's foreign file moves aside under the cut name, twice.
        let home = scratch();
        let target = target_path(home.path(), &one).expect("valid");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("dirs");
        for (round, before) in [(0, b"first".as_slice()), (1, b"second".as_slice())] {
            plant(&target, before, 0o600);
            let (_, report) = apply(
                home.path(),
                &[entry(&one, b"vault")],
                &Manifest::default(),
                9,
            )
            .expect("applied");
            let base = home
                .path()
                .join(".secrets/.replaced")
                .join(aside_name(&one, 9));
            let want = if round == 0 {
                base
            } else {
                PathBuf::from(format!("{}.1", base.display()))
            };
            assert_eq!(report.moved_aside, [(one.clone(), want.clone())]);
            assert_eq!(read(&want), before);
        }
    }

    #[test]
    fn reports_carry_names_never_values() {
        let sentinel = "Zq9!sentinel+value";
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir_all(secrets.join("d")).expect("dir");
        plant(&secrets.join("old"), sentinel.as_bytes(), 0o600);
        let entries = [
            entry("old", b"new"),
            entry("d", sentinel.as_bytes()),
            entry("n", sentinel.as_bytes()),
        ];
        let (manifest, report) =
            apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
        let shown = format!("{report:?} {manifest:?}");
        assert!(!shown.contains("sentinel"), "{shown}");
        assert_eq!(report.failed.len(), 1);
    }
}

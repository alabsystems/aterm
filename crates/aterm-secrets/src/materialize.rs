// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Writing a machine's secrets to disk (docs/DESIGN-secrets-2026-10-20.md §7, Sync).

use crate::fsx::{self, Kind};
use crate::text::{self, Entry};
use crate::{Error, SecretBytes};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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
    /// Removed because the vault dropped them or took them back, or because their name is
    /// reserved ([`crate::reserved_name`]) and a build that did not reserve it wrote the file.
    pub removed: Vec<String>,
    /// Left in place: dropped (or taken back) by the vault, or a reserved name's file, but
    /// changed since it wrote them, not looked at, or ` if-absent` entries whose target or
    /// anchor is the machine's own or whose anchor could not be looked at.
    pub left: Vec<String>,
    /// Not written, or not removed: the name and why (never the value).
    pub failed: Vec<(String, String)>,
}

/// The directory under `~/.secrets` that moved-aside files go to.
pub const REPLACED_DIR: &str = ".replaced";

const MAX_ASIDE_SUFFIX: u32 = 1000;

const NAME_MAX: usize = 255;

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

/// Where entry `name` lands under `home`.
pub fn target_path(home: &Path, name: &str) -> crate::Result<PathBuf> {
    if crate::reserved_name(name) {
        return Err(Error::new("a reserved entry has no plaintext file path"));
    }
    written_path(home, name)
}

/// Where entry `name` landed under `home` in a build that did not reserve it: [`target_path`]
/// without the reserved-name refusal, for taking back the file such a build wrote.
fn written_path(home: &Path, name: &str) -> crate::Result<PathBuf> {
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

/// The name a moved-aside copy of entry `name` gets (before any `.1`, `.2` suffix).
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
    while !flat.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}~{}{stamp}", &flat[..keep], &digest[..ASIDE_DIGEST])
}

/// Make the files under `home` match `entries`, given what the vault wrote last time.
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

/// Entry name → value digest of what a pass is to write, or a pass cut short was writing.
pub type Pending = BTreeMap<String, String>;

/// [`apply`], resuming a pass cut short and saving ahead for this one.
pub fn apply_resuming(
    home: &Path,
    entries: &[Entry],
    held: &Manifest,
    resumed: &Pending,
    now: u64,
    ahead: &mut dyn FnMut(&Manifest, &Pending) -> crate::Result<()>,
) -> crate::Result<(Manifest, Report)> {
    // Control records stay in the encrypted vault cache. Filtering before any
    // path/standing/pending computation also prevents reporting their keys as
    // ordinary materialization failures or treating them as files to restore.
    // A reserved name the held manifest still names is a file a build that did not
    // reserve it wrote (the fleet code before it was reserved): it is taken back below,
    // once this pass's writes are done, as a dropped entry's file is.
    let written_reserved: Vec<(String, String)> = held
        .files
        .iter()
        .filter(|(name, _)| crate::reserved_name(name))
        .map(|(name, digest)| (name.clone(), digest.clone()))
        .collect();
    let ordinary;
    let entries = if entries
        .iter()
        .any(|entry| crate::reserved_name(&entry.name))
    {
        ordinary = entries
            .iter()
            .filter(|entry| !crate::reserved_name(&entry.name))
            .cloned()
            .collect::<Vec<_>>();
        ordinary.as_slice()
    } else {
        entries
    };
    let ordinary_held = Manifest {
        files: held
            .files
            .iter()
            .filter(|(name, _)| !crate::reserved_name(name))
            .map(|(name, digest)| (name.clone(), digest.clone()))
            .collect(),
    };
    let held = &ordinary_held;
    let secrets = home.join(".secrets");
    let places = Places::new(home, &secrets, now);
    let mut resumed_held = held.clone();
    for (name, digest) in resumed {
        let Ok(target) = target_path(home, name) else {
            continue;
        };
        if !places.reachable(name) {
            continue;
        }
        sweep_temps(&target, digest);
        if held.files.get(name) == Some(digest) {
            continue;
        }
        let wrote = fsx::read_private(&target, text::MAX_VALUE)
            .ok()
            .flatten()
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
    if written_reserved.is_empty() {
        ahead(held, &pending)?;
    } else {
        // Saved ahead with the reserved names' records, so a pass cut short leaves their files
        // for the next pass to take back.
        let mut kept = held.clone();
        kept.files.extend(written_reserved.iter().cloned());
        ahead(&kept, &pending)?;
    }
    let mut manifest = Manifest::default();
    let mut report = Report::default();
    let mut claimed: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut spared: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut withdrawn: Vec<(String, String)> = Vec::new();
    // Each directory a write renamed a file into: the entries written there, and the digest
    // each held before ([`flush_written`]).
    let mut written_in: BTreeMap<PathBuf, Vec<(String, Option<String>)>> = BTreeMap::new();
    for entry in entries {
        let mut held_digest = held.files.get(&entry.name);
        let placed = match target_path(home, &entry.name) {
            Err(error) => Err(error.0),
            Ok(_) if !text::anchor_received(entry, |name| by_name.get(name).copied()) => {
                Err(format!(
                    "its anchor `{}` is not an if-absent entry this machine receives",
                    entry.anchor.as_deref().unwrap_or_default()
                ))
            }
            Ok(target) => match places.prepare(&entry.name) {
                Err(why) => {
                    spared.extend(fsx::file_id(&target));
                    if entry.if_absent && places.linked(&entry.name) {
                        Ok(if held_digest.is_some() {
                            Placed::Held
                        } else {
                            Placed::Left
                        })
                    } else {
                        Err(why)
                    }
                }
                Ok(()) => {
                    if held_digest.is_none() {
                        held_digest =
                            fsx::file_id(&target).and_then(|id| history.get(&id).copied());
                    }
                    let digest = digest_hex(entry.value.as_slice());
                    let unannounced = !entry.if_absent
                        && !pending.contains_key(&entry.name)
                        && held_digest != Some(&digest)
                        && matches!(
                            fsx::kind(&target),
                            Ok(Kind::Missing | Kind::File | Kind::Symlink)
                        );
                    let standing = standings
                        .get(entry.name.as_str())
                        .copied()
                        .unwrap_or(Standing::Unseen);
                    let placed = if unannounced && adoptable(&target, entry, held_digest) {
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
                if let Some(dir) = target_path(home, &entry.name)
                    .ok()
                    .and_then(|target| target.parent().map(Path::to_path_buf))
                {
                    written_in
                        .entry(dir)
                        .or_default()
                        .push((entry.name.clone(), held_digest.cloned()));
                }
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
    flush_written(&written_in, &mut manifest, &mut report);
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
            continue;
        }
        if !places.reachable(name) || id.is_some_and(|id| spared.contains(&id)) {
            manifest.files.insert(name.clone(), digest.clone());
            report.left.push(name.clone());
            continue;
        }
        take_back(home, name, &target, digest).book(name, digest, &mut manifest, &mut report);
    }
    // A reserved name's file is never the vault's to keep: removed while it holds what the
    // vault wrote, left (and out of the manifest) once the person changed it or where this
    // pass does not look, and kept in the manifest only when its removal failed, so the next
    // pass tries again.
    for (name, digest) in &written_reserved {
        let Ok(target) = written_path(home, name) else {
            continue;
        };
        let id = fsx::file_id(&target);
        if id.is_some_and(|id| claimed.contains_key(&id)) {
            continue;
        }
        if !places.reachable(name) || id.is_some_and(|id| spared.contains(&id)) {
            report.left.push(name.clone());
            continue;
        }
        take_back(home, name, &target, digest).book(name, digest, &mut manifest, &mut report);
    }
    Ok((manifest, report))
}

/// What [`take_back`] did with a file the vault wrote and no longer places.
enum TakenBack {
    /// Gone already, or under a file where no file can be.
    Gone,
    /// Held what the vault wrote, and removed.
    Removed,
    /// Changed since the vault wrote it, or not a plain file: left as it is.
    Left,
    /// Could not be looked at or removed: why.
    Failed(String),
}

impl TakenBack {
    /// Book what happened to entry `name`'s file, which the vault wrote with digest `digest`:
    /// a failed removal keeps the record, so the next pass tries again.
    fn book(self, name: &str, digest: &str, manifest: &mut Manifest, report: &mut Report) {
        match self {
            TakenBack::Gone => {}
            TakenBack::Removed => report.removed.push(name.to_owned()),
            TakenBack::Left => report.left.push(name.to_owned()),
            TakenBack::Failed(why) => {
                manifest.files.insert(name.to_owned(), digest.to_owned());
                report.failed.push((name.to_owned(), why));
            }
        }
    }
}

/// Remove `target`, entry `name`'s file under `home`, if it still holds the value of digest
/// `digest`; never through a link.
fn take_back(home: &Path, name: &str, target: &Path, digest: &str) -> TakenBack {
    match fsx::kind(target) {
        Ok(Kind::Missing) => TakenBack::Gone,
        Ok(Kind::File) => {
            match fsx::wipe_if(target, text::MAX_VALUE, |bytes| digest_hex(bytes) == digest) {
                Ok(true) => TakenBack::Removed,
                Ok(false) => TakenBack::Left,
                Err(error) => TakenBack::Failed(error.0),
            }
        }
        Ok(Kind::Symlink | Kind::Dir | Kind::Other) => TakenBack::Left,
        Err(_) if under_a_file(home, name) => TakenBack::Gone,
        Err(error) => TakenBack::Failed(error.0),
    }
}

/// Flush once each directory this pass's writes renamed files into (`written_in`), after their
/// ordered writes ([`fsx::write_ordered`]): N files cost N ordered flushes and one durable flush
/// per directory, not two durable flushes each, and their renames are durable before the state
/// that names them is saved. An entry whose directory's flush failed (other than as one the
/// volume cannot flush) failed, with the digest it held before kept, as a write that failed
/// after its rename always has: the next pass writes it again.
fn flush_written(
    written_in: &BTreeMap<PathBuf, Vec<(String, Option<String>)>>,
    manifest: &mut Manifest,
    report: &mut Report,
) {
    for (dir, written) in written_in {
        let Err(error) = aterm_durable::Flush::Durable.dir_best_effort(dir) else {
            continue;
        };
        for (name, held) in written {
            report.written.retain(|done| done != name);
            match held {
                Some(digest) => manifest.files.insert(name.clone(), digest.clone()),
                None => manifest.files.remove(name),
            };
            report
                .failed
                .push((name.clone(), format!("{}: {error}", dir.display())));
        }
    }
}

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
            let adopted = target
                .as_deref()
                .is_some_and(|target| adoptable(target, entry, wrote));
            (wrote != Some(&digest) && !adopted).then(|| (entry.name.clone(), digest))
        })
        .collect()
}

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

struct Places<'a> {
    home: &'a Path,
    secrets: &'a Path,
    /// `~/.secrets` made private, or why it was refused (a symlink, not a directory, another
    /// user's): the reason every entry that lands there, and every move aside, fails with.
    secrets_ok: Result<(), String>,
    /// What moved-aside copies are named by ([`aside_name`]).
    now: u64,
    /// aterm's own key and state directory (`~/.config/aterm/secrets`, links followed) by
    /// (device, inode): a home path that reaches it by another spelling (through the real
    /// directory a symlinked `~/.config` names) is refused as a symlinked directory is, so no
    /// entry lands on the machine key or the manifest.
    own: Option<(u64, u64)>,
}

impl<'a> Places<'a> {
    fn new(home: &'a Path, secrets: &'a Path, now: u64) -> Places<'a> {
        Places {
            home,
            secrets,
            secrets_ok: fsx::ensure_private_dir(secrets).map_err(|error| error.0),
            now,
            own: own_dir(home),
        }
    }

    fn prepare(&self, name: &str) -> Result<(), String> {
        match name.strip_prefix("~/") {
            Some(rel) => make_parents(self.home, rel, self.own),
            None => self.secrets_ok.clone(),
        }
    }

    /// Whether a directory on the way to home path `name`'s file is a symlink (a dotfiles
    /// setup's `~/.config`, say): an ` if-absent` entry there is the machine's own, never
    /// written through the link and not a failure, as a link in the file's own place is.
    fn linked(&self, name: &str) -> bool {
        linked(self.home, name, self.own)
    }

    fn reachable(&self, name: &str) -> bool {
        match name.strip_prefix("~/") {
            Some(rel) => refused_dir(self.home, rel, self.own).is_none(),
            None => self.secrets_ok.is_ok(),
        }
    }
}

enum Placed {
    Written,
    Unchanged,
    /// A plain entry whose target, a file the vault never wrote, already held its value:
    /// narrowed and kept, and still the machine's own (out of the manifest), so dropping the
    /// entry leaves it and a new value moves it aside first.
    Adopted,
    /// An ` if-absent` entry whose target, or anchor, is the machine's own, or whose target
    /// is behind a symlinked directory ([`Places::linked`]).
    Left,
    /// An anchored ` if-absent` entry whose file the vault wrote and whose anchor is the
    /// machine's own: taken back, as a dropped entry is.
    Withdrawn,
    /// An ` if-absent` entry whose file the vault wrote and which could not be looked at (its
    /// anchor, or its own file behind a symlinked directory): left as it is, and still the
    /// vault's.
    Held,
}

/// How an ` if-absent` entry's file stands, with whatever it is anchored to ([`standings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// The vault's: a gap, or a file still holding what the vault wrote. As an anchor, it
    /// lets the entries anchored to it fill their gaps and update what the vault wrote.
    Vaults,
    /// The machine's own: a file the vault never wrote or that changed since, a link, a
    /// directory.
    Own,
    /// Not looked at (seen only through a symlinked or refused directory, or an error):
    /// as an anchor, it lets nothing be written and takes nothing back.
    Unseen,
}

enum Judged {
    /// The new value already.
    Same,
    /// What the vault last wrote (or nothing any more): overwrite it.
    Ours,
    /// Something the vault did not write, or edited since, or unreadable: move it aside.
    Foreign,
}

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
    fsx::write_ordered(target, value, 0o600).map_err(|error| error.0)?;
    Ok(Placed::Written)
}

fn fill(
    target: &Path,
    kind: Kind,
    value: &[u8],
    held: Option<&String>,
    standing: Standing,
) -> Result<Placed, String> {
    let changed = || {
        Err(format!(
            "{}: became this machine's own while this pass ran; the next pass judges it again",
            target.display()
        ))
    };
    match kind {
        Kind::Missing if standing == Standing::Vaults => {
            if fsx::write_new_ordered(target, value, 0o600).map_err(|error| error.0)? {
                Ok(Placed::Written)
            } else {
                changed()
            }
        }
        Kind::Missing => Ok(Placed::Left),
        Kind::File if held.is_some() => match (judge(target, value, held), standing) {
            (Judged::Foreign, Standing::Vaults) => changed(),
            (Judged::Foreign, _) => Ok(Placed::Left),
            (Judged::Same, Standing::Vaults) => {
                narrow(target)?;
                Ok(Placed::Unchanged)
            }
            (Judged::Ours, Standing::Vaults) => {
                fsx::write_ordered(target, value, 0o600).map_err(|error| error.0)?;
                Ok(Placed::Written)
            }
            (Judged::Same | Judged::Ours, Standing::Own) => Ok(Placed::Withdrawn),
            (Judged::Same | Judged::Ours, Standing::Unseen) => Ok(Placed::Held),
        },
        _ if standing == Standing::Vaults => changed(),
        Kind::File | Kind::Symlink | Kind::Dir | Kind::Other => Ok(Placed::Left),
    }
}

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

fn adoptable(target: &Path, entry: &Entry, wrote: Option<&String>) -> bool {
    !entry.if_absent
        && wrote.is_none()
        && matches!(fsx::kind(target), Ok(Kind::File))
        && holds(target, entry.value.as_slice())
}

fn holds(target: &Path, value: &[u8]) -> bool {
    fsx::read_public(target, text::MAX_VALUE)
        .ok()
        .flatten()
        .is_some_and(|bytes| aterm_digest::ct_eq(SecretBytes::new(bytes).as_slice(), value))
}

fn judge(target: &Path, value: &[u8], held: Option<&String>) -> Judged {
    let len = std::fs::symlink_metadata(target).map_or(u64::MAX, |meta| meta.len());
    if len > u64::try_from(text::MAX_VALUE).unwrap_or(u64::MAX) {
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
        Ok(None) => Judged::Ours,
        Err(_) => Judged::Foreign,
    }
}

fn file_digest(target: &Path) -> Option<String> {
    let bytes = fsx::read_public(target, text::MAX_VALUE).ok()??;
    let bytes = SecretBytes::new(bytes);
    Some(digest_hex(bytes.as_slice()))
}

fn narrow(target: &Path) -> Result<(), String> {
    let at = |error: std::io::Error| format!("{}: {error}", target.display());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(target)
        .map_err(at)?;
    let meta = file.metadata().map_err(at)?;
    if meta.file_type().is_file() && meta.uid() == fsx::euid() && meta.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(at)?;
    }
    Ok(())
}

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

fn make_parents(home: &Path, rel: &str, own: Option<(u64, u64)>) -> Result<(), String> {
    for dir in parents(home, rel) {
        match fsx::kind(&dir).map_err(|error| error.0)? {
            Kind::Dir if own.is_some() && fsx::file_id(&dir) == own => {
                return Err(format!(
                    "{}: is aterm's own key and state directory; refusing to write in it",
                    dir.display()
                ));
            }
            Kind::Dir => {}
            Kind::Missing => match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
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

/// aterm's own key and state directory under `home` by (device, inode) ([`Places`]).
fn own_dir(home: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(crate::client::Paths::for_home(home).dir)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Whether home path `name`'s file is behind a symlinked directory ([`Places::linked`]).
fn linked(home: &Path, name: &str, own: Option<(u64, u64)>) -> bool {
    name.strip_prefix("~/").is_some_and(|rel| {
        refused_dir(home, rel, own)
            .is_some_and(|dir| fsx::kind(&dir).is_ok_and(|kind| kind == Kind::Symlink))
    })
}

/// Whether entry `name`'s file under `home` is behind a symlinked directory: a pass never
/// writes or looks through the link, and holds a file the vault wrote there as it was
/// ([`Placed::Held`]), so whoever checks the vault's files are intact does not judge it either.
pub fn behind_a_link(home: &Path, name: &str) -> bool {
    linked(home, name, own_dir(home))
}

/// Whether entry `name`'s file under `home` holds the value of manifest digest `digest`.
pub fn holds_digest(home: &Path, name: &str, digest: &str) -> bool {
    target_path(home, name)
        .ok()
        .and_then(|target| fsx::read_private(&target, text::MAX_VALUE).ok().flatten())
        .is_some_and(|bytes| digest_hex(bytes.as_slice()) == digest)
}

/// Whether entry `name`'s file under `home` is a gap a pass writes: missing, with no directory
/// on its way that a pass refuses (a symlink, or one it does not own) ([`Places::reachable`]).
pub fn is_gap(home: &Path, name: &str) -> bool {
    let reachable = name
        .strip_prefix("~/")
        .is_none_or(|rel| refused_dir(home, rel, own_dir(home)).is_none());
    reachable
        && target_path(home, name)
            .is_ok_and(|target| matches!(fsx::kind(&target), Ok(Kind::Missing)))
}

fn refused_dir(home: &Path, rel: &str, own: Option<(u64, u64)>) -> Option<PathBuf> {
    for dir in parents(home, rel) {
        match fsx::kind(&dir) {
            Ok(Kind::Dir) if own.is_none() || fsx::file_id(&dir) != own => {}
            Ok(Kind::Dir | Kind::Symlink) => return Some(dir),
            Ok(Kind::Missing | Kind::File | Kind::Other) | Err(_) => return None,
        }
    }
    None
}

fn under_a_file(home: &Path, name: &str) -> bool {
    name.strip_prefix("~/").is_some_and(|rel| {
        parents(home, rel)
            .iter()
            .any(|dir| matches!(fsx::kind(dir), Ok(Kind::File | Kind::Other)))
    })
}

fn sweep_temps(target: &Path, digest: &str) {
    let (Some(dir), Some(file)) = (target.parent(), target.file_name()) else {
        return;
    };
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(".{}.tmp.", file.to_string_lossy());
    for temp in listing
        .flatten()
        .filter(|item| item.file_name().to_string_lossy().starts_with(&prefix))
        .map(|item| item.path())
    {
        let linked = std::fs::symlink_metadata(&temp)
            .is_ok_and(|meta| meta.file_type().is_file() && meta.nlink() > 1);
        if linked {
            let _ = std::fs::remove_file(&temp);
        } else {
            let _ = fsx::wipe_if(&temp, text::MAX_VALUE, |bytes| digest_hex(bytes) == digest);
        }
    }
}

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

    #[test]
    fn commons_control_is_not_materialized_or_resumed_and_a_changed_copy_stays() {
        let root = scratch();
        let name = crate::commons::ENTRY;
        let entries = [
            entry("ordinary", b"ordinary value"),
            entry(name, b"inert control key"),
            entry("aterm-commons-control-v2", b"inert future control key"),
        ];
        let path = root.path().join(".secrets").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        plant(&path, b"an existing local file", 0o600);
        let held = Manifest {
            files: BTreeMap::from([(name.into(), digest_hex(b"what an older build wrote"))]),
        };
        let resumed = Pending::from([(name.into(), digest_hex(b"inert control key"))]);
        let (manifest, report) = apply_resuming(
            root.path(),
            &entries,
            &held,
            &resumed,
            1,
            &mut |held, pending| {
                assert_eq!(
                    held.files.get(name),
                    Some(&digest_hex(b"what an older build wrote")),
                    "saved ahead with the record, for a pass cut short"
                );
                assert!(!pending.contains_key(name));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(read(&path), b"an existing local file");
        assert_eq!(report.written, ["ordinary"]);
        assert_eq!(
            report.left,
            [name],
            "a copy changed since it was written stays"
        );
        assert!(
            report.removed.is_empty() && report.failed.is_empty() && report.moved_aside.is_empty()
        );
        assert!(!manifest.files.contains_key(name));
        for spelling in [
            name.to_owned(),
            name.to_uppercase(),
            format!("~/.secrets/{name}"),
            format!("~/.SECRETS/{}", name.to_uppercase()),
            "aterm-commons-control-v2".into(),
        ] {
            assert!(target_path(root.path(), &spelling).is_err(), "{spelling}");
        }
        let empty = scratch();
        apply(empty.path(), &entries[1..], &Manifest::default(), 2).unwrap();
        assert!(
            !empty.path().join(".secrets").join(name).exists(),
            "a control record never creates a plaintext key file"
        );
    }

    /// Reserved names a build that did not reserve them could write as files, by the spelling
    /// its manifest held (a `~/.secrets/` spelling was never a valid entry name).
    const RESERVED_COPIES: [&str; 3] = [
        "aterm-fleet-code",
        "ATERM-FLEET-CODE",
        crate::commons::ENTRY,
    ];

    #[test]
    fn a_reserved_file_an_older_build_wrote_is_removed_while_it_holds_what_it_wrote() {
        for name in RESERVED_COPIES {
            let root = scratch();
            let path = root.path().join(".secrets").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let held = Manifest {
                files: BTreeMap::from([
                    (name.into(), digest_hex(b"inert reserved value")),
                    ("ordinary".into(), digest_hex(b"ordinary value")),
                ]),
            };
            plant(
                &root.path().join(".secrets/ordinary"),
                b"ordinary value",
                0o600,
            );
            // Whether the vault still holds the entry (in place now) or dropped it, the file an
            // older build wrote goes, and the manifest forgets the name.
            for entries in [
                vec![
                    entry("ordinary", b"ordinary value"),
                    entry(name, b"inert reserved value"),
                ],
                vec![entry("ordinary", b"ordinary value")],
            ] {
                plant(&path, b"inert reserved value", 0o600);
                let (manifest, report) = apply(root.path(), &entries, &held, 1).unwrap();
                assert!(!path.exists(), "{name}: {report:?}");
                assert_eq!(report.removed, [name], "{name}");
                assert!(
                    report.failed.is_empty() && report.left.is_empty(),
                    "{name}: {report:?}"
                );
                assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["ordinary"]);
                assert_eq!(
                    read(&root.path().join(".secrets/ordinary")),
                    b"ordinary value"
                );
                let (again, report) = apply(root.path(), &entries, &manifest, 2).unwrap();
                assert_eq!(again, manifest, "{name}");
                assert!(
                    report.removed.is_empty() && report.left.is_empty(),
                    "{name}: {report:?}"
                );
            }
        }
    }

    #[test]
    fn a_reserved_file_the_person_changed_or_linked_stays_and_is_forgotten() {
        for name in RESERVED_COPIES {
            let root = scratch();
            let path = root.path().join(".secrets").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let held = Manifest {
                files: BTreeMap::from([(name.into(), digest_hex(b"inert reserved value"))]),
            };
            plant(&path, b"the person's own value", 0o600);
            let (manifest, report) = apply(root.path(), &[], &held, 1).unwrap();
            assert_eq!(read(&path), b"the person's own value", "{name}");
            assert_eq!(report.left, [name], "{name}");
            assert!(
                report.removed.is_empty() && report.failed.is_empty(),
                "{name}"
            );
            assert!(manifest.files.is_empty(), "{name}: {manifest:?}");
            // A link in the file's place is never followed, whatever it reaches.
            std::fs::remove_file(&path).unwrap();
            let reached = root.path().join("reached");
            plant(&reached, b"inert reserved value", 0o600);
            std::os::unix::fs::symlink(&reached, &path).unwrap();
            let (manifest, report) = apply(root.path(), &[], &held, 2).unwrap();
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(read(&reached), b"inert reserved value", "{name}");
            assert_eq!(report.left, [name], "{name}");
            assert!(manifest.files.is_empty(), "{name}: {manifest:?}");
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
        let (manifest, report) =
            apply(home.path(), &[entry("t", b"same")], &manifest, 2).expect("again");
        assert_eq!(report.unchanged, ["t"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn narrowing_never_follows_a_link_swapped_in_after_the_look() {
        let home = scratch();
        let elsewhere = home.path().join("elsewhere");
        plant(&elsewhere, b"same", 0o644);
        let link = home.path().join("t");
        std::os::unix::fs::symlink(&elsewhere, &link).expect("link");
        assert!(narrow(&link).is_err(), "a link is refused");
        assert_eq!(mode(&elsewhere), 0o644, "what it names keeps its mode");
        narrow(&elsewhere).expect("narrowed");
        assert_eq!(mode(&elsewhere), 0o600);
    }

    #[test]
    fn dropping_an_adopted_entry_leaves_the_machines_own_file() {
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
    fn a_dropped_file_whose_unlink_is_refused_keeps_its_bytes_for_the_next_pass() {
        let home = scratch();
        let name = "~/locked/hosts.yml";
        let (held, _) =
            apply(home.path(), &[entry(name, b"v")], &Manifest::default(), 1).expect("first");
        let locked = home.path().join("locked");
        let target = locked.join("hosts.yml");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock");
        let refused = apply(home.path(), &[], &held, 2);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");
        let (manifest, report) = refused.expect("applied");
        if fsx::euid() == 0 {
            assert_eq!(report.removed, [name]);
            return;
        }
        assert_eq!(failed_names(&report), [name]);
        assert_eq!(read(&target), b"v", "not zeroed while its name stays");
        assert_eq!(manifest, held, "still what the vault wrote");
        let (manifest, report) = apply(home.path(), &[], &manifest, 3).expect("next");
        assert_eq!(report.removed, [name]);
        assert!(report.left.is_empty() && report.failed.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!target.exists());
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

    #[test]
    fn a_dropped_name_whose_directory_is_now_a_file_is_forgotten() {
        let home = scratch();
        let name = "~/.config/foo/token";
        let (held, _) =
            apply(home.path(), &[entry(name, b"v")], &Manifest::default(), 1).expect("first");
        let foo = home.path().join(".config/foo");
        std::fs::remove_dir_all(&foo).expect("rm");
        plant(&foo, b"a file now", 0o600);
        let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
        assert_eq!(report, Report::default(), "nothing can be at that path");
        assert!(manifest.files.is_empty());
        assert_eq!(read(&foo), b"a file now");
    }

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
        assert!(manifest.files.is_empty(), "{:?}", manifest.files);
    }

    #[test]
    fn a_dropped_name_for_another_entrys_file_is_never_wiped() {
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
            assert!(report.moved_aside.is_empty(), "{:?}", report.moved_aside);
            assert!(report.removed.is_empty());
        } else {
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

    fn identity(value: &[u8]) -> [Entry; 3] {
        [
            anchored(KEY, value, TOML),
            anchored(TOML, value, PROFILE),
            anchored(PROFILE, value, KEY),
        ]
    }

    #[test]
    fn a_machines_own_machine_toml_keeps_the_vaults_key_and_profile_away() {
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
    fn a_temp_a_pass_cut_short_left_holding_its_value_is_wiped() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        let (held, _) =
            apply(home.path(), &[entry("t", b"v1")], &Manifest::default(), 1).expect("first");
        let cut = secrets.join(".t.tmp.4242.0badcafe");
        plant(&cut, b"v2", 0o600);
        plant(&secrets.join("f"), b"filled", 0o600);
        let linked = secrets.join(".f.tmp.4242.12345678");
        std::fs::hard_link(secrets.join("f"), &linked).expect("link");
        let other = secrets.join(".t.tmp.4343.deadbeef");
        plant(&other, b"another writer's", 0o600);
        let resumed = Pending::from([
            ("t".to_string(), digest_hex(b"v2")),
            ("f".to_string(), digest_hex(b"filled")),
        ]);
        let entries = [entry("t", b"v2"), gap("f", b"filled")];
        let (_, report) =
            apply_resuming(
                home.path(),
                &entries,
                &held,
                &resumed,
                2,
                &mut |_, _| Ok(()),
            )
            .expect("resumed");
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!cut.exists(), "the cut pass's copy of the value is gone");
        assert!(!linked.exists());
        assert_eq!(
            read(&secrets.join("f")),
            b"filled",
            "never zeroed through it"
        );
        assert_eq!(read(&other), b"another writer's");
        assert_eq!(read(&secrets.join("t")), b"v2");
    }

    #[test]
    fn a_cut_temp_that_names_another_file_loses_only_its_name() {
        let home = scratch();
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).expect("dir");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700)).expect("0700");
        plant(&secrets.join("t.tmp.1"), b"shared value", 0o600);
        let theirs = secrets.join(".t.tmp.1.tmp.4242.12345678");
        std::fs::hard_link(secrets.join("t.tmp.1"), &theirs).expect("link");
        plant(&secrets.join("g-kept"), b"shared value", 0o600);
        let moved = secrets.join(".g.tmp.4242.87654321");
        std::fs::hard_link(secrets.join("g-kept"), &moved).expect("link");
        let resumed = Pending::from(
            ["t", "t.tmp.1", "g"].map(|name| (name.to_string(), digest_hex(b"shared value"))),
        );
        let entries = [
            entry("t", b"shared value"),
            gap("t.tmp.1", b"shared value"),
            gap("g", b"shared value"),
        ];
        let (manifest, report) = apply_resuming(
            home.path(),
            &entries,
            &Manifest::default(),
            &resumed,
            2,
            &mut |_, _| Ok(()),
        )
        .expect("resumed");
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!theirs.exists() && !moved.exists());
        assert_eq!(read(&secrets.join("t.tmp.1")), b"shared value");
        assert_eq!(read(&secrets.join("g-kept")), b"shared value");
        assert!(report.left.is_empty(), "{:?}", report.left);
        assert_eq!(manifest.files.len(), 3);
    }

    #[test]
    fn a_gap_taken_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back() {
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        let (manifest, report) = apply_resuming(
            home.path(),
            &identity(b"shared"),
            &Manifest::default(),
            &Pending::new(),
            1,
            &mut |_, pending| {
                assert_eq!(pending.len(), 3);
                std::fs::create_dir(home.path().join(".aterm")).expect("dir");
                plant(&at(TOML), b"its own id", 0o600);
                Ok(())
            },
        )
        .expect("applied");
        assert_eq!(failed_names(&report), [TOML]);
        assert_eq!(read(&at(TOML)), b"its own id");
        assert!(!manifest.files.contains_key(TOML));

        let (manifest, report) =
            apply(home.path(), &identity(b"shared"), &manifest, 2).expect("next");
        assert_eq!(report.removed, [KEY, PROFILE]);
        assert_eq!(report.left, [TOML]);
        assert!(report.failed.is_empty() && report.written.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!at(KEY).exists() && !at(PROFILE).exists());
        assert_eq!(read(&at(TOML)), b"its own id");
    }

    #[test]
    fn a_file_the_vault_wrote_changed_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back()
     {
        let home = scratch();
        let at = |name: &str| target_path(home.path(), name).expect("valid");
        let (held, _) =
            apply(home.path(), &identity(b"v1"), &Manifest::default(), 1).expect("first");
        let (manifest, report) = apply_resuming(
            home.path(),
            &identity(b"v2"),
            &held,
            &Pending::new(),
            2,
            &mut |_, pending| {
                assert_eq!(pending.len(), 3);
                plant(&at(TOML), b"its own id", 0o600);
                Ok(())
            },
        )
        .expect("applied");
        assert_eq!(failed_names(&report), [TOML]);
        assert_eq!(read(&at(TOML)), b"its own id");

        let (manifest, report) = apply(home.path(), &identity(b"v2"), &manifest, 3).expect("next");
        assert_eq!(report.removed, [KEY, PROFILE]);
        assert_eq!(report.left, [TOML]);
        assert!(report.failed.is_empty() && report.written.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!at(KEY).exists() && !at(PROFILE).exists());
        assert_eq!(read(&at(TOML)), b"its own id");
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

        std::fs::remove_file(at(PROFILE)).expect("rm");
        let (manifest, report) =
            apply(home.path(), &identity(b"next"), &manifest, 2).expect("next");
        assert_eq!(report.written, [KEY, TOML, PROFILE]);
        assert_eq!(read(&at(PROFILE)), b"next");
        assert_eq!(manifest.files.len(), 3);

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

        let (manifest, report) = apply(home.path(), &identity(b"v3"), &manifest, 3).expect("again");
        assert_eq!(report.left, [KEY, TOML, PROFILE]);
        assert!(report.written.is_empty() && report.removed.is_empty());
        assert!(manifest.files.is_empty());
        assert!(!at(PROFILE).exists());

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
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.left, [anchor, "profile"]);
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
        assert_eq!(failed_names(&report), ["a", "b"]);
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
        assert_eq!(report.left, ["~/link/key", "c"]);
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
    fn aterms_own_directory_by_another_spelling_is_refused() {
        let home = scratch();
        let real = home.path().join("dotfiles/config");
        let own = real.join("aterm/secrets");
        std::fs::create_dir_all(&own).expect("dir");
        std::os::unix::fs::symlink(&real, home.path().join(".config")).expect("link");
        plant(&own.join("identity"), b"the machine key", 0o600);
        let name = "~/dotfiles/config/aterm/secrets/identity";
        let (manifest, report) = apply(
            home.path(),
            &[entry(name, b"v"), entry("~/dotfiles/x", b"v")],
            &Manifest::default(),
            1,
        )
        .expect("applied");
        assert_eq!(failed_names(&report), [name]);
        assert!(
            report.failed[0].1.contains("aterm's own"),
            "{:?}",
            report.failed
        );
        assert_eq!(report.written, ["~/dotfiles/x"]);
        assert!(report.moved_aside.is_empty());
        assert_eq!(read(&own.join("identity")), b"the machine key");
        assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["~/dotfiles/x"]);

        let held = Manifest {
            files: BTreeMap::from([(name.to_string(), digest_hex(b"the machine key"))]),
        };
        let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
        assert_eq!(report.left, [name]);
        assert_eq!(manifest, held);
        assert_eq!(read(&own.join("identity")), b"the machine key");
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

        std::fs::remove_file(&secrets).expect("unlink");
        std::fs::rename(&real, &secrets).expect("move back");
        let (last, report) = apply(home.path(), &entries, &manifest, 2).expect("reached");
        assert_eq!(report.removed, ["dropped"]);
        assert!(report.failed.is_empty() && report.left.is_empty());
        assert!(!secrets.join("dropped").exists());
        assert_eq!(read(&secrets.join("a")), b"v");
        assert!(!last.files.contains_key("dropped"));
    }

    /// An entry behind a symlinked directory is never written through it. A plain entry, and
    /// any entry in a symlinked `~/.secrets`, is a failure; an ` if-absent` home file there is
    /// the machine's own (a dotfiles setup's `~/.config`), left, and kept in the manifest
    /// when the vault wrote it.
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
        plant(&deep.join("d"), b"held", 0o600);
        let entries = [
            gap("gh_token", b"vault"),
            gap("~/a/b/c", b"vault"),
            gap("~/a/b/d", b"vault"),
            entry("~/link/x", b"v"),
            entry("~/target/x", b"v"),
        ];
        let held = Manifest {
            files: BTreeMap::from([("~/a/b/d".to_string(), digest_hex(b"held"))]),
        };
        let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
        assert_eq!(failed_names(&report), ["gh_token", "~/link/x"]);
        for ((_, why), link) in report.failed.iter().zip([".secrets", "link"]) {
            let link = home.path().join(link).display().to_string();
            assert!(why.contains(&link) && why.contains("symlink"), "{why}");
        }
        assert_eq!(report.left, ["~/a/b/c", "~/a/b/d"]);
        assert_eq!(report.unchanged, ["~/target/x"]);
        assert_eq!(
            manifest.files,
            BTreeMap::from([("~/a/b/d".to_string(), digest_hex(b"held"))]),
            "the vault's file behind the link stays the vault's"
        );
        assert_eq!(read(&real.join("gh_token")), b"theirs");
        assert_eq!(read(&deep.join("c")), b"theirs");
        assert_eq!(read(&deep.join("d")), b"held");
    }

    #[test]
    fn a_dropped_name_for_a_file_a_refused_entry_names_is_kept() {
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

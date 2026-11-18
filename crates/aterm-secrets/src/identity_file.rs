// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS MACHINE'S VAULT IDENTITY AT REST: the identity file (`~/.config/aterm/secrets/identity`,
//! one owner-only file of one link) that holds the machine's age key and the lab mark it was made
//! with (A12), as the client reads and makes it ([`read_marked`], [`machine_identity`]).
//!
//! - On Unix the file is the key's text, in age-keygen's form: comment lines (when it was made,
//!   its public key, a lab identity's mark) and the secret key on the last line that is no comment
//!   ([`plain_text`]).
//! - On Windows that same text is sealed to this Windows user by DPAPI
//!   ([`aterm_uds::dpapi::seal`], user scope, under this module's own entropy), and the file holds
//!   the comment lines a person reads and one line, [`SEALED`] and the sealed text in base64: a
//!   copy of the file taken to another user or another PC opens nothing. A file DPAPI can no
//!   longer open (the user's password reset by an administrator, the profile moved, the blob
//!   sealed under another entropy or altered) holds no key this user has: it reads as no identity,
//!   and the next identity made sets it aside (`identity.unsealable-<unix seconds>`) and makes a
//!   new key, which the vault admits as a new machine, never as a corrupt file. That holds only
//!   where DPAPI works in the logon session that reads it ([`aterm_uds::dpapi::unseal`] confirms
//!   a refusal with a probe): in one where it opens nothing (an OpenSSH key's logon, a scheduled
//!   task's that stores no password) the key reads as an error and nothing is set aside, and a
//!   making seals its new key, and writes it whole, before it sets anything aside. A plain key on
//!   Windows is refused: no build writes one there.

use crate::age::Identity;
use crate::{Error, LabMark, SecretBytes, fsx};
use std::path::Path;

/// The comment line that marks a lab identity in its file ([`crate::LabMark`]), followed by the
/// mark's word. A comment, so every build reads the key past it as before (the key is the last
/// line that is not one).
pub(crate) const LAB_LINE: &str = "# aterm lab identity: ";

/// The most bytes an identity file holds that a read of it takes.
pub(crate) const IDENTITY_MAX: usize = 16 << 10;

/// The word that starts a Windows identity file's sealed line, then one space and the DPAPI blob
/// of the key's text in padded base64.
#[cfg(windows)]
pub(crate) const SEALED: &str = "ATERM-DPAPI-SEALED-1";

/// The entropy every Windows identity file's blob is sealed under: a blob another program sealed
/// for this user under other entropy (or none) is no identity this module opens.
#[cfg(windows)]
const ENTROPY: &[u8] = b"aterm secrets machine identity v1";

/// The lock file beside the identity file a Windows identity's making holds ([`install`]): a
/// process that finds a file DPAPI cannot open sets it aside only while no other process is
/// making one, so none sets aside a key another just made.
#[cfg(windows)]
const MAKING_LOCK: &str = ".identity.lock";

/// How long a making waits for another process's making of this machine's key to finish: far
/// past the read, the seal and the one small write it makes.
#[cfg(windows)]
const MAKING_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The making lock, held exclusive and let go by an unlock as it drops, before its close.
#[cfg(windows)]
struct Held(std::fs::File);

#[cfg(windows)]
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The identity and lab mark the plain text `bytes` hold: the key on the last line that is no
/// comment, the mark a line starting [`LAB_LINE`]. `path` names the file in an error.
///
/// # Errors
/// The text is no UTF-8, holds no line that is no comment, or that line is no age identity.
pub(crate) fn plain_marked_in(
    path: &Path,
    bytes: &SecretBytes,
) -> crate::Result<(Identity, Option<LabMark>)> {
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("identity: not UTF-8"))?;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| Error::new("identity: no key in the file"))?;
    let mark = text.lines().map(str::trim).find_map(|line| {
        line.strip_prefix(LAB_LINE)
            .map(|rest| LabMark::of_word(rest.split_whitespace().next().unwrap_or("")))
    });
    let identity = Identity::parse(line)
        .map_err(|_| Error::new(format!("{}: not an age identity", path.display())))?;
    Ok((identity, mark))
}

/// The key's text for `identity` marked `mark`, made at `now` (Unix seconds): two comment lines
/// (when, and the public key), the mark's line where it has one, and the secret key.
///
/// The text is written into one buffer made at its whole length before the key goes in, so no
/// copy of the key is left behind in memory a growing buffer let go of, unwiped.
pub(crate) fn plain_text(identity: &Identity, mark: Option<LabMark>, now: u64) -> SecretBytes {
    let secret = identity.to_bech32();
    let head = format!(
        "# aterm secrets machine key: created {now} (unix), never copied off this machine.\n# public key: {}\n",
        identity.recipient().to_bech32()
    );
    let marked = mark.map(|mark| {
        format!(
            "{LAB_LINE}{} (it founds its own lab and never joins the live ALab)\n",
            mark.word()
        )
    });
    let whole = head.len() + marked.as_ref().map_or(0, String::len) + secret.len() + 1;
    let mut file = Vec::with_capacity(whole);
    file.extend_from_slice(head.as_bytes());
    if let Some(marked) = &marked {
        file.extend_from_slice(marked.as_bytes());
    }
    file.extend_from_slice(secret.as_slice());
    file.push(b'\n');
    debug_assert_eq!(
        file.capacity(),
        whole,
        "the key's text outgrew the buffer made for it, leaving a copy behind"
    );
    SecretBytes::new(file)
}

/// The identity and lab mark the identity file's `bytes` hold ([`plain_marked_in`] of the plain
/// text on Unix; on Windows of the text its sealed line opens to), `None` on Windows where DPAPI
/// cannot open that line for this user: a key this user no longer has.
///
/// # Errors
/// As [`plain_marked_in`]; on Windows also a file with no sealed line (a plain key among them),
/// a sealed line that is no base64, or DPAPI failing for any reason but a blob it cannot open.
pub(crate) fn marked_in(
    path: &Path,
    bytes: &SecretBytes,
) -> crate::Result<Option<(Identity, Option<LabMark>)>> {
    #[cfg(unix)]
    {
        plain_marked_in(path, bytes).map(Some)
    }
    #[cfg(windows)]
    {
        let Some(text) = unsealed(path, bytes)? else {
            return Ok(None);
        };
        plain_marked_in(path, &text).map(Some)
    }
}

/// The key's text the Windows identity file's `bytes` seal, `None` where DPAPI cannot open it for
/// this user ([`aterm_uds::dpapi::Unsealed::NotOpenable`]).
///
/// # Errors
/// As [`marked_in`]'s Windows errors.
#[cfg(windows)]
fn unsealed(path: &Path, bytes: &SecretBytes) -> crate::Result<Option<SecretBytes>> {
    use aterm_uds::dpapi::Unsealed;
    let at = |why: &str| Error::new(format!("{}: {why}", path.display()));
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("identity: not UTF-8"))?;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| Error::new("identity: no key in the file"))?;
    let Some(sealed) = line
        .strip_prefix(SEALED)
        .and_then(|rest| rest.strip_prefix(' '))
    else {
        return Err(at(
            "holds a key no Windows build sealed; aterm keeps a Windows machine's key sealed to \
             its user (DPAPI)",
        ));
    };
    let blob = crate::b64::decode_padded(sealed.trim())
        .ok_or_else(|| at("its sealed key is not base64"))?;
    match aterm_uds::dpapi::unseal(&blob, ENTROPY) {
        Ok(opened) => Ok(Some(SecretBytes::from(opened.expose()))),
        Err(Unsealed::NotOpenable(_)) => Ok(None),
        // A failure no stamp of the file decides: asked again, or in another session, DPAPI may
        // open it, so a trace scope armed around the read records a retry.
        Err(Unsealed::Failed(e)) => {
            let failed = at(&e.to_string());
            aterm_uds::trace::retry(failed.to_string());
            Err(failed)
        }
    }
}

/// The identity file's text for `identity` marked `mark`, made at `now`: [`plain_text`] on Unix;
/// on Windows its comment lines (when, the public key, that it is sealed) and its sealed line,
/// the plain text sealed to this user by DPAPI.
///
/// # Errors
/// On Windows, DPAPI's.
pub(crate) fn stored_text(
    identity: &Identity,
    mark: Option<LabMark>,
    now: u64,
) -> crate::Result<SecretBytes> {
    let plain = plain_text(identity, mark, now);
    #[cfg(unix)]
    {
        Ok(plain)
    }
    #[cfg(windows)]
    {
        let blob = aterm_uds::dpapi::seal(plain.as_slice(), ENTROPY)
            .map_err(|e| Error::new(format!("sealing the machine key: {e}")))?;
        Ok(SecretBytes::new(
            format!(
                "# aterm secrets machine key: created {now} (unix), never copied off this machine.\n\
                 # public key: {}\n\
                 # sealed to this Windows user by DPAPI: only this user on this PC opens it.\n\
                 {SEALED} {}\n",
                identity.recipient().to_bech32(),
                crate::b64::encode_padded(&blob)
            )
            .into_bytes(),
        ))
    }
}

/// This machine's identity and its lab mark, read together from one read of the identity file at
/// `path` ([`marked_in`]); `None` where there is none, or (Windows) where it holds a key this user
/// no longer has.
///
/// # Errors
/// The file cannot be read privately ([`fsx::read_private`]), or as [`marked_in`].
pub(crate) fn read_marked(path: &Path) -> crate::Result<Option<(Identity, Option<LabMark>)>> {
    let Some(bytes) = fsx::read_private(path, IDENTITY_MAX)? else {
        return Ok(None);
    };
    marked_in(path, &bytes)
}

/// Write `identity` as this machine's key at `path` with its lab mark, `mark` (A12), made at `now`:
/// written with the key, in one new file, so no identity is ever without the mark it was made
/// with, and none gains one later. Where another process wrote the file first, its key and mark
/// win.
///
/// # Errors
/// The write fails, or a file at `path` holds no key this reads.
#[cfg(unix)]
pub(crate) fn install(
    path: &Path,
    identity: Identity,
    mark: Option<LabMark>,
    now: u64,
) -> crate::Result<Identity> {
    let file = stored_text(&identity, mark, now)?;
    if fsx::write_new(path, file.as_slice(), 0o600)? {
        return Ok(identity);
    }
    taken_by(path)
}

/// [`install`] on Windows, holding the making lock ([`MAKING_LOCK`]) beside `path`: a key there
/// already wins, and a file there DPAPI cannot open for this user is set aside
/// (`identity.unsealable-<now>`), so the new key takes its name.
///
/// Nothing is set aside before the new key is sealed and written whole beside it
/// ([`fsx::write_new_staged`]): a seal that fails (DPAPI in a logon that holds no password) or a
/// write that fails changes nothing, and the file is set aside only where DPAPI, which opens a
/// probe in this session, cannot open it ([`aterm_uds::dpapi::Unsealed::NotOpenable`]); in a
/// session where DPAPI opens nothing the read is an error and nothing is touched.
///
/// # Errors
/// The seal fails, the lock cannot be made or taken, the file there cannot be read or set aside,
/// or as the Unix arm's.
#[cfg(windows)]
pub(crate) fn install(
    path: &Path,
    identity: Identity,
    mark: Option<LabMark>,
    now: u64,
) -> crate::Result<Identity> {
    let file = stored_text(&identity, mark, now)?;
    let lock_path = path.with_file_name(MAKING_LOCK);
    let lock = fsx::open_lock(&lock_path)?;
    aterm_uds::file_lock::lock_exclusive_until(
        &lock,
        std::time::Instant::now() + MAKING_WAIT,
        "another process is making this machine's key",
    )
    .map_err(|e| Error::new(format!("{}: {e}", lock_path.display())))?;
    let _held = Held(lock);
    let unsealable = match fsx::read_private(path, IDENTITY_MAX)? {
        Some(bytes) => match marked_in(path, &bytes)? {
            Some((held, _)) => return Ok(held),
            None => true,
        },
        None => false,
    };
    let placed = if unsealable {
        fsx::write_new_staged(path, file.as_slice(), 0o600, || set_aside(path, now))?
    } else {
        fsx::write_new(path, file.as_slice(), 0o600)?
    };
    if placed {
        return Ok(identity);
    }
    taken_by(path)
}

/// The key a file another writer put at `path` first holds, as [`install`] answers it.
///
/// # Errors
/// It holds none this reads.
fn taken_by(path: &Path) -> crate::Result<Identity> {
    read_marked(path)?
        .map(|(identity, _)| identity)
        .ok_or_else(|| Error::new("the machine key changed while it was being created; try again"))
}

/// Set the identity file at `path`, which DPAPI cannot open for this user, aside beside it as
/// `<name>.unsealable-<now>` (`.<n>` after it where that is taken), never replacing anything, and
/// say so in the log: the file is kept, since a person whose old password comes back may want
/// its key.
///
/// # Errors
/// No free name, or the rename's.
#[cfg(windows)]
fn set_aside(path: &Path, now: u64) -> crate::Result<()> {
    let name = path
        .file_name()
        .map_or_else(|| "identity".into(), |name| name.to_string_lossy());
    for n in 0..1000_u32 {
        let aside = path.with_file_name(if n == 0 {
            format!("{name}.unsealable-{now}")
        } else {
            format!("{name}.unsealable-{now}.{n}")
        });
        if fsx::kind(&aside)? != fsx::Kind::Missing {
            continue;
        }
        fsx::rename_noreplace(path, &aside)?;
        aterm_log::info!(
            "secrets: {} holds a machine key DPAPI no longer opens for this user; kept as {}, and \
             a new key made, which the vault admits as a new machine",
            path.display(),
            aside.display()
        );
        return Ok(());
    }
    Err(Error::new(format!(
        "{}: no free name to set it aside under",
        path.display()
    )))
}

/// This machine's key at `path` in the vault folder `dir`, made there if there is none (Windows:
/// none this user can open) with the lab mark `mark` at `now` ([`install`]).
///
/// # Errors
/// The file cannot be read or holds no key; the folder cannot be made private; or as [`install`].
pub(crate) fn machine_identity(
    dir: &Path,
    path: &Path,
    mark: Option<LabMark>,
    now: u64,
) -> crate::Result<Identity> {
    if let Some((identity, _)) = read_marked(path)? {
        return Ok(identity);
    }
    fsx::ensure_private_dir(dir)?;
    install(path, Identity::generate()?, mark, now)
}

#[cfg(test)]
#[path = "identity_file_tests.rs"]
mod tests;

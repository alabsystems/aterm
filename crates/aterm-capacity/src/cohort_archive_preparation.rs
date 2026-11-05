// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unpublished fixed-body gate preparation. No partial directory is an archive slot.
//! Only this known constant may be completed; existing final slots are never repaired.

use super::*;

pub(super) struct Published {
    directory: File,
    gate: File,
    pub(super) stamp: Stamp,
    id: Value,
    origin: DirOrigin,
}

impl Published {
    pub(super) fn recheck(&self, parent: &File, key: &str) -> io::Result<()> {
        if identity(&open(parent, key, false, true)?, true)? != self.id
            || identity(&self.directory, true)? != self.id
            || directory_origin(&self.directory)? != self.origin
            || Stamp::read(&self.gate)? != self.stamp
            || Stamp::read(&open(&self.directory, GATE, false, false)?)? != self.stamp
        {
            return Err(bad(
                "published archive gate lost its original preparation origin",
            ));
        }
        Ok(())
    }
}

fn name(key: &str) -> String {
    format!(".preparing-{key}")
}

fn unchanged(
    parent: &File,
    named: &str,
    directory: &File,
    id: &Value,
    origin: DirOrigin,
) -> io::Result<()> {
    if identity(&open(parent, named, false, true)?, true)? != *id
        || identity(directory, true)? != *id
        || directory_origin(directory)? != origin
    {
        return Err(bad("unpublished archive directory changed physical origin"));
    }
    let mut seen = false;
    for item in fd::Dir::read_from(directory.as_fd())? {
        let item = item?;
        let child = item
            .file_name()
            .to_str()
            .map_err(|_| bad("non-UTF8 unpublished archive child"))?;
        if matches!(child, "." | "..") {
            continue;
        }
        if child != GATE || seen {
            return Err(bad("unknown unpublished archive data is preserved"));
        }
        seen = true;
    }
    if directory_origin(directory)? != origin {
        return Err(bad("unpublished archive inventory changed during read"));
    }
    Ok(())
}

pub(super) fn publish(parent: &File, key: &str) -> io::Result<Published> {
    let temporary = name(key);
    match fd::mkdirat(parent, temporary.as_str(), Mode::RWXU) {
        Ok(()) => {}
        Err(error) if io::Error::from(error).kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let directory = open(parent, &temporary, false, true)?;
    let id = identity(&directory, true)?;
    let mut origin = directory_origin(&directory)?;
    unchanged(parent, &temporary, &directory, &id, origin)?;
    #[cfg(test)]
    boundary("before-gate-create")?;
    let (gate, text, before) = match open(&directory, GATE, false, false) {
        Ok(file) => {
            let (text, before) = bytes(&file, GATE_BODY.len())?;
            let mode = before.mode() & 0o7777;
            if !GATE_BODY.starts_with(text.as_str()) || !matches!(mode, 0o400 | 0o600) {
                return Err(bad(
                    "unpublished gate is not a private non-executable constant prefix",
                ));
            }
            unchanged(parent, &temporary, &directory, &id, origin)?;
            if text.len() != GATE_BODY.len() && mode != 0o600 {
                return Err(bad("a partial sealed gate is not an owned write prefix"));
            }
            if mode == 0o600 {
                let writer = File::from(fd::openat(
                    &directory,
                    GATE,
                    OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                )?);
                if Stamp::read(&writer)? != before
                    || Stamp::read(&file)? != before
                    || Stamp::read(&open(&directory, GATE, false, false)?)? != before
                {
                    return Err(bad(
                        "unpublished gate changed before its exact-prefix continuation",
                    ));
                }
                (writer, text, before)
            } else {
                (file, text, before)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            unchanged(parent, &temporary, &directory, &id, origin)?;
            let file = open(&directory, GATE, true, false)?;
            // Record the exact owned create before any file write or covering I/O.
            origin = directory_origin(&directory)?;
            fd::fchmod(&file, Mode::RUSR | Mode::WUSR)?;
            let before = Stamp::read(&file)?;
            #[cfg(test)]
            boundary("gate-created")?;
            (file, String::new(), before)
        }
        Err(error) => return Err(error),
    };
    unchanged(parent, &temporary, &directory, &id, origin)?;
    if text.len() < GATE_BODY.len() {
        #[cfg(test)]
        if INTERRUPT.with(|slot| {
            if slot.get() == Some("gate-short-write") {
                slot.set(None);
                true
            } else {
                false
            }
        }) {
            let remaining = &GATE_BODY.as_bytes()[text.len()..];
            let cut = (remaining.len() / 2).max(1);
            gate.write_all_at(&remaining[..cut], text.len() as u64)?;
            return Err(io::Error::from_raw_os_error(28));
        }
        gate.write_all_at(&GATE_BODY.as_bytes()[text.len()..], text.len() as u64)?;
    }
    let (complete, after) = bytes(&gate, GATE_BODY.len())?;
    if complete != GATE_BODY || after.0[..2] != before.0[..2] || after.0[3..6] != before.0[3..6] {
        return Err(bad(
            "unpublished continuation changed its original gate or constant bytes",
        ));
    }
    #[cfg(test)]
    boundary("gate-complete")?;
    if gate.metadata()?.mode() & 0o7777 == 0o600 {
        fd::fchmod(&gate, Mode::RUSR)?
    }
    let stamp = Stamp::read(&gate)?;
    #[cfg(test)]
    boundary("gate-sealed")?;
    unchanged(parent, &temporary, &directory, &id, origin)?;
    cover_frozen(&directory, GATE, &gate, GATE_BODY, &stamp)?;
    unchanged(parent, &temporary, &directory, &id, origin)?;
    #[cfg(test)]
    boundary("gate-covered")?;
    if Stamp::read(&gate)? != stamp || Stamp::read(&open(&directory, GATE, false, false)?)? != stamp
    {
        return Err(bad(
            "unpublished gate changed before its known directory publication",
        ));
    }
    fd::renameat_with(
        parent,
        temporary.as_str(),
        parent,
        key,
        RenameFlags::NOREPLACE,
    )?;
    // Only the directory's known rename origin changes; the SAME gate file stays frozen.
    let origin = directory_origin(&directory)?;
    let published = Published {
        directory,
        gate,
        stamp,
        id,
        origin,
    };
    published.recheck(parent, key)?;
    #[cfg(test)]
    boundary("slot-published")?;
    aterm_durable::durable(parent)?;
    published.recheck(parent, key)?;
    #[cfg(test)]
    boundary("slot-parent-covered")?;
    Ok(published)
}

#[cfg(test)]
thread_local! {static INTERRUPT:std::cell::Cell<Option<&'static str>>=const {std::cell::Cell::new(None)};}

#[cfg(test)]
pub(super) fn interrupt(stage: &'static str) {
    INTERRUPT.with(|slot| slot.set(Some(stage)));
}

#[cfg(test)]
fn boundary(stage: &str) -> io::Result<()> {
    if INTERRUPT.with(|slot| {
        if slot.get() == Some(stage) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other(
            "injected unpublished archive gate interruption",
        ));
    }
    Ok(())
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private session placement and read-only AppKit focus evidence, never a focus lease.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pub pid: u32,
    pub parent: u32,
    pub uid: u32,
    pub start_s: u64,
    pub start_us: u32,
    pub executable: PathBuf,
}

fn process_sample(
    record: &aterm_sysprobe::kproc::Record,
    path: Option<PathBuf>,
    pid: u32,
    parent: u32,
    executable: &Path,
) -> Option<Process> {
    if record.pid != pid
        || record.ppid != parent
        || record.uid != crate::sys::getuid()
        || record.zombie
        || record.start_s == 0
        || record.start_us >= 1_000_000
        || path.as_deref() != Some(executable)
    {
        return None;
    }
    Some(Process {
        pid,
        parent,
        uid: record.uid,
        start_s: record.start_s,
        start_us: record.start_us,
        executable: path?,
    })
}

/// Observe the owned child's original kernel birth and image; this is not a process lease.
pub(super) fn process(pid: u32, parent: u32, executable: &Path) -> Result<Process, String> {
    let read = || {
        aterm_sysprobe::kproc::record_read(pid)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "owned application process disappeared".to_owned())
    };
    let before = read()?;
    let path = aterm_sysprobe::process_path(pid);
    let after = read()?;
    let sampled = process_sample(&before, path.clone(), pid, parent, executable)
        .ok_or("owned application birth, parent or image is unconfirmed")?;
    if process_sample(&after, path, pid, parent, executable).as_ref() != Some(&sampled) {
        return Err("owned application identity changed during observation".into());
    }
    Ok(sampled)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Session {
    pub sid: String,
    pub nonce: String,
}

fn field<'a>(fields: &[&'a str], prefix: &str) -> Result<&'a str, String> {
    let mut values = fields.iter().filter_map(|word| word.strip_prefix(prefix));
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing activation field {prefix}"))?;
    if values.next().is_some() {
        return Err(format!("ambiguous activation field {prefix}"));
    }
    Ok(value)
}

/// Admit the one live terminal in the already verified logical window, without guessing a SID.
pub(super) fn session(roster: &str, logical_window: u64) -> Result<Session, String> {
    let mut selected = None;
    for line in roster.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields
            .first()
            .is_none_or(|local| local.parse::<u64>().is_err())
        {
            continue;
        }
        if fields.get(3) != Some(&"alive")
            || field(&fields, "window=")?.parse::<u64>().ok() != Some(logical_window)
        {
            continue;
        }
        if selected.is_some() {
            return Err("private logical window has more than one live terminal".into());
        }
        let sid = fields.get(1).copied().ok_or("missing private session ID")?;
        let nonce = field(&fields, "nonce=")?;
        if crate::stub::first_sid(sid) != Some(sid)
            || nonce.len() != 32
            || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            || field(&fields, "active=")? != "1"
        {
            return Err("private live terminal identity or active placement is unconfirmed".into());
        }
        selected = Some(Session {
            sid: sid.into(),
            nonce: nonce.into(),
        });
    }
    selected.ok_or_else(|| "no live terminal in the verified private logical window".into())
}

pub(super) struct Focus {
    /// AppKit windowNumber, independently named from the logical chrome WindowId.
    pub window: Option<u64>,
    active: bool,
    key: bool,
}

impl Focus {
    pub fn ready(&self, native_window: Option<u64>) -> bool {
        self.window
            .is_some_and(|window| native_window.is_none_or(|expected| window == expected))
            && self.active
            && self.key
    }
}

pub(super) fn focus(body: &str) -> Result<Focus, String> {
    let mut lines = body.lines();
    let fields: Vec<_> = lines
        .next()
        .ok_or("missing native focus readback")?
        .split_whitespace()
        .collect();
    if lines.next().is_some() || fields.first() != Some(&"OK") || fields.len() != 7 {
        return Err("invalid native focus readback".into());
    }
    field(&fields, "posted_keys=")?
        .parse::<u64>()
        .map_err(|_| "invalid observed posted-key count")?;
    let window = field(&fields, "window=")?
        .parse::<u64>()
        .map_err(|_| "invalid AppKit window number")?;
    let active = field(&fields, "app_active=")?;
    let key = field(&fields, "window_key=")?;
    let ready = field(&fields, "key=")?;
    match field(&fields, "available=")? {
        "0" if window == 0 && active == "-" && key == "-" && ready == "-" => Ok(Focus {
            window: None,
            active: false,
            key: false,
        }),
        "1" if window != 0
            && matches!(active, "0" | "1")
            && matches!(key, "0" | "1")
            && ready
                == if active == "1" && key == "1" {
                    "1"
                } else {
                    "0"
                } =>
        {
            Ok(Focus {
                window: Some(window),
                active: active == "1",
                key: key == "1",
            })
        }
        _ => Err("inconsistent native focus readback".into()),
    }
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod tests;

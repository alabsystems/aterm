// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private v2 observations. Parsing data does not construct retirement authority.

use aterm_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) const FRAME: usize = 16 * 1024;
pub(super) const BUFFER: usize = 64 * 1024;

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid private epoch frame")
}

fn fields(value: &Value, names: &[&str]) -> io::Result<()> {
    value
        .as_object()
        .filter(|object| {
            object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
        })
        .map(|_| ())
        .ok_or_else(bad)
}

fn number(value: &Value, key: &str) -> io::Result<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(bad)
}

fn path(value: &Value, key: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(value.get(key).and_then(Value::as_str).ok_or_else(bad)?);
    if !path.is_absolute() {
        return Err(bad());
    }
    Ok(path)
}

pub(super) enum Event {
    Opened {
        backing: u32,
        roots: u32,
        published: u32,
        fifo: PathBuf,
    },
    Spawned {
        id: u32,
        pid: u32,
    },
    RootExit {
        id: u32,
        pid: u32,
        code: Option<i32>,
        success: bool,
    },
    Closing,
    Retired {
        withdrawn: u32,
        fifo: PathBuf,
    },
}

pub(super) fn decode(bytes: &[u8], epoch: &str) -> io::Result<Event> {
    if bytes.is_empty() || bytes.len() > FRAME {
        return Err(bad());
    }
    let value: Value = aterm_json::from_slice(bytes).map_err(|_| bad())?;
    // Both the qualified helper and aterm's writer emit sorted compact objects.
    // Exact round-trip rejects duplicate keys lost by an untyped Value map,
    // without introducing a second JSON parser or public authority constructor.
    if aterm_json::to_vec(&value).map_err(|_| bad())? != bytes {
        return Err(bad());
    }
    if value.get("v").and_then(Value::as_u64) != Some(2)
        || value.get("epoch").and_then(Value::as_str) != Some(epoch)
    {
        return Err(bad());
    }
    match value.get("event").and_then(Value::as_str) {
        Some("opened") => {
            fields(
                &value,
                &[
                    "v",
                    "epoch",
                    "event",
                    "backing",
                    "roots",
                    "published",
                    "fifo",
                ],
            )?;
            Ok(Event::Opened {
                backing: number(&value, "backing")?,
                roots: number(&value, "roots")?,
                published: number(&value, "published")?,
                fifo: path(&value, "fifo")?,
            })
        }
        Some("spawned") => {
            fields(&value, &["v", "epoch", "event", "id", "pid"])?;
            let pid = number(&value, "pid")?;
            if pid == 0 {
                return Err(bad());
            }
            Ok(Event::Spawned {
                id: number(&value, "id")?,
                pid,
            })
        }
        Some("root-exit") => {
            fields(
                &value,
                &["v", "epoch", "event", "id", "pid", "code", "success"],
            )?;
            let pid = number(&value, "pid")?;
            let success = value
                .get("success")
                .and_then(Value::as_bool)
                .ok_or_else(bad)?;
            let code = match value.get("code") {
                Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(bad)?,
                ),
                None => return Err(bad()),
            };
            if pid == 0 || success != (code == Some(0)) {
                return Err(bad());
            }
            Ok(Event::RootExit {
                id: number(&value, "id")?,
                pid,
                code,
                success,
            })
        }
        Some("closing") => {
            fields(&value, &["v", "epoch", "event"])?;
            Ok(Event::Closing)
        }
        Some("retired") => {
            fields(
                &value,
                &["v", "epoch", "event", "withdrawn", "fifo", "scope"],
            )?;
            if value.get("scope").and_then(Value::as_str) != Some("owned-roots-and-explicit-tokens")
            {
                return Err(bad());
            }
            Ok(Event::Retired {
                withdrawn: number(&value, "withdrawn")?,
                fifo: path(&value, "fifo")?,
            })
        }
        _ => Err(bad()),
    }
}

fn encode(value: Value) -> io::Result<Vec<u8>> {
    let mut bytes = aterm_json::to_vec(&value).map_err(|_| bad())?;
    bytes.push(b'\n');
    if bytes.len() > FRAME {
        return Err(bad());
    }
    Ok(bytes)
}

pub(super) fn spawn(epoch: &str, id: u32, manifest: &Path, target: &Path) -> io::Result<Vec<u8>> {
    let manifest = manifest
        .to_str()
        .filter(|_| manifest.is_absolute())
        .ok_or_else(bad)?;
    let target = target
        .to_str()
        .filter(|_| target.is_absolute())
        .ok_or_else(bad)?;
    encode(json!({"v":2,"epoch":epoch,"op":"spawn","id":id,"manifest":manifest,"target":target}))
}

pub(super) fn close(epoch: &str) -> io::Result<Vec<u8>> {
    encode(json!({"v":2,"epoch":epoch,"op":"close"}))
}

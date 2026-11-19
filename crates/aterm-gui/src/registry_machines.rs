// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The inventory reader runs off the UI thread. An explicit open or refresh runs at the
//! responsive role; a restore or automatic refresh runs in the background.

pub(crate) use aterm_db::alab::view::inventory::Machine;
#[cfg(test)]
pub(crate) use aterm_db::alab::view::inventory::Observation;
use std::sync::{Arc, mpsc};

pub(crate) type Inventory = Result<Vec<Machine>, ReadError>;

#[derive(Debug)]
pub(crate) enum ReadError {
    Unavailable(String),
    #[cfg(unix)]
    Store(aterm_db::Error),
    #[cfg(unix)]
    Incomplete,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => f.write_str(message),
            #[cfg(unix)]
            Self::Store(error) => std::fmt::Display::fmt(error, f),
            #[cfg(unix)]
            Self::Incomplete => f.write_str("Registry history is still arriving on this machine"),
        }
    }
}

pub(crate) fn load(
    paths: Option<crate::tables_store::Paths>,
    role: crate::qos::Role,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<mpsc::Receiver<Inventory>, ReadError> {
    let (send, receive) = mpsc::channel();
    crate::qos::spawn_lane_thread("aterm-aregistry", role, move || {
        let result = paths
            .map_or_else(crate::tables_store::live, Ok)
            .map_err(ReadError::Unavailable)
            .and_then(read);
        if send.send(result).is_ok() {
            wake();
        }
    })
    .map_err(|error| ReadError::Unavailable(error.to_string()))?;
    Ok(receive)
}

#[cfg(unix)]
fn read(paths: crate::tables_store::Paths) -> Inventory {
    let view = aterm_db::alab::Registry::at(paths)
        .view()
        .map_err(ReadError::Store)?;
    if !view.complete {
        return Err(ReadError::Incomplete);
    }
    Ok(aterm_db::alab::view::inventory::machines(&view))
}

#[cfg(not(unix))]
fn read(paths: crate::tables_store::Paths) -> Inventory {
    match paths {}
}

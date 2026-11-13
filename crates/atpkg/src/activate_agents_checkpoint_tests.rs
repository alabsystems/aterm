// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::store::Layout;

pub(crate) enum Event<'a> {
    Read(&'a str),
    Written(&'a str),
    Waiting(&'a str),
}

type Hook = Arc<dyn Fn(Event<'_>) + Send + Sync>;
static HOOKS: Mutex<BTreeMap<PathBuf, Hook>> = Mutex::new(BTreeMap::new());

pub(crate) fn at(layout: &Layout, event: Event<'_>) {
    let hook = HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&layout.prefix)
        .cloned();
    if let Some(hook) = hook {
        hook(event);
    }
}

pub(crate) fn watch(layout: &Layout, hook: Hook) -> Watch {
    assert!(
        HOOKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(layout.prefix.clone(), hook)
            .is_none()
    );
    Watch(layout.prefix.clone())
}

pub(crate) fn waiting(path: &std::path::Path) {
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return;
    };
    let hooks: Vec<Hook> = HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(prefix, _)| path.parent() == Some(prefix.join("bin").as_path()))
        .map(|(_, hook)| hook.clone())
        .collect();
    for hook in hooks {
        hook(Event::Waiting(name));
    }
}

pub(crate) struct Watch(PathBuf);

impl Drop for Watch {
    fn drop(&mut self) {
        HOOKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

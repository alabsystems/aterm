// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Closed retained-controller requests. These are selectors and callback DATA;
//! the original Native service/constructor/controller owners decide every effect.
use std::collections::BTreeMap;
use std::path::PathBuf;

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn number(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) controller_operation: String,
    pub(crate) action: Action,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Frontier {
    pub(crate) service: String,
    pub(crate) go: Option<String>,
    pub(crate) frontier: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) service: String,
    pub(crate) source: String,
    pub(crate) selection: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SelectionFrontier {
    pub(crate) selection: Selection,
    pub(crate) go: Option<String>,
    pub(crate) frontier: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Prepare {
        service: String,
        source_build: u64,
        target_build: u64,
        settings: PathBuf,
        input: PathBuf,
        command: PathBuf,
    },
    Enter {
        service: String,
        go: String,
        callback: PathBuf,
    },
    InitialBegin {
        service: String,
    },
    ReplacementBegin {
        service: String,
        go: String,
    },
    UiAck(Frontier),
    FrontierAck {
        nonce: String,
    },
    Origin {
        attachment: String,
    },
    InitialActivation(Frontier),
    Activation(Frontier),
    Commit(Frontier),
    Promote(Frontier),
    AppCurrent {
        app_owner: String,
    },
    Restoration {
        service: String,
        go: String,
        old_attachment: String,
        new_attachment: String,
        frontier: String,
    },
    Register {
        service: String,
        source: String,
        attachment: String,
    },
    Inhibit(Selection),
    Retire(Selection),
    CancelUnaccepted {
        service: String,
        source: String,
        attachment: String,
    },
    SelectionBegin {
        selection: Selection,
        go: Option<String>,
    },
    SelectionUiAck(SelectionFrontier),
    SelectionObserve(SelectionFrontier),
    SelectionCommit(SelectionFrontier),
    SelectionPromote(SelectionFrontier),
}
impl Action {
    pub(super) fn service(&self) -> Option<&str> {
        match self {
            Self::Prepare { service, .. }
            | Self::Enter { service, .. }
            | Self::InitialBegin { service }
            | Self::ReplacementBegin { service, .. }
            | Self::Register { service, .. }
            | Self::CancelUnaccepted { service, .. }
            | Self::Restoration { service, .. } => Some(service),
            Self::UiAck(frontier)
            | Self::InitialActivation(frontier)
            | Self::Activation(frontier)
            | Self::Commit(frontier)
            | Self::Promote(frontier) => Some(&frontier.service),
            Self::Inhibit(selection) | Self::Retire(selection) => Some(&selection.service),
            Self::SelectionBegin { selection, .. } => Some(&selection.service),
            Self::SelectionUiAck(frontier)
            | Self::SelectionObserve(frontier)
            | Self::SelectionCommit(frontier)
            | Self::SelectionPromote(frontier) => Some(&frontier.selection.service),
            Self::FrontierAck { .. } | Self::Origin { .. } | Self::AppCurrent { .. } => None,
        }
    }
}
fn key(fields: &mut BTreeMap<&str, &str>, name: &str) -> Option<String> {
    let value = fields.remove(name)?;
    hex(value, 32).then(|| value.into())
}
fn optional_key(fields: &mut BTreeMap<&str, &str>, name: &str) -> Option<Option<String>> {
    fields.remove(name).map_or(Some(None), |value| {
        hex(value, 32).then(|| Some(value.into()))
    })
}
fn path(fields: &mut BTreeMap<&str, &str>, name: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let value = fields.remove(name)?;
    if value.is_empty() || !value.len().is_multiple_of(2) || !hex(value, value.len()) {
        return None;
    }
    let bytes = aterm_codec::hex::decode_vec(value.as_bytes())?;
    if bytes.contains(&0) {
        return None;
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}
fn frontier(fields: &mut BTreeMap<&str, &str>, replacement: bool) -> Option<Frontier> {
    Some(Frontier {
        service: key(fields, "service")?,
        go: if replacement {
            Some(key(fields, "go")?)
        } else {
            None
        },
        frontier: key(fields, "frontier")?,
    })
}
fn selection(fields: &mut BTreeMap<&str, &str>) -> Option<Selection> {
    Some(Selection {
        service: key(fields, "service")?,
        source: key(fields, "source_operation")?,
        selection: key(fields, "selection")?,
    })
}
fn selection_frontier(fields: &mut BTreeMap<&str, &str>) -> Option<SelectionFrontier> {
    Some(SelectionFrontier {
        selection: selection(fields)?,
        go: optional_key(fields, "go")?,
        frontier: key(fields, "frontier")?,
    })
}
impl Request {
    pub(crate) fn parse(name: &str, fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        let controller_operation = key(fields, "controller_operation")?;
        let action = match name {
            "controller-retained-prepare" => {
                let service = key(fields, "service")?;
                let source_build = number(fields.remove("source_build")?)?;
                let target_build = number(fields.remove("target_build")?)?;
                if source_build == 0 || target_build == 0 {
                    return None;
                }
                Action::Prepare {
                    service,
                    source_build,
                    target_build,
                    settings: path(fields, "settings")?,
                    input: path(fields, "input")?,
                    command: path(fields, "command")?,
                }
            }
            "controller-retained-enter" => Action::Enter {
                service: key(fields, "service")?,
                go: key(fields, "go")?,
                callback: path(fields, "callback")?,
            },
            "controller-retained-initial-begin" => Action::InitialBegin {
                service: key(fields, "service")?,
            },
            "controller-retained-frontier-begin" => Action::ReplacementBegin {
                service: key(fields, "service")?,
                go: key(fields, "go")?,
            },
            "controller-retained-initial-ui-ack" => Action::UiAck(frontier(fields, false)?),
            "controller-retained-ui-ack" => Action::UiAck(frontier(fields, true)?),
            "controller-retained-frontier-ack" => Action::FrontierAck {
                nonce: key(fields, "nonce")?,
            },
            "controller-retained-origin" => Action::Origin {
                attachment: key(fields, "attachment")?,
            },
            "controller-retained-initial-activation" => {
                Action::InitialActivation(frontier(fields, false)?)
            }
            "controller-retained-activation" => Action::Activation(frontier(fields, true)?),
            "controller-retained-activation-commit" => Action::Commit(frontier(fields, true)?),
            "controller-retained-app-promote" => Action::Promote(frontier(fields, true)?),
            "controller-retained-restoration-current" => Action::Restoration {
                service: key(fields, "service")?,
                go: key(fields, "go")?,
                old_attachment: key(fields, "old_attachment")?,
                new_attachment: key(fields, "new_attachment")?,
                frontier: key(fields, "frontier")?,
            },
            "controller-retained-app-current" => Action::AppCurrent {
                app_owner: key(fields, "app_owner")?,
            },
            "controller-source-selection-register" => Action::Register {
                service: key(fields, "service")?,
                source: key(fields, "source_operation")?,
                attachment: key(fields, "attachment")?,
            },
            "controller-source-selection-inhibit" => Action::Inhibit(selection(fields)?),
            "controller-source-selection-retire" => Action::Retire(selection(fields)?),
            "controller-source-selection-cancel-unaccepted" => Action::CancelUnaccepted {
                service: key(fields, "service")?,
                source: key(fields, "source_operation")?,
                attachment: key(fields, "attachment")?,
            },
            "controller-source-selection-begin" => Action::SelectionBegin {
                selection: selection(fields)?,
                go: optional_key(fields, "go")?,
            },
            "controller-source-selection-ui-ack" => {
                Action::SelectionUiAck(selection_frontier(fields)?)
            }
            "controller-source-selection-observe" => {
                Action::SelectionObserve(selection_frontier(fields)?)
            }
            "controller-source-selection-commit" => {
                Action::SelectionCommit(selection_frontier(fields)?)
            }
            "controller-source-selection-promote" => {
                Action::SelectionPromote(selection_frontier(fields)?)
            }
            _ => return None,
        };
        Some(Self {
            controller_operation,
            action,
        })
    }
    pub(crate) fn name(&self) -> &'static str {
        match &self.action {
            Action::Prepare { .. } => "controller-retained-prepare",
            Action::Enter { .. } => "controller-retained-enter",
            Action::InitialBegin { .. } => "controller-retained-initial-begin",
            Action::ReplacementBegin { .. } => "controller-retained-frontier-begin",
            Action::UiAck(frontier) if frontier.go.is_none() => {
                "controller-retained-initial-ui-ack"
            }
            Action::UiAck(_) => "controller-retained-ui-ack",
            Action::FrontierAck { .. } => "controller-retained-frontier-ack",
            Action::Origin { .. } => "controller-retained-origin",
            Action::InitialActivation(_) => "controller-retained-initial-activation",
            Action::Activation(_) => "controller-retained-activation",
            Action::Commit(_) => "controller-retained-activation-commit",
            Action::Promote(_) => "controller-retained-app-promote",
            Action::AppCurrent { .. } => "controller-retained-app-current",
            Action::Restoration { .. } => "controller-retained-restoration-current",
            Action::Register { .. } => "controller-source-selection-register",
            Action::Inhibit(_) => "controller-source-selection-inhibit",
            Action::Retire(_) => "controller-source-selection-retire",
            Action::CancelUnaccepted { .. } => "controller-source-selection-cancel-unaccepted",
            Action::SelectionBegin { .. } => "controller-source-selection-begin",
            Action::SelectionUiAck(_) => "controller-source-selection-ui-ack",
            Action::SelectionObserve(_) => "controller-source-selection-observe",
            Action::SelectionCommit(_) => "controller-source-selection-commit",
            Action::SelectionPromote(_) => "controller-source-selection-promote",
        }
    }
}

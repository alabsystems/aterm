// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source selection and `security-posture` control verb for the Security page's macOS access
//! posture: its MACOS ACCESS block, DEVELOPER TOOLS card and debugger row.

use crate::native_settings::MacosAccess;

/// Whether this build stages postures ([`cmd`]): a development build only.
const INJECTION_ENABLED: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The usage line every junk form answers with.
const USAGE: &str = "usage: security-posture [list|off|<name>]";

/// The posture the Security page draws its MACOS ACCESS block, DEVELOPER TOOLS card and debugger
/// row from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SecurityPostureSource {
    /// The windowed macOS host's probes ([`crate::App::publish_security_page`]).
    Host,
    /// Nothing published: a headless instance, a build off macOS.
    Absent,
    /// A named fixture a development build staged on a headless instance.
    Staged {
        name: String,
        access: Box<MacosAccess>,
    },
}

impl SecurityPostureSource {
    /// Returns `Host` on a windowed macOS instance and `Absent` everywhere else.
    pub(crate) const fn at_launch(macos: bool, headless: bool) -> Self {
        if macos && !headless {
            Self::Host
        } else {
            Self::Absent
        }
    }

    /// The word `security-posture` answers with.
    pub(crate) fn word(&self) -> &str {
        match self {
            Self::Host => "host",
            Self::Absent => "off",
            Self::Staged { name, .. } => name,
        }
    }
}

/// Action requested by the `security-posture` control verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Read,
    List,
    Stage(Option<String>),
}

/// The verb's arguments, refused where this build does not stage. Pure.
pub(crate) fn parse(rest: &str, injection: bool) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let asked = match words.as_slice() {
        [] => return Ok(Asked::Read),
        ["list"] => return Ok(Asked::List),
        ["off"] => Asked::Stage(None),
        [name] => Asked::Stage(Some((*name).to_string())),
        _ => return Err(USAGE.to_string()),
    };
    if !injection {
        return Err(
            "security-posture <name>|off needs a development build (dev-seams)".to_string(),
        );
    }
    if let Asked::Stage(Some(name)) = &asked
        && !catalog_holds(name)
    {
        return Err(format!(
            "{USAGE} (no posture `{name}`; `security-posture list` names them)"
        ));
    }
    Ok(asked)
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
fn catalog_holds(name: &str) -> bool {
    crate::security_fixtures::NAMES.contains(&name)
}

#[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
fn catalog_holds(_name: &str) -> bool {
    false
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
fn catalog_line() -> String {
    crate::security_fixtures::NAMES.join(",")
}

#[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
fn catalog_line() -> String {
    String::new()
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
fn staged(name: &str) -> Option<MacosAccess> {
    crate::security_fixtures::named(name)
}

#[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
fn staged(_name: &str) -> Option<MacosAccess> {
    None
}

/// Handles `security-posture` on the control thread, forwarding reads and stages to the UI thread.
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let asked = match parse(rest, INJECTION_ENABLED) {
        Ok(Asked::List) => return format!("OK postures={}\n", catalog_line()),
        Ok(asked) => asked,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| {
        crate::Wake::ControlSecurityPosture { asked, reply }
    }) {
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

impl crate::App {
    /// Applies or reads the security posture on the main thread (staging is headless-only).
    pub(crate) fn control_security_posture(&mut self, asked: &Asked) -> Result<String, String> {
        if let Asked::Stage(name) = asked {
            if !self.headless {
                return Err(
                    "security-posture stages a posture on a headless instance only".to_string(),
                );
            }
            self.security_posture = match name {
                None => {
                    self.clear_macos_access();
                    SecurityPostureSource::Absent
                }
                Some(name) => SecurityPostureSource::Staged {
                    name: name.clone(),
                    access: Box::new(staged(name).ok_or_else(|| format!("no posture `{name}`"))?),
                },
            };
            self.sync_settings_consent_posture();
        }
        Ok(format!(
            "posture={} views={}",
            self.security_posture.word(),
            self.settings_views_drawing_macos_access()
        ))
    }
}

#[cfg(test)]
#[path = "security_posture_tests.rs"]
mod tests;
